import { useRef, useState } from "react";
import { Loader2, RefreshCw } from "lucide-react";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { asError } from "@/lib/api";

/** 单个账号续期的返回（三个产品的字段略有差异，只认这几个共同项）。 */
interface RenewResult {
  refreshed?: boolean;
  message?: string;
  remainingDays?: number;
}

export interface RenewAllTokensProps {
  /** 产品名，只用于文案。 */
  product: string;
  /** 当前页的账号 id 列表（点按钮时才读：列表可能刚被刷新过）。 */
  ids: string[];
  /** 单个账号续期。抛错 = 这个账号失败（其余继续）。 */
  renew: (id: string) => Promise<RenewResult | unknown>;
  /** 全部跑完后调用（刷新列表 / 重算积分）。 */
  onFinished?: () => void;
  disabled?: boolean;
}

/**
 * 「一键续期」：把**当前页**所有账号的 token 逐个续期。
 *
 * 三个产品各自有单账号续期命令，这里只是把它们串起来，不新增后端批量命令 ——
 * 逐账号调用能精确知道「哪个成功、哪个失败」，一次性批量命令反而会把失败原因压成一句。
 *
 * 三条硬规矩：
 * 1. **必须二次确认**：一次点下去会向上游发 N 次换 token 请求，误点代价是全部账号的
 *    登录态被刷新一遍。所以按钮只负责开确认框，真正执行在确认之后。
 * 2. **串行**：并发换 token 容易被上游频控（尤其 Trae），而且失败时全是 429 分不清原因。
 * 3. **失败不清 token**：某个账号失败只记录原因，绝不把它的旧 token 置空 ——
 *    宁可它还是旧的，也不能让用户账号直接掉线。
 *
 * ⚠️ 只作用于传入的 `ids`：首页不提供这个按钮，各管理页各管各的账号。
 */
export function RenewAllTokensButton({
  product,
  ids,
  renew,
  onFinished,
  disabled,
}: RenewAllTokensProps) {
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState(0);
  const [total, setTotal] = useState(0);
  /** 本次运行的失败清单（只在 dialog 里显示，不进全局状态）。 */
  const failures = useRef<{ id: string; error: string }[]>([]);

  async function run() {
    const list = [...ids];
    if (list.length === 0) return;
    setBusy(true);
    setDone(0);
    setTotal(list.length);
    failures.current = [];
    let ok = 0;
    let skipped = 0;

    for (const id of list) {
      try {
        const r = (await renew(id)) as RenewResult | null;
        // `refreshed === false` 是「上游认为还新鲜，没必要换」，不是失败。
        if (r && typeof r === "object" && r.refreshed === false) skipped += 1;
        else ok += 1;
      } catch (e) {
        failures.current.push({ id, error: asError(e).split("\n")[0] });
      }
      setDone((n) => n + 1);
    }

    setBusy(false);
    setOpen(false);
    onFinished?.();

    const failed = failures.current.length;
    const head = `${product} 续期完成：成功 ${ok} 个${
      skipped ? ` · 跳过 ${skipped} 个` : ""
    }${failed ? ` · 失败 ${failed} 个` : ""}`;
    const detail = failed
      ? `失败原因（首个）：${failures.current[0].error}\n失败账号的现有 token 不会被清除。`
      : "列表已按新的有效期刷新。";
    if (failed) toast.warning(head, { description: detail });
    else toast.success(head, { description: detail });
  }

  return (
    <>
      <Button
        variant="outline"
        size="sm"
        disabled={disabled || busy || ids.length === 0}
        onClick={() => setOpen(true)}
        title={`把当前页这 ${ids.length} 个账号的 token 逐个续期（会先弹确认）`}
        data-action="renew-all-tokens"
      >
        {busy ? <Loader2 className="size-4 animate-spin" /> : <RefreshCw className="size-4" />}
        一键续期
        <span className="ml-1 tabular-nums opacity-70">{ids.length}</span>
      </Button>

      <Dialog open={open} onOpenChange={(v) => (busy ? null : setOpen(v))}>
        <DialogContent
          className="max-w-md"
          // 执行中不允许点遮罩关掉：关掉会让人以为可以中途取消，实际请求还在跑。
          onInteractOutside={(e) => busy && e.preventDefault()}
        >
          <DialogHeader>
            <DialogTitle>确认续期 {total || ids.length} 个账号？</DialogTitle>
            <DialogDescription>
              将对本页 {ids.length} 个 {product} 账号逐个换取新的 token，并写回本地账号库。
              <br />
              过程约需十几秒（逐个请求，避免被上游频控）；失败账号的现有 token 不会被清除。
            </DialogDescription>
          </DialogHeader>

          {busy ? (
            <div className="rounded-lg border bg-muted/40 px-3 py-2 text-sm">
              正在续期：{done} / {total}
              {failures.current.length > 0 ? ` · 已失败 ${failures.current.length}` : ""}
              <div className="mt-1 text-xs text-muted-foreground">
                请不要关闭程序；中途关掉已完成的账号不会回退。
              </div>
            </div>
          ) : null}

          <DialogFooter>
            <Button variant="outline" disabled={busy} onClick={() => setOpen(false)}>
              取消
            </Button>
            <Button onClick={() => void run()} disabled={busy || ids.length === 0}>
              {busy ? <Loader2 className="size-4 animate-spin" /> : <RefreshCw className="size-4" />}
              确认续期
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
