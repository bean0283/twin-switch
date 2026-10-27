import { useCallback, useEffect, useState } from "react";
import { Link } from "react-router-dom";
import {
  ArrowLeft,
  CheckCircle2,
  Copy,
  KeyRound,
  Loader2,
  MessageSquare,
  RefreshCw,
  Send,
  XCircle,
} from "lucide-react";
import { toast } from "sonner";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { gatewayGuideSelftest } from "@/lib/api";
import { copyText } from "@/lib/clipboard";
import { cn } from "@/lib/utils";

type Step = { id: string; label: string; ok: boolean; detail: string; sample?: string[] };
type Test = {
  ok: boolean;
  steps: Step[];
  models: { count: number; sample: string[] };
  chat: { ok: boolean; ms?: number; status?: number; content?: string; error?: string } | null;
  checkedAt: number;
};

const ENDPOINT_FULL = "http://127.0.0.1:57892/v1/chat/completions";
const ENDPOINT_BASE = "http://127.0.0.1:57892/v1";

/** 三步里出现的可复制串：点一下进剪贴板。 */
function CopyRow({ text, note }: { text: string; note?: string }) {
  return (
    <div className="flex items-center gap-2 rounded-lg border border-border bg-muted/30 px-2.5 py-1.5">
      <code className="min-w-0 flex-1 truncate font-mono text-[11px]">{text}</code>
      <Button
        size="sm"
        variant="ghost"
        className="h-6 shrink-0 gap-1 px-1.5 text-[11px]"
        onClick={async () => {
          if (await copyText(text)) toast.success("已复制", { description: text });
          else toast.error("复制失败", { description: "请手动选中后按 Ctrl+C：" + text });
        }}
      >
        <Copy className="size-3" />
        复制
      </Button>
      {note ? <span className="shrink-0 text-[10px] text-muted-foreground">{note}</span> : null}
    </div>
  );
}

/** 步骤序号圆标（示意图上的「标注」）。 */
function StepNo({ n }: { n: number }) {
  return (
    <span className="grid size-5 shrink-0 place-items-center rounded-full bg-primary text-[11px] font-semibold text-primary-foreground">
      {n}
    </span>
  );
}

export default function GatewayGuidePage() {
  const [test, setTest] = useState<Test | null>(null);
  const [busy, setBusy] = useState(false);
  const [key, setKey] = useState("");
  const [model, setModel] = useState("");
  const [chatBusy, setChatBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const run = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      const r = await gatewayGuideSelftest({});
      setTest(r as Test);
      setModel((prev) => prev || (r.models?.sample?.[0] ?? ""));
      if (r.error) setError(r.error);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, []);

  // 打开页面就自动跑一次真机自检（不消耗额度：第 4 步需要 Key 才发请求）
  useEffect(() => {
    void run();
  }, [run]);

  async function runChat() {
    if (!key.trim()) {
      toast.error("先填一枚网关 Key", { description: "在「积分共享」页创建，形如 sk-ts-…" });
      return;
    }
    setChatBusy(true);
    try {
      const r = await gatewayGuideSelftest({ key: key.trim(), model: model.trim() || undefined });
      setTest(r as Test);
      const chat = r.chat;
      if (chat?.ok) {
        toast.success("真实对话已跑通", { description: `${chat.ms ?? 0} ms · 回复：${chat.content ?? ""}` });
      } else {
        toast.error("对话失败", { description: chat?.error?.slice(0, 120) ?? "未知原因" });
      }
    } catch (e) {
      toast.error("请求失败", { description: String(e).slice(0, 120) });
    } finally {
      setChatBusy(false);
    }
  }

  return (
    <div className="mx-auto flex min-h-full w-full max-w-4xl flex-col gap-5 px-4 py-6">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h1 className="text-xl font-semibold tracking-tight">积分共享 · 使用说明</h1>
          <p className="mt-1 text-sm text-muted-foreground">
            把三个产品的积分合成一个池子，用 OpenAI 兼容接口对外提供。
            下面每一步都能在本机现场验证，不需要你凭感觉猜。
          </p>
        </div>
        <Button variant="outline" size="sm" asChild className="shrink-0 gap-1.5">
          <Link to="/gateway">
            <ArrowLeft className="size-4" />
            回积分共享
          </Link>
        </Button>
      </div>

      {/* 现场自检 */}
      <Card>
        <CardHeader className="pb-2">
          <div className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-2">
            <div className="min-w-0">
              <CardTitle className="text-base">现场自检（打开本页自动跑）</CardTitle>
              <CardDescription>
                {test
                  ? `刚刚检测：${new Date(test.checkedAt).toLocaleTimeString("zh-CN", { hour12: false })} · 模型 ${
                      test.models?.count ?? 0
                    } 个`
                  : "正在检测…"}
              </CardDescription>
            </div>
            <Button variant="outline" size="sm" disabled={busy} onClick={() => void run()}>
              {busy ? <Loader2 className="size-4 animate-spin" /> : <RefreshCw className="size-4" />}
              重新检测
            </Button>
          </div>
        </CardHeader>
        <CardContent className="space-y-2">
          {error ? (
            <div className="rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-xs text-destructive break-all">
              {error}
            </div>
          ) : null}
          {!test ? (
            <Skeleton className="h-28 rounded-xl" />
          ) : (
            test.steps.map((s) => (
              <div key={s.id} className="flex items-start gap-2 rounded-lg border px-3 py-2 text-xs">
                {s.ok ? (
                  <CheckCircle2 className="mt-0.5 size-4 shrink-0 text-emerald-600" />
                ) : (
                  <XCircle className="mt-0.5 size-4 shrink-0 text-destructive" />
                )}
                <span className="min-w-0 flex-1">
                  <span className="font-medium">{s.label}</span>
                  <span className="ml-2 break-all text-muted-foreground">{s.detail}</span>
                </span>
              </div>
            ))
          )}
          {test?.models?.sample?.length ? (
            <div className="flex flex-wrap items-center gap-1.5 pt-1 text-[11px] text-muted-foreground">
              可用模型示例：
              {test.models.sample.slice(0, 8).map((m) => (
                <Badge key={m} variant="secondary" className="font-mono text-[10px]">
                  {m}
                </Badge>
              ))}
            </div>
          ) : null}
        </CardContent>
      </Card>

      {/* 三步接入 */}
      <Card>
        <CardHeader className="pb-2">
          <CardTitle className="text-base">三步接入（以 WorkBuddy 为例）</CardTitle>
          <CardDescription>
            地址和 Key 是同一套，换客户端只需改「填到哪一层」和「模型名」。
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-3">
          {/* ① */}
          <div className="rounded-lg border p-3">
            <div className="mb-2 flex items-center gap-2 text-sm font-medium">
              <StepNo n={1} />
              <KeyRound className="size-4 text-muted-foreground" />
              先创建一个网关 Key
            </div>
            <p className="text-xs text-muted-foreground">
              在本页上方「积分共享 → 网关 Key」里点「创建 Key」。明文只显示一次，复制保存好；
              库里只存哈希与前后 4 位。
            </p>
            <div className="mt-2 rounded-md border border-dashed border-primary/40 bg-primary/5 px-3 py-1.5 text-[11px] text-primary">
              标注：Key 形如 <code className="font-mono">sk-ts-xxxxxxxx</code>，填到客户端的 API Key 栏
            </div>
          </div>

          {/* ② */}
          <div className="rounded-lg border p-3">
            <div className="mb-2 flex items-center gap-2 text-sm font-medium">
              <StepNo n={2} />
              填接口地址（两种写法，别填错层级）
            </div>
            <div className="space-y-2">
              <div>
                <div className="mb-1 text-[11px] font-medium text-foreground">
                  WorkBuddy：填**完整端点**
                </div>
                <CopyRow text={ENDPOINT_FULL} note="它把地址原样 POST" />
              </div>
              <div>
                <div className="mb-1 text-[11px] font-medium text-foreground">
                  Trae / Qoder / 其它 OpenAI 兼容客户端：填 **Base URL**
                </div>
                <CopyRow text={ENDPOINT_BASE} note="客户端自己拼 /chat/completions" />
              </div>
            </div>
            <div className="mt-2 rounded-md border border-dashed border-primary/40 bg-primary/5 px-3 py-1.5 text-[11px] text-primary">
              标注：WorkBuddy 填 <code className="font-mono">…/v1/chat/completions</code>；
              其它客户端填到 <code className="font-mono">…/v1</code>。填错的典型症状是 404。
            </div>
            <div className="mt-2 rounded-md border border-dashed border-border bg-muted/30 px-3 py-1.5 text-[11px] text-muted-foreground">
              在别的设备上用（飞牛 NAS / 另一台电脑）：先到「积分共享」页勾上
              「允许局域网内的其它设备使用」，再把上面的 <code className="font-mono">127.0.0.1</code>{" "}
              换成本机局域网 IP（形如{" "}
              <code className="font-mono">http://192.168.x.x:57892/v1</code>），端口与路径都不变。
              网关会强制校验 Key，局域网来源不带 Key 一律 401；容器里跑的应用不能用{" "}
              <code className="font-mono">127.0.0.1</code>。
            </div>
          </div>

          {/* ③ */}
          <div className="rounded-lg border p-3">
            <div className="mb-2 flex items-center gap-2 text-sm font-medium">
              <StepNo n={3} />
              <MessageSquare className="size-4 text-muted-foreground" />
              选模型名（从「模型管理」复制）
            </div>
            <p className="text-xs text-muted-foreground">
              回到「积分共享 → 模型管理」，按产品分块列出所有模型，点一下即复制。规则：
            </p>
            <ul className="mt-1 space-y-1 text-xs text-muted-foreground">
              <li>
                · 裸名（如 <code className="font-mono">glm-5.3</code>）：后端在提供它的产品之间选号，
                <span className="text-foreground">积分到期最早的先用</span>
              </li>
              <li>
                · 想锁死某家：加前缀 <code className="font-mono">wb:glm-5.3</code> /{" "}
                <code className="font-mono">trae:glm-5.3</code> /{" "}
                <code className="font-mono">qoder:GLM-5.3</code>（国际版用 <code className="font-mono">-g</code>）
              </li>
              <li>
                · Qoder 只露客户端里的名字（<code className="font-mono">GLM-5.3</code>），
                <code className="font-mono">gmodel</code> 这类代号由网关自己换，你不用管
              </li>
              <li>· 别手打，直接复制（大小写、连字符都可能和上游不一致）</li>
            </ul>
            {test?.models?.sample?.length ? (
              <div className="mt-2 flex flex-wrap items-center gap-1.5">
                <span className="text-[11px] text-muted-foreground">现场可用的：</span>
                {test.models.sample.slice(0, 6).map((m) => (
                  <button
                    key={m}
                    type="button"
                    onClick={async () => {
                      if (await copyText(m)) toast.success("已复制模型名", { description: m });
                      else toast.error("复制失败", { description: "请手动复制：" + m });
                    }}
                    className="rounded border px-1.5 py-0.5 font-mono text-[11px] hover:bg-accent"
                  >
                    {m}
                  </button>
                ))}
              </div>
            ) : null}
          </div>
        </CardContent>
      </Card>

      {/* 真发一次 */}
      <Card>
        <CardHeader className="pb-2">
          <CardTitle className="text-base">真发一次对话（可选）</CardTitle>
          <CardDescription>
            填一枚 Key 点发送：会**真的**走一遍网关（消耗极少量额度），成功即说明整条链路通了。
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-2">
          <div className="grid gap-2 sm:grid-cols-[minmax(0,1fr)_minmax(0,180px)_auto]">
            <Input
              value={key}
              onChange={(e) => setKey(e.target.value)}
              placeholder="sk-ts-…"
              className="h-8 font-mono text-xs"
            />
            <Input
              value={model}
              onChange={(e) => setModel(e.target.value)}
              placeholder="模型名（默认取第一个）"
              className="h-8 font-mono text-xs"
            />
            <Button size="sm" disabled={chatBusy} onClick={() => void runChat()} className="gap-1.5">
              {chatBusy ? <Loader2 className="size-3.5 animate-spin" /> : <Send className="size-3.5" />}
              发送
            </Button>
          </div>
          {test?.chat ? (
            <div
              className={cn(
                "rounded-lg border px-3 py-2 text-xs",
                test.chat.ok
                  ? "border-emerald-500/30 bg-emerald-500/5"
                  : "border-destructive/30 bg-destructive/5 text-destructive",
              )}
            >
              {test.chat.ok ? (
                <>
                  <span className="font-medium">跑通了</span>（{test.chat.ms ?? 0} ms，模型{" "}
                  <code className="font-mono">{model || "?"}</code>）
                  <div className="mt-1 break-all text-muted-foreground">
                    回复：{test.chat.content || "（空）"}
                  </div>
                </>
              ) : (
                <div className="break-all">
                  <span className="font-medium">
                    失败{test.chat.status ? `（HTTP ${test.chat.status}）` : ""}
                  </span>
                  ：{test.chat.error}
                </div>
              )}
            </div>
          ) : null}
        </CardContent>
      </Card>

      {/* FAQ */}
      <Card>
        <CardHeader className="pb-2">
          <CardTitle className="text-base">常见问题</CardTitle>
        </CardHeader>
        <CardContent className="space-y-2 text-xs text-muted-foreground">
          <p>
            <span className="font-medium text-foreground">404 或「接口不存在」</span>
            ：地址层级填错。WorkBuddy 要完整端点，其它客户端填到 <code className="font-mono">/v1</code>。
          </p>
          <p>
            <span className="font-medium text-foreground">401</span>
            ：Key 不对，或账号凭据已被服务端作废（账号在别处重新登录过）。前者重建 Key，后者到账号页重新登录。
          </p>
          <p>
            <span className="font-medium text-foreground">响应很慢 / 超时</span>
            ：热门免费模型可能在上游排队（如 <code className="font-mono">DeepSeek-Flash</code>），
            换 <code className="font-mono">Qwen3.8-Max</code> / <code className="font-mono">GLM-5.3</code> 之类。
            <span className="block pt-1">
              想确认「慢在哪一步」看 <code className="font-mono">~/.twin-switch/gateway-metrics.jsonl</code>
              ：<code className="font-mono">ttft_ms</code> 是首字延迟（远小于{" "}
              <code className="font-mono">total_ms</code> 说明流式生效了）、
              <code className="font-mono">queued</code> 是撞上排队、
              <code className="font-mono">failovers</code> 是换了几个号（&gt;0 会把总时长放大）。
            </span>
          </p>
          <p>
            <span className="font-medium text-foreground">流式出问题时的一键退路</span>
            ：启动前设 <code className="font-mono">TWIN_TS_NO_STREAM=1</code> 退回「聚合后一次性返回」，
            设 <code className="font-mono">TWIN_TS_NO_AFFINITY=1</code> 让会话 ID 每次新建（不复用）。
          </p>
          <p>
            <span className="font-medium text-foreground">模型名报「未知作用域」</span>
            ：前缀写错了。可用：<code className="font-mono">trae</code> /{" "}
            <code className="font-mono">trae-g</code> / <code className="font-mono">wb</code> /{" "}
            <code className="font-mono">wb-g</code> / <code className="font-mono">qoder</code> /{" "}
            <code className="font-mono">qoder-g</code>。
          </p>
        </CardContent>
      </Card>
    </div>
  );
}
