#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""真机验证「切换账号时同步 WorkBuddy 定时任务」的辅助脚本（只读，除 backup 外不改任何文件）。

用法（Windows / Git Bash 均可，只需 python 3.8+，无第三方依赖）:

    python tools/verify-automation-sync.py snap      # 切号前：备份库 + 存快照
    python tools/verify-automation-sync.py diff      # 切号后：与快照对比，看有没有真的写进去
    python tools/verify-automation-sync.py report    # 看最后一次同步的现场（automation-sync.json）+ 诊断日志
    python tools/verify-automation-sync.py all       # 一次跑完 diff + report

判定口径
    diff 里「目标账号 +N 条」= 后端真的写进去了；
    目标账号 0 变化但 report 显示 copied 有值 = 被客户端/云端覆盖（本次要抓的那个风险）；
    两者都 0 且 report 显示 notNeeded = 压根没走同步（看 reason 是哪一种）。
"""

from __future__ import annotations

import json
import os
import sqlite3
import sys
import time
from pathlib import Path

# Windows 控制台默认 GBK，中文/箭头会炸，强制 UTF-8
try:
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
except Exception:
    pass


def home() -> Path:
    return Path(os.path.expanduser("~"))


def store_dir() -> Path:
    """与 Rust 侧 config::store_dir() 对齐：支持 TWIN_SWITCH_HOME 覆盖。"""
    env = os.environ.get("TWIN_SWITCH_HOME")
    if env:
        return Path(env)
    return home() / ".twin-switch"


def db_path() -> Path:
    return home() / ".workbuddy" / "workbuddy.db"


def snap_path() -> Path:
    return store_dir() / "automation-sync-snap.json"


def ts(ms) -> str:
    if not ms:
        return "-"
    try:
        v = float(ms)
        return time.strftime("%m-%d %H:%M:%S", time.localtime(v / 1000 if v > 1e11 else v))
    except Exception:
        return str(ms)


def connect_ro() -> sqlite3.Connection:
    p = db_path()
    if not p.exists():
        sys.exit(f"找不到 {p} —— 这台机器没装 WorkBuddy 或路径不对")
    # mode=ro：绝不写库。WAL 存在时也能读到已提交数据。
    con = sqlite3.connect(f"file:{p.as_posix()}?mode=ro", uri=True)
    con.row_factory = sqlite3.Row
    return con


# ---------------------------------------------------------------------------
# 备份：用 sqlite 官方备份 API（客户端在跑、有 WAL 也安全），别用文件拷贝
# ---------------------------------------------------------------------------
def cmd_backup() -> Path:
    src = db_path()
    dst = src.with_name(f"workbuddy.db.bak-verify-{time.strftime('%Y%m%d-%H%M%S')}")
    with sqlite3.connect(src) as s, sqlite3.connect(dst) as d:
        s.backup(d)
    print(f"已备份：{dst}  ({dst.stat().st_size / 1024 / 1024:.1f} MB)")
    return dst


# ---------------------------------------------------------------------------
# 快照 / 对比
# ---------------------------------------------------------------------------
def collect(con: sqlite3.Connection) -> dict:
    cur = con.cursor()
    rows = []
    for r in cur.execute(
        """
        select a.id, a.name, a.owner_user_id, a.status, a.deleted_at,
               a.created_at, a.rrule, a.model_id,
               (select count(*) from automation_runs x where x.automation_id = a.id),
               (select count(*) from automation_runtime_state x where x.automation_id = a.id),
               a.updated_at
          from automations a
         order by a.created_at
        """
    ):
        rows.append(
            {
                "id": r[0],
                "name": r[1],
                "owner": r[2],
                "status": r[3],
                "deleted": bool(r[4]),
                "created": r[5],
                "rrule": r[6],
                "model": r[7],
                "runs": r[8],
                "state": r[9],
                # 覆盖模式**不新增行**，只改已有行 —— 没有这一列就完全看不出它生效了。
                "updated": r[10],
            }
        )
    return {"takenAt": int(time.time() * 1000), "rows": rows}


def counts(rows: list[dict]) -> dict:
    out: dict = {}
    for r in rows:
        if r["deleted"]:
            continue
        out[r["owner"] or "-"] = out.get(r["owner"] or "-", 0) + 1
    return out


def short(uid) -> str:
    return (uid or "-")[:8]


def cmd_snap() -> None:
    cmd_backup()
    con = connect_ro()
    data = collect(con)
    con.close()
    snap_path().parent.mkdir(parents=True, exist_ok=True)
    snap_path().write_text(json.dumps(data, ensure_ascii=False, indent=1), encoding="utf-8")
    print(f"\n快照已存：{snap_path()}")
    print(f"共 {len(data['rows'])} 条任务，按账号：")
    for uid, n in sorted(counts(data["rows"]).items(), key=lambda kv: -kv[1]):
        print(f"  {short(uid):10} {n} 条")
    print("\n下一步：去 TwinSwitch 里切一次号，回来跑 `diff`。")


def cmd_diff() -> None:
    if not snap_path().exists():
        sys.exit("还没有快照 —— 先跑 `snap`（切号之前）")
    before = json.loads(snap_path().read_text(encoding="utf-8"))
    con = connect_ro()
    after = collect(con)
    con.close()

    b_by_id = {r["id"]: r for r in before["rows"]}
    a_by_id = {r["id"]: r for r in after["rows"]}
    new_ids = [i for i in a_by_id if i not in b_by_id]
    gone_ids = [i for i in b_by_id if i not in a_by_id]

    cb, ca = counts(before["rows"]), counts(after["rows"])
    print(f"快照时间：{ts(before['takenAt'])}    现在：{ts(after['takenAt'])}")
    print("\n== 各账号任务数（未删除）==")
    print(f"{'owner':10} {'前':>4} {'后':>4} {'变化':>6}")
    for uid in sorted(set(cb) | set(ca)):
        b, a = cb.get(uid, 0), ca.get(uid, 0)
        flag = "" if a == b else ("  <== 增长" if a > b else "  <== 减少")
        print(f"{short(uid):10} {b:>4} {a:>4} {a - b:>+6}{flag}")

    print(f"\n== 新增 {len(new_ids)} 条 / 消失 {len(gone_ids)} 条 ==")
    for i in new_ids:
        r = a_by_id[i]
        print(f"  + {short(r['owner']):10} {ts(r['created'])}  runs={r['runs']:<3} state={r['state']:<2} {r['name'][:38]}")
    for i in gone_ids:
        r = b_by_id[i]
        print(f"  - {short(r['owner']):10} {ts(r['created'])}  {r['name'][:38]}")

    # 覆盖模式不新增行：靠 updated_at 变化识别"这条被改写过"。
    # ⚠️ 旧快照里没有 `updated` 字段：只有两边都拿得到时才比它，否则会把
    #    "快照版本不同"误报成"整库被改写"（第一版就踩了这个）。
    def _changed(before_row: dict, after_row: dict) -> bool:
        for key in ("rrule", "model", "status"):
            if before_row.get(key) != after_row.get(key):
                return True
        bu, au = before_row.get("updated"), after_row.get("updated")
        return bu is not None and au is not None and bu != au

    changed = [i for i in a_by_id if i in b_by_id and _changed(b_by_id[i], a_by_id[i])]
    print(f"\n== 内容被改写的任务 {len(changed)} 条（覆盖模式看这里）==")
    for i in changed:
        b, a = b_by_id[i], a_by_id[i]
        print(
            f"  ~ {short(a['owner']):10} {a['name'][:30]:32} "
            f"model {b['model']} → {a['model']} | status {b['status']} → {a['status']} | runs {b['runs']} → {a['runs']}"
        )

    # 同名同周期 = 同一个任务：覆盖模式会把多出来的行软删除合并。
    # 软删除的行仍在库里（可恢复），所以表现为 deleted false → true，而不是"消失"。
    merged = [
        i
        for i in a_by_id
        if i in b_by_id
        and not b_by_id[i].get("deleted")
        and a_by_id[i].get("deleted")
    ]
    print(f"\n== 被合并（软删除）的任务 {len(merged)} 条 ==")
    for i in merged:
        b = b_by_id[i]
        print(f"  m {short(b['owner']):10} {b['name'][:38]}  （同名同周期重复，已并入最早那条）")

    print("\n== 判定 ==")
    if new_ids:
        owners = {r["owner"] for r in after["rows"] if r["id"] in set(new_ids)}
        print(f"后端确实写入了 {len(new_ids)} 条（目标账号 {', '.join(short(o) for o in owners)}）→ 同步链路通。")
        print("接着跑 `report` 看 mode / copied 是否与这里数量一致。")
    elif changed or merged:
        print(f"没有新增，但 {len(changed)} 条已有任务被改写、{len(merged)} 条重复被合并 → **覆盖模式生效**。")
        print("跑 `report` 确认 mode=overwrite、overwritten 里有这些名字。")
    elif gone_ids:
        print("只有删除没有新增 —— 不是同步的动作，先确认是不是你自己删了任务。")
    else:
        print("一条都没变。跑 `report` 看 reason / mode：")
        print("  「设置里已关闭…」→ 开关没生效，检查 settings.json")
        print("  「没有切换前的账号或同一个账号」→ 切号前就没有登录态，或切的还是同一个账号")
        print("  mode=skip 且 reason 里出现「已存在跳过」→ 目标账号本来就有同款任务，属正常；")
        print("     想强制刷新就把设置里的「同款任务处理」改成「覆盖」再切一次。")


def cmd_report() -> None:
    # 1) 最近一次同步的现场
    rep = store_dir() / "workbuddy" / "automation-sync.json"
    print("== 最近一次同步（automation-sync.json）==")
    if rep.exists():
        d = json.loads(rep.read_text(encoding="utf-8"))
        for k in ("at", "mode", "enabled", "ok", "notNeeded", "reason", "error",
                  "fromUid", "toUid", "sourceCount", "extraDuplicates"):
            if k in d:
                print(f"  {k:12} = {d[k]}")
        for k in ("copied", "overwritten", "skippedNames"):
            if isinstance(d.get(k), list):
                names = d[k]
                print(f"  {k:12} = {len(names)} 个" + (f"：{', '.join(str(x)[:20] for x in names[:8])}" if names else ""))
    else:
        print(f"  还没有这个文件（{rep}）—— 说明本版本还一次都没走过同步分支。")

    # 2) 设置里的开关
    sj = store_dir() / "settings.json"
    print("\n== 设置开关 ==")
    if sj.exists():
        s = json.loads(sj.read_text(encoding="utf-8"))
        print(f"  syncWorkbuddyAutomations = {s.get('syncWorkbuddyAutomations')}   （期望 True）")
        print(f"  settings.json 修改时间   = {ts(sj.stat().st_mtime_ns // 1_000_000)}")
    else:
        print(f"  没有 {sj}")

    # 3) 诊断日志（默认只落 error；成功行要先把日志级别调成 info）
    logdir = store_dir() / "logs"
    print("\n== 诊断日志中的 wb.automation-sync ==")
    hits = 0
    if logdir.exists():
        for f in sorted(logdir.rglob("*.jsonl"), key=lambda p: p.stat().st_mtime, reverse=True)[:5]:
            for line in f.read_text(encoding="utf-8", errors="replace").splitlines():
                if "automation-sync" in line:
                    hits += 1
                    print(f"  [{f.name}] {line[:220]}")
                    if hits >= 8:
                        break
            if hits >= 8:
                break
    if hits == 0:
        print("  （无。成功行需要日志级别=info 才落盘；失败行是 error，默认就写）")


def main() -> None:
    cmd = (sys.argv[1] if len(sys.argv) > 1 else "all").lower()
    if cmd == "snap":
        cmd_snap()
    elif cmd == "diff":
        cmd_diff()
    elif cmd == "report":
        cmd_report()
    elif cmd == "backup":
        cmd_backup()
    elif cmd == "all":
        cmd_diff()
        print()
        cmd_report()
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
