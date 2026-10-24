#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""TwinSwitch 网关 · 工具调用（function calling）验收台。

为什么需要它：带 tools 的请求失败有三种完全不同的原因，混在一起看就会误判——
  · 网关把工具调用丢了 / 拼坏了  ⇒ 我们的 bug，必须修；
  · 模型自己不肯调用工具        ⇒ 换模型，不是 bug；
  · 上游/账号不可用             ⇒ 去「积分共享」页看账号。
本脚本对每个用例都判定到这四类之一，而不是只报「失败」。

用法（网关必须在运行，默认 127.0.0.1:57892）：

  python tools/gateway-tool-test.py --key sk-xxxxxxxx
  python tools/gateway-tool-test.py --key sk-xxxxxxxx --model deepseek-v4.1-flash
  python tools/gateway-tool-test.py --key sk-xxxxxxxx --all
  python tools/gateway-tool-test.py --key sk-xxxxxxxx --model glm-5.2 --verbose

Key 在客户端「积分共享」页新建/复制（明文只在创建时显示一次，忘了就吊销重建）。
"""

import argparse
import json
import socket
import sys
import time
import urllib.error
import urllib.request

try:  # Windows 控制台默认 GBK，不切就可能在打印中文时炸
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
except Exception:
    pass

DEFAULT_BASE = "http://127.0.0.1:57892"

# 工具定义：参数刻意带一个必填 string + 一个可选 integer，
# 这样能同时检验「必填参数到了」「类型没被写成字符串」「可选参数没被硬塞」。
TOOLS = [
    {
        "type": "function",
        "function": {
            "name": "get_weather",
            "description": "查询指定城市未来几天的天气",
            "parameters": {
                "type": "object",
                "properties": {
                    "city": {"type": "string", "description": "城市名，用中文"},
                    "days": {"type": "integer", "description": "要查几天，1 到 7"},
                },
                "required": ["city"],
            },
        },
    }
]

ASK_TOOL = "杭州今天天气怎么样？请调用 get_weather 工具查询，不要自己编答案。"


def _opener():
    # ⚠️ 必须显式禁掉环境代理：本机 shell 常带 https_proxy，会把 127.0.0.1 也代理走，
    # 结果所有请求都变成「连不上」，看起来像网关挂了。
    return urllib.request.build_opener(urllib.request.ProxyHandler({}))


OPENER = _opener()


def http(method, url, key=None, payload=None, accept="application/json", timeout=180):
    """返回 (status, body_text, headers)。status==0 表示连不上（不是 HTTP 错误）。"""
    data = None
    if payload is not None:
        data = json.dumps(payload, ensure_ascii=False).encode("utf-8")
    req = urllib.request.Request(url, data=data, method=method)
    if data is not None:
        req.add_header("content-type", "application/json")
    if key:
        req.add_header("authorization", "Bearer " + key)
    req.add_header("accept", accept)
    t0 = time.time()
    try:
        r = OPENER.open(req, timeout=timeout)
        return r.status, r.read().decode("utf-8", "replace"), dict(r.headers), time.time() - t0
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace"), dict(e.headers or {}), time.time() - t0
    except Exception as e:  # socket.timeout / URLError / ConnectionReset …
        return 0, f"{type(e).__name__}: {e}", {}, time.time() - t0


def chat(base, key, payload, stream=False, timeout=180):
    return http(
        "POST",
        base + "/v1/chat/completions",
        key=key,
        payload=payload,
        accept="text/event-stream" if stream else "application/json",
        timeout=timeout,
    )


def err_text(status, body, headers):
    """把错误体压成一句话。网关错误是 OpenAI 风格 JSON，上游错误是纯文本。"""
    snippet = (body or "").strip()
    try:
        o = json.loads(snippet)
        msg = o.get("error", {}).get("message") if isinstance(o.get("error"), dict) else None
        if msg:
            snippet = msg
        elif isinstance(o.get("detail"), str):
            snippet = o["detail"]
    except Exception:
        pass
    head = f"HTTP {status}" if status else "连不上"
    return f"{head}｜{snippet[:400]}"


def parse_json(body):
    try:
        return json.loads(body)
    except Exception:
        return None


def find_residue(text):
    """正文里残留的「工具调用文本」——Qoder 私有协议会把调用写在正文里。"""
    if not text:
        return None
    for marker in ("<function_calls>", "<invoke", "function_call>"):
        if marker in text:
            return marker
    return None


# ---------------------------------------------------------------- 各用例


def case_models(base):
    st, body, _, dt = http("GET", base + "/v1/models", timeout=20)
    if st != 200:
        return False, f"读模型清单失败：{err_text(st, body, {})}", []
    o = parse_json(body) or {}
    ids = [m.get("id") for m in o.get("data", []) if m.get("id")]
    return True, f"{len(ids)} 个模型（{dt:.2f}s）", ids


def case_baseline(base, key, model):
    """T2 不带 tools：确认这条上游本身是通的。基线不过，后面全是噪声。"""
    st, body, _, dt = chat(
        base, key, {"model": model, "messages": [{"role": "user", "content": "回答两个字：你好"}]}
    )
    if st in (401, 403):
        return "BLOCKED", f"这组 Key 用不了：{err_text(st, body, {})}"
    if st != 200:
        return "UPSTREAM_ERR", f"不带 tools 就失败：{err_text(st, body, {})}"
    o = parse_json(body) or {}
    content = (o.get("choices") or [{}])[0].get("message", {}).get("content") or ""
    if not content.strip():
        return "GATEWAY_BUG", "200 但正文为空（不带 tools 时的空回答）"
    return "PASS", f"正文 {len(content)} 字（{dt:.2f}s）：{content.strip()[:40]}"


def case_empty_tools(base, key, model):
    """T3 tools: [] —— 有些客户端会把空数组一起发上来，不能因此 400/502。"""
    st, body, _, _ = chat(
        base,
        key,
        {
            "model": model,
            "messages": [{"role": "user", "content": "回答两个字：收到"}],
            "tools": [],
        },
    )
    if st != 200:
        return "GATEWAY_BUG", f"空 tools 数组被拒：{err_text(st, body, {})}"
    return "PASS", "空 tools 正常返回 200"


def case_tools(base, key, model):
    """T4 真 tools + 明确要求调用。这是核心用例。"""
    st, body, _, dt = chat(
        base,
        key,
        {
            "model": model,
            "messages": [{"role": "user", "content": ASK_TOOL}],
            "tools": TOOLS,
            "tool_choice": "auto",
        },
    )
    if st != 200:
        return "GATEWAY_BUG", f"带 tools 直接失败：{err_text(st, body, {})}", None

    o = parse_json(body)
    if o is None:
        return "GATEWAY_BUG", f"200 但返回体不是 JSON：{body[:200]}", None
    choice = (o.get("choices") or [{}])[0]
    msg = choice.get("message") or {}
    calls = msg.get("tool_calls") or []
    finish = choice.get("finish_reason")
    content = msg.get("content")
    problems = []

    if not calls:
        residue = find_residue(content if isinstance(content, str) else "")
        if residue:
            return (
                "GATEWAY_BUG",
                f"正文里残留 {residue} 文本、tool_calls 为空（应提取成标准 tool_calls）",
                None,
            )
        if finish == "tool_calls":
            return (
                "GATEWAY_BUG",
                "finish_reason=tool_calls 但 message 里没有 tool_calls（调用被丢了）",
                None,
            )
        return (
            "MODEL_NO_TOOL",
            f"模型没调用工具，只回了文本（finish={finish}）：{(content or '')[:60]!r}",
            None,
        )

    # ① 同一 index 不许裂成两条
    if len(calls) > 1:
        names = [((c.get("function") or {}).get("name")) for c in calls]
        dup = {n for n in names if n and names.count(n) > 1}
        if dup:
            problems.append(f"同名调用裂成 {len(calls)} 条（{sorted(dup)}）")

    for i, c in enumerate(calls):
        if not c.get("id"):
            problems.append(f"tool_calls[{i}] 没有 id（客户端无法回 tool 结果）")
        if c.get("type") != "function":
            problems.append(f"tool_calls[{i}].type={c.get('type')!r}，应为 'function'")
        if "function_call" in c:
            problems.append(f"tool_calls[{i}] 还带着 function_call 字段（未转成 function）")
        fn = c.get("function") or {}
        if not fn.get("name"):
            problems.append(f"tool_calls[{i}] 没有 function.name")
        args = fn.get("arguments")
        if isinstance(args, dict):
            problems.append(f"tool_calls[{i}].arguments 是对象，必须是 JSON 字符串")
            args_obj = args
        else:
            try:
                args_obj = json.loads(args or "")
            except Exception:
                problems.append(
                    f"tool_calls[{i}].arguments 不是合法 JSON（典型症状：分片拼接成两段）："
                    f"{(args or '')[:120]!r}"
                )
                args_obj = None
        if isinstance(args_obj, dict) and "city" not in args_obj:
            problems.append(f"tool_calls[{i}].arguments 缺必填参数 city：{args_obj}")
        if isinstance(args_obj, dict) and "days" in args_obj and not isinstance(
            args_obj["days"], int
        ):
            problems.append(f"tool_calls[{i}].arguments.days 类型不是 integer：{args_obj['days']!r}")

    if finish != "tool_calls":
        problems.append(f"finish_reason={finish!r}，有调用时必须是 'tool_calls'")
    if content == "":
        problems.append("有调用时 content 给了空串（部分客户端会当空回答），应为 null")

    detail = f"{len(calls)} 个调用，{dt:.2f}s，finish={finish}"
    if problems:
        return "GATEWAY_BUG", detail + "；" + "；".join(problems), calls
    return "PASS", detail + f"，arguments={calls[0]['function']['arguments'][:80]}", calls


def case_stream(base, key, model):
    """T5 流式：delta 里没 tool_calls，客户端就永远等不到工具调用。"""
    payload = {
        "model": model,
        "messages": [{"role": "user", "content": ASK_TOOL}],
        "tools": TOOLS,
        "stream": True,
    }
    st, body, headers, dt = chat(base, key, payload, stream=True)
    if st != 200:
        return "GATEWAY_BUG", f"stream=true 失败：{err_text(st, body, headers)}"

    # ⚠️ 先看它到底是不是流：request 里 stream:true 但回来普通 JSON 时，
    # 只会看到「没有 [DONE]」，看不出真正原因（是错误体？还是根本没按流式回？）。
    ctype = {str(k).lower(): v for k, v in headers.items()}.get("content-type", "")
    if "text/event-stream" not in ctype.lower():
        o = parse_json(body)
        if isinstance(o, dict) and o.get("error"):
            return "GATEWAY_BUG", f"stream=true 但回了错误 JSON：{err_text(st, body, headers)}"
        if isinstance(o, dict):
            return "GATEWAY_BUG", f"stream=true 但回的是普通 JSON（content-type={ctype}），客户端会解析失败"
        return "GATEWAY_BUG", f"stream=true 但回的既不是 SSE 也不是 JSON（content-type={ctype}）：{body[:160]}"

    saw_calls = 0
    saw_content = False
    finish = None
    done = False
    for line in body.splitlines():
        line = line.strip()
        if not line.startswith("data:"):
            continue
        data = line[5:].strip()
        if data == "[DONE]":
            done = True
            continue
        o = parse_json(data)
        if not isinstance(o, dict):
            continue
        for ch in o.get("choices") or []:
            if ch.get("finish_reason"):
                finish = ch["finish_reason"]
            delta = ch.get("delta") or {}
            if delta.get("tool_calls"):
                saw_calls += len(delta["tool_calls"])
            if isinstance(delta.get("content"), str) and delta["content"]:
                saw_content = True

    if not done:
        return "GATEWAY_BUG", "流没有以 data: [DONE] 收尾（客户端可能一直挂着）"
    if not saw_calls:
        if saw_content and finish != "tool_calls":
            return "MODEL_NO_TOOL", f"流里只有正文、没有 tool_calls（finish={finish}）"
        return "GATEWAY_BUG", f"流里没有任何 delta.tool_calls（finish={finish}）"
    if finish != "tool_calls":
        return "GATEWAY_BUG", f"delta 有 {saw_calls} 个调用，但结束 chunk 的 finish_reason={finish!r}"
    return "PASS", f"{saw_calls} 个 delta.tool_calls，finish=tool_calls，有 [DONE]（{dt:.2f}s）"


def case_multi_turn(base, key, model, calls):
    """T6 回填 tool 结果：只发出去不给回来，Agent 跑不起来。"""
    if not calls:
        return "SKIP", "没有可回填的 tool_calls（T4 未通过）"
    call = calls[0]
    cid = call.get("id") or "call_0"
    messages = [
        {"role": "user", "content": ASK_TOOL},
        {"role": "assistant", "content": None, "tool_calls": [call]},
        {"role": "tool", "tool_call_id": cid, "content": '{"temp_c":23,"cond":"晴"}'},
    ]
    st, body, headers, dt = chat(
        base, key, {"model": model, "messages": messages, "tools": TOOLS}
    )
    if st != 200:
        return "MULTI_TURN_FAIL", f"回填 tool 结果后失败：{err_text(st, body, headers)}"
    o = parse_json(body) or {}
    content = (o.get("choices") or [{}])[0].get("message", {}).get("content") or ""
    if not str(content).strip():
        return "MULTI_TURN_FAIL", "200 但正文为空（上游可能不认 role=tool）"
    ok = ("23" in str(content)) or ("晴" in str(content))
    return ("PASS" if ok else "SUSPECT"), f"{dt:.2f}s，正文：{str(content).strip()[:60]}"


def case_region_binding(base, key, model):
    """T8 前缀锁死：`作用域:模型` 必须只走那一家，不被「更紧迫」的号带走。"""
    if ":" not in model:
        return "SKIP", "裸名按紧迫度多候选选号；想验锁死就 --model trae-cn:xxx 再跑一次"
    st, body, _, _ = chat(
        base, key, {"model": model, "messages": [{"role": "user", "content": "回答：ok"}]}
    )
    if st != 200:
        return "GATEWAY_BUG", f"带作用域前缀 {model} 失败：{err_text(st, body, {})}"
    return "PASS", f"{model} 正常返回（前缀锁死生效）"


# ---------------------------------------------------------------- 报告


LABEL = {
    "PASS": "通过",
    "MODEL_NO_TOOL": "模型不肯调用（非网关问题）",
    "GATEWAY_BUG": "网关问题",
    "UPSTREAM_ERR": "上游/账号问题",
    "BLOCKED": "Key 用不了（先换 Key）",
    "MULTI_TURN_FAIL": "多轮回填失败（上游可能不支持 role=tool）",
    "SUSPECT": "存疑（回填了但回答没用上数据）",
    "SKIP": "跳过",
}


def run_model(base, key, model, verbose=False):
    print(f"\n{'=' * 74}")
    print(f"模型：{model}")
    print("=" * 74)
    rows = []

    def rep(case, verdict, note=""):
        rows.append((case, verdict, note))
        if verdict == "PASS":
            flag = "✓"
        elif verdict in ("SKIP", "MODEL_NO_TOOL"):
            flag = "·"
        else:
            flag = "✗"
        print(f"  {flag} {case:<16} {LABEL.get(verdict, verdict)}")
        if note:
            print(f"      {note}")

    verdict, note = case_baseline(base, key, model)
    rep("T2 基线(无tools)", verdict, note)
    if verdict != "PASS":
        print("  ⚠ 基线不通，工具用例没有意义，已跳过（结论：未验证，不是「没问题」）。")
        return rows

    verdict, note = case_empty_tools(base, key, model)
    rep("T3 空tools[]", verdict, note)

    verdict, note, calls = case_tools(base, key, model)
    rep("T4 真tools", verdict, note)

    verdict, note = case_stream(base, key, model)
    rep("T5 流式", verdict, note)

    verdict, note = case_multi_turn(base, key, model, calls)
    rep("T6 回填结果", verdict, note)

    verdict, note = case_region_binding(base, key, model)
    rep("T8 前缀锁死", verdict, note)

    return rows


def main():
    ap = argparse.ArgumentParser(description="TwinSwitch 网关工具调用验收台")
    ap.add_argument("--base", default=DEFAULT_BASE, help=f"网关地址，默认 {DEFAULT_BASE}")
    ap.add_argument("--key", required=True, help="网关 Key（sk- 开头，在「积分共享」页新建）")
    ap.add_argument("--model", default="", help="要测的模型名；省略则用 /v1/models 的第一个")
    ap.add_argument("--all", action="store_true", help="遍历 /v1/models 里的每个模型（会消耗多次额度）")
    ap.add_argument("--timeout", type=int, default=180, help="单请求超时秒数，默认 180")
    ap.add_argument("--verbose", action="store_true", help="打印原始返回（排障用）")
    a = ap.parse_args()

    ok, note, models = case_models(a.base)
    print(f"网关 {a.base}")
    print(f"  healthz：{note}" if not ok else f"  模型清单：{note}")
    if not ok:
        print("\n网关没有在跑。先在 TwinSwitch 的「积分共享」页点「启动网关」，再重跑本脚本。")
        return 2
    if not models:
        print("  模型清单是空的：先去「积分共享」页同步一下模型（或用网关的 discover 刷新）。")
        return 2

    targets = models if a.all else [a.model or models[0]]
    if a.model and a.model not in models and not a.all:
        print(f"  ⚠ {a.model} 不在模型清单里，仍然照你说的测。")

    all_rows = {}
    for m in targets:
        all_rows[m] = run_model(a.base, a.key, m, a.verbose)

    def verdict_of(rows, case):
        for c, v, _ in rows:
            if c == case:
                return v
        return ""

    print(f"\n{'=' * 74}")
    print("汇总")
    print("=" * 74)
    print(f"  {'模型':<26} {'T4 真tools':<22} {'T5 流式':<22} T6 回填")
    print("  " + "-" * 70)
    for m, rows in all_rows.items():
        cells = []
        for case in ("T4 真tools", "T5 流式", "T6 回填结果"):
            v = verdict_of(rows, case)
            cells.append(LABEL.get(v, "未验证") if v else "未验证")
        print(f"  {m[:25]:<26} {cells[0]:<20} {cells[1]:<20} {cells[2]}")

    # ⚠️ 「未验证」必须与「没问题」分开说：基线就不通时，一个字都不能算通过。
    verified = [m for m, rows in all_rows.items() if verdict_of(rows, "T4 真tools")]
    bugs = [m for m in verified if verdict_of(all_rows[m], "T4 真tools") == "GATEWAY_BUG"
            or verdict_of(all_rows[m], "T5 流式") == "GATEWAY_BUG"]
    blocked = [m for m, rows in all_rows.items() if verdict_of(rows, "T2 基线(无tools)") == "BLOCKED"]

    print()
    if not verified:
        if blocked:
            print(f"判定：未验证 —— {len(blocked)} 个模型都卡在 Key 上（401/403），换一把有效 Key 再跑。")
        else:
            print("判定：未验证 —— 基线就没通（账号/上游问题），工具用例一条都没跑到，不是「没问题」。")
        return 3
    if bugs:
        print(f"判定：网关侧有 {len(bugs)} 个模型不达标 —— {', '.join(bugs)}")
        print("下一步：把上面的 ✗ 行连同模型名发出来；日志在 %USERPROFILE%\\.twin-switch\\gateway-errors.jsonl")
        return 1
    rest = [m for m in all_rows if m not in verified]
    tail = f"（另有 {len(rest)} 个模型未验证）" if rest else ""
    print(f"判定：已验证的 {len(verified)} 个模型网关侧均无问题{tail}。")
    print("      若某个模型没调用工具，是模型自身能力/偏好问题 —— 换愿意用工具的模型即可。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
