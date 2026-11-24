import { useCallback, useEffect, useState } from "react";
import { FolderCog, Loader2, RotateCcw, Save } from "lucide-react";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import * as api from "@/lib/api";

/**
 * 「数据目录」设置卡（用户要求：除核心数据外都允许换路径）。
 *
 * ## 划分原则
 *
 * - **核心数据不可改**：账号库、凭据（DPAPI）、设置，永远在 `~/.twin-switch`。
 *   一旦分家，加密凭据与备份的一致性就无法保证（DPAPI 是本机用户维度的）。
 * - **体积大 / 可重建 / 可搬走的三类允许改**：缓存（Trae 解密副本动辄几百 MB）、
 *   备份、导出包、回收站。
 *
 * ## 一条必须说清的边界
 *
 * 改路径只影响**新文件写到哪里**，不会自动搬迁已有文件 —— 搬几十万个文件中途失败
 * 会留下半套数据。要腾空间请自己把旧目录移走（界面把旧路径也显示出来）。
 */

type Kind = {
  key: string;
  label: string;
  default: string;
  current: string;
  custom: boolean;
};

export function DataPathsCard() {
  const [core, setCore] = useState<string>("");
  const [kinds, setKinds] = useState<Kind[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [draft, setDraft] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState<Record<string, boolean>>({});

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const r = await api.dataPathsGet();
      if (!r.ok) throw new Error(r.error || "读取失败");
      setCore(r.core ?? "");
      const list = (r.kinds ?? []) as Kind[];
      setKinds(list);
      setDraft(Object.fromEntries(list.map((k) => [k.key, k.current])));
      setError(null);
    } catch (e) {
      setError(api.asError(e).split("\n")[0]);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const save = async (key: string, path: string | null) => {
    setBusy((b) => ({ ...b, [key]: true }));
    try {
      const r = await api.dataPathsSet(key, path);
      if (!r.ok) throw new Error(r.error || "保存失败");
      const list = (r.paths?.kinds ?? []) as Kind[];
      setKinds(list);
      setDraft(Object.fromEntries(list.map((k) => [k.key, k.current])));
      toast.success(path ? "目录已更新" : "已恢复默认目录", {
        description: "只影响新写入的文件；旧目录里的内容需要你自己搬",
      });
    } catch (e) {
      toast.error("保存失败", { description: api.asError(e).split("\n")[0] });
    } finally {
      setBusy((b) => ({ ...b, [key]: false }));
    }
  };

  return (
    <Card>
      <CardHeader className="pb-3">
        <CardTitle className="flex items-center gap-2 text-base">
          <FolderCog className="size-4" />
          数据目录
        </CardTitle>
        <CardDescription>
          缓存 / 备份 / 导出 / 回收站都可以换到别的盘。核心数据（账号、凭据、设置）不提供修改。
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        {loading ? (
          <p className="text-xs text-muted-foreground">读取中…</p>
        ) : error ? (
          <p className="text-xs text-destructive">读取失败：{error}</p>
        ) : (
          <>
            <div className="rounded-lg bg-muted/40 px-3 py-2 text-[11px]">
              <div className="font-medium text-foreground">核心数据（不可改）</div>
              <div className="mt-0.5 break-all font-mono text-muted-foreground">{core}</div>
              <div className="mt-1 text-muted-foreground/80">
                账号库、凭据（DPAPI 加密）与设置都在这里 —— 换盘会让凭据解密与备份一致性出问题。
              </div>
            </div>

            <div className="space-y-2">
              {kinds.map((k) => (
                <div key={k.key} className="rounded-lg border border-border/60 px-3 py-2">
                  <div className="flex items-center justify-between gap-2">
                    <div className="min-w-0">
                      <div className="text-xs font-medium">{k.label}</div>
                      <div className="mt-0.5 break-all font-mono text-[10px] text-muted-foreground">
                        {k.current}
                        {k.custom ? (
                          <span className="ml-1 text-emerald-600">（已自定义）</span>
                        ) : null}
                      </div>
                    </div>
                    <div className="flex shrink-0 items-center gap-1">
                      <Button
                        size="sm"
                        variant="outline"
                        disabled={!!busy[k.key] || !k.custom}
                        onClick={() => void save(k.key, null)}
                        title="恢复默认目录"
                      >
                        <RotateCcw className="size-3.5" />
                        恢复
                      </Button>
                    </div>
                  </div>
                  <div className="mt-2 flex items-center gap-1.5">
                    <Input
                      value={draft[k.key] ?? ""}
                      onChange={(e) => setDraft((d) => ({ ...d, [k.key]: e.target.value }))}
                      placeholder="例如 D:\\TwinSwitch\\cache"
                      className="h-8 font-mono text-[11px]"
                    />
                    <Button
                      size="sm"
                      disabled={!!busy[k.key] || !(draft[k.key] ?? "").trim()}
                      onClick={() => void save(k.key, (draft[k.key] ?? "").trim())}
                    >
                      {busy[k.key] ? (
                        <Loader2 className="size-3.5 animate-spin" />
                      ) : (
                        <Save className="size-3.5" />
                      )}
                      保存
                    </Button>
                  </div>
                </div>
              ))}
            </div>

            <p className="text-[10px] leading-relaxed text-muted-foreground/80">
              改目录只影响**之后新写入**的文件，不会自动搬迁已有内容。要腾出 C 盘空间，
              请把旧目录里的文件自己移到新位置（或直接删掉可重建的缓存）。
            </p>
          </>
        )}
      </CardContent>
    </Card>
  );
}
