import { useEffect, useMemo, useState } from "react";
import { ArrowDown, ArrowUp, GripVertical, Loader2 } from "lucide-react";
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
import { asError, gatewayPrioritySet } from "@/lib/api";

/** 弹窗只用到这几个字段（与 `PoolEntry` 兼容，不必整份传进来）。 */
export interface PriorityAccount {
  key: string;
  product: string;
  scope: string;
  label: string;
  remaining?: number | null;
}

const PRODUCT_NAME: Record<string, string> = {
  trae: "Trae",
  workbuddy: "WorkBuddy",
  qoder: "Qoder",
};

/**
 * 账号优先级排序弹窗。
 *
 * ## 为什么是弹窗，而不是在「账号池」表格里直接拖
 *
 * 账号池是**按产品分组、默认折叠**的（Trae / WorkBuddy / Qoder 三块），
 * 有两个问题：① 用户得先展开才看得到行，拖拽没有可见入口；
 * ② 折叠状态下看不到全局顺序，而优先级恰恰是**跨产品的一条队列**（通用模型按它派单）。
 * 所以在弹窗里把全部账号摊平成一列，顺序即派单顺序，一眼能看全。
 *
 * ## 为什么同时给拖拽和上下箭头
 *
 * 拖拽在长列表里更快，但**不可发现**（用户原话：「我没看到有优先级的拖动顺序」）；
 * 箭头按钮虽然慢，但一眼就知道能点。两个都留，成本只有几行。
 *
 * 保存时传的是**整份列表**：后端要拿它与当前账号池对账（账号被删了不该继续占名次）。
 */
export function PriorityOrderDialog({
  open,
  onOpenChange,
  accounts,
  order,
  onSaved,
}: {
  open: boolean;
  onOpenChange: (v: boolean) => void;
  accounts: PriorityAccount[];
  /** 已保存的顺序（可能含池子里已经没有的 key，这里会被过滤掉）。 */
  order: string[];
  onSaved?: () => void;
}) {
  const [list, setList] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [dragKey, setDragKey] = useState<string | null>(null);

  /** 打开时按「已存顺序 + 池子里没排过的」拼出一份初始列表。 */
  useEffect(() => {
    if (!open) return;
    const byKey = new Map(accounts.map((a) => [a.key, a]));
    const head = order.filter((k) => byKey.has(k));
    const tail = accounts.map((a) => a.key).filter((k) => !head.includes(k));
    setList([...head, ...tail]);
  }, [open, order, accounts]);

  const index = useMemo(() => new Map(accounts.map((a) => [a.key, a])), [accounts]);

  function move(from: number, to: number) {
    if (to < 0 || to >= list.length || from === to) return;
    setList((prev) => {
      const next = [...prev];
      const [item] = next.splice(from, 1);
      next.splice(to, 0, item);
      return next;
    });
  }

  async function save() {
    setBusy(true);
    try {
      const r = await gatewayPrioritySet(list);
      // 落盘的是后端规范化后的顺序；本地跟着它走，避免「显示顺序 ≠ 实际顺序」。
      setList(r.order);
      toast.success("优先级已保存", {
        description: "从上到下依次使用；余额低于 100 积分的账号会自动跳过。",
      });
      onSaved?.();
      onOpenChange(false);
    } catch (e) {
      toast.error("保存优先级失败", { description: asError(e).split("\n")[0] });
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={(v) => (busy ? null : onOpenChange(v))}>
      <DialogContent className="max-w-xl">
        <DialogHeader>
          <DialogTitle>账号优先级</DialogTitle>
          <DialogDescription>
            从上到下依次使用，排在前面的先用；余额低于 <b>100 积分</b> 的账号会自动跳到下一个。
            <br />
            通用模型（如 <code className="text-[11px]">glm-5.3</code>）按这份全局顺序；
            <code className="text-[11px]">trae:</code> 这类特定模型只在该产品内部按同一份顺序。
          </DialogDescription>
        </DialogHeader>

        <div className="max-h-[52vh] overflow-y-auto rounded-lg border">
          {list.length === 0 ? (
            <p className="px-3 py-8 text-center text-xs text-muted-foreground">
              账号池里还没有可用账号。
            </p>
          ) : (
            list.map((key, i) => {
              const a = index.get(key);
              if (!a) return null;
              return (
                <div
                  key={key}
                  draggable
                  onDragStart={() => setDragKey(key)}
                  onDragOver={(e) => e.preventDefault()}
                  onDrop={(e) => {
                    e.preventDefault();
                    if (!dragKey) return;
                    const from = list.indexOf(dragKey);
                    setDragKey(null);
                    move(from, i);
                  }}
                  onDragEnd={() => setDragKey(null)}
                  className={`flex items-center gap-2 border-b px-2 py-2 last:border-b-0 ${
                    dragKey === key ? "opacity-40" : ""
                  }`}
                >
                  <GripVertical className="size-3.5 shrink-0 cursor-grab text-muted-foreground" />
                  <span className="w-5 shrink-0 text-right font-mono text-[11px] text-muted-foreground">
                    {i + 1}
                  </span>
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-sm">{a.label}</div>
                    <div className="text-[10px] text-muted-foreground">
                      {PRODUCT_NAME[a.product] ?? a.product} · {a.scope} ·{" "}
                      {a.remaining == null ? "余额未知" : `${Math.round(a.remaining)} 积分`}
                    </div>
                  </div>
                  <div className="flex shrink-0 items-center gap-1">
                    <Button
                      variant="outline"
                      size="icon"
                      className="size-6"
                      disabled={i === 0}
                      onClick={() => move(i, i - 1)}
                      title="上移（更优先）"
                    >
                      <ArrowUp className="size-3.5" />
                    </Button>
                    <Button
                      variant="outline"
                      size="icon"
                      className="size-6"
                      disabled={i === list.length - 1}
                      onClick={() => move(i, i + 1)}
                      title="下移"
                    >
                      <ArrowDown className="size-3.5" />
                    </Button>
                  </div>
                </div>
              );
            })
          )}
        </div>

        <p className="text-[11px] text-muted-foreground">
          拖动行或点箭头都能调序；保存后立即生效，正在进行的对话不受影响。
        </p>

        <DialogFooter>
          <Button variant="outline" disabled={busy} onClick={() => onOpenChange(false)}>
            取消
          </Button>
          <Button disabled={busy || list.length === 0} onClick={() => void save()}>
            {busy ? <Loader2 className="size-4 animate-spin" /> : null}
            保存顺序
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
