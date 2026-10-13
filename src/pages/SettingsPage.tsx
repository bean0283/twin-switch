import { useCallback, useEffect, useState } from "react";
import { toast } from "sonner";
import { AlertTriangle, HardDrive, Loader2, RotateCcw, Save, ShieldAlert } from "lucide-react";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import * as api from "@/lib/api";
import {
  KEEP_COUNT_CAP,
  KEEP_DAYS_CAP,
  clampInt,
  gbInputToMb,
  mbToGbInput,
} from "@/lib/backup-policy";
import type { BackupRetention, SettingsSnapshot } from "@/lib/trae-types";

/**
 * 设置页（T62）。
 *
 * 目前只有一项：**整库备份保留策略** —— 时间 / 最大容量 / 保留份数**三重触发**。
 *
 * 它管的是「删会话 / 编辑回写 / 导入 / 导出」这四条链路留下的整库备份
 * （每批 ≈ 一个整库，实测 300 MB ~ 380 MB）。没有这个设置之前，份数是在代码里写死的
 * （删会话留 1 份、导入留 2 份），而且「编辑前备份」因为分组判据漏了 `_before_edit_`
 * 导致一份都清不掉，本机实测堆了 6.4 GB。
 *
 * ⚠️ 三条界面硬规则：
 *  1. **「空」和「失败」绝不许长得一样** —— 读失败时撤掉表单，明确写「读取失败，
 *     不是设置被重置」；绝不能拿默认值填进输入框假装正常。
 *  2. **界面上的策略原文一律用后端返回的 `summary`**，不自己拼 —— 那句话与实际
 *     删文件的判据同源，自己拼就会变成「同一屏两个说法」。
 *  3. **保存后用返回值刷界面**（后端回读一次再返回），不会出现「存进去的是 A、
 *     界面显示的还是 B」。
 */
export default function SettingsPage() {
  const [snap, setSnap] = useState<SettingsSnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  /** 表单草稿。`null` = 还没载入 / 载入失败（此时不渲染表单）。 */
  const [draft, setDraft] = useState<BackupRetention | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const s = await api.settingsGet();
      setSnap(s);
      setDraft(s.backupRetention);
    } catch (cause) {
      // ⚠️ 撤掉表单与默认值：失败时给用户看「30 天 / 3 份 / 4 GB」，
      //    他会以为设置被重置了 —— 实际只是文件坏了。
      setSnap(null);
      setDraft(null);
      setError(api.asError(cause));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  async function save() {
    if (!draft) return;
    setSaving(true);
    try {
      const next = await api.settingsSet(draft);
      setSnap(next);
      // 用**后端回读的实际值**刷表单：手输了离谱的数会被后端夹到合理区间，
      // 界面必须显示夹过之后的值，否则用户以为自己的 99999 天生效了。
      setDraft(next.backupRetention);
      toast.success("设置已保存", { description: next.summary });
    } catch (cause) {
      toast.error("保存失败", { description: api.asError(cause).split("\n")[0] });
    } finally {
      setSaving(false);
    }
  }

  const dirty =
    !!draft &&
    !!snap &&
    (draft.enabled !== snap.backupRetention.enabled ||
      draft.keepDays !== snap.backupRetention.keepDays ||
      draft.keepCount !== snap.backupRetention.keepCount ||
      draft.maxTotalMb !== snap.backupRetention.maxTotalMb);

  return (
    <div className="mx-auto flex min-h-full w-full max-w-4xl flex-col gap-5 px-4 py-6">
      <div>
        <h1 className="text-xl font-semibold tracking-tight">设置</h1>
        <p className="mt-1 text-sm text-muted-foreground">
          工具自己的偏好（落在 <code className="rounded bg-muted px-1 py-0.5 text-xs">~/.twin-switch/settings.json</code>）。
          这里改的都是「本工具怎么干活」，不会动 Trae / WorkBuddy 客户端里的任何数据。
        </p>
      </div>

      {loading ? (
        <Card>
          <CardHeader>
            <Skeleton className="h-5 w-40" />
            <Skeleton className="mt-2 h-4 w-72" />
          </CardHeader>
          <CardContent className="grid gap-3">
            <Skeleton className="h-9 w-full" />
            <Skeleton className="h-9 w-full" />
          </CardContent>
        </Card>
      ) : error ? (
        <Alert variant="destructive" data-settings-error="1">
          <ShieldAlert className="size-4" />
          <AlertTitle>读取设置失败（不是设置被重置）</AlertTitle>
          <AlertDescription className="break-all whitespace-pre-wrap">
            {error}
            <br />
            为避免误导，这一页<strong>没有</strong>用默认值顶上来。可以点下面重试；如果一直失败，
            说明 <code className="rounded bg-muted px-1 py-0.5 text-xs">settings.json</code>{" "}
            内容坏了（删掉它即可回到默认值，不影响任何 Trae / WorkBuddy 数据）。
          </AlertDescription>
        </Alert>
      ) : draft && snap ? (
        <Card data-settings-loaded="1">
          <CardHeader>
            <CardTitle className="flex items-center gap-2 text-base">
              <HardDrive className="size-4" />
              整库备份保留策略
            </CardTitle>
            <CardDescription>
              删会话、改回合、导入、导出这几条链路在动库之前都会先整份备份（每批 ≈ 一个整库，
              实测 300~380 MB）。这里设定它保留多久、留几份、最多占多少。
              <strong className="text-foreground">三重触发，任一超限即清理。</strong>
            </CardDescription>
          </CardHeader>
          <CardContent className="grid min-w-0 gap-5">
            {/* 总开关 */}
            <div className="grid gap-2 rounded-lg border bg-muted/30 px-3 py-3">
              <label className="grid grid-cols-[auto_minmax(0,1fr)] items-start gap-3">
                <input
                  type="checkbox"
                  className="mt-0.5 size-4 shrink-0 accent-[var(--primary)]"
                  checked={draft.enabled}
                  onChange={(e) => setDraft({ ...draft, enabled: e.target.checked })}
                  data-field="enabled"
                  title="开 / 关整库备份的自动清理"
                />
                <span className="min-w-0">
                  <span className="block text-sm font-medium">启用保留策略</span>
                  <span className="mt-0.5 block text-xs text-muted-foreground">
                    关掉之后<strong>不会自动清理任何备份</strong>（备份只增不减，可到清理页逐批手动删）。
                  </span>
                </span>
              </label>
            </div>

            {/* 三重触发 —— 三个输入框必须**等宽对齐**。
                ⚠️ 单位别写成「输入框后面的一个行内 span」：`天`/`份`/`GB` 三个词宽度不同，
                   flex 会让输入框各自缩到不同宽度（实测 144 / 135 / 213 px，看着像没对齐）。
                   给单位一个**固定宽度的槽**（`grid-cols-[minmax(0,1fr)_2.25rem]`），
                   三个框就一样宽、单位也落在同一条竖线上。 */}
            <div className="grid gap-4 sm:grid-cols-3">
              <div className="grid min-w-0 content-start gap-2">
                <Label htmlFor="keep-days" className="text-sm">
                  保留上限 · 时间
                </Label>
                <div className="grid min-w-0 grid-cols-[minmax(0,1fr)_2.25rem] items-center gap-2">
                  <Input
                    id="keep-days"
                    type="number"
                    min={0}
                    max={KEEP_DAYS_CAP}
                    step={1}
                    value={String(draft.keepDays)}
                    disabled={!draft.enabled}
                    onChange={(e) =>
                      setDraft({ ...draft, keepDays: clampInt(e.target.value, 0, KEEP_DAYS_CAP) })
                    }
                    data-field="keepDays"
                    className="min-w-0"
                  />
                  <span className="text-sm text-muted-foreground">天</span>
                </div>
                <p className="text-xs text-muted-foreground">早于这个天数就清掉；0 = 不按时间判。</p>
              </div>

              <div className="grid min-w-0 content-start gap-2">
                <Label htmlFor="keep-count" className="text-sm">
                  保留上限 · 份数
                </Label>
                <div className="grid min-w-0 grid-cols-[minmax(0,1fr)_2.25rem] items-center gap-2">
                  <Input
                    id="keep-count"
                    type="number"
                    min={0}
                    max={KEEP_COUNT_CAP}
                    step={1}
                    value={String(draft.keepCount)}
                    disabled={!draft.enabled}
                    onChange={(e) =>
                      setDraft({ ...draft, keepCount: clampInt(e.target.value, 0, KEEP_COUNT_CAP) })
                    }
                    data-field="keepCount"
                    className="min-w-0"
                  />
                  <span className="text-sm text-muted-foreground">份</span>
                </div>
                <p className="text-xs text-muted-foreground">每个客户端留这么多份；0 = 不按份数判。</p>
              </div>

              <div className="grid min-w-0 content-start gap-2">
                <Label htmlFor="max-total-gb" className="text-sm">
                  保留上限 · 容量
                </Label>
                <div className="grid min-w-0 grid-cols-[minmax(0,1fr)_2.25rem] items-center gap-2">
                  <Input
                    id="max-total-gb"
                    type="number"
                    min={0}
                    max={1024}
                    step={0.5}
                    value={String(mbToGbInput(draft.maxTotalMb))}
                    disabled={!draft.enabled}
                    onChange={(e) =>
                      setDraft({ ...draft, maxTotalMb: gbInputToMb(e.target.value) })
                    }
                    data-field="maxTotalGb"
                    className="min-w-0"
                  />
                  <span className="text-sm text-muted-foreground">GB</span>
                </div>
                <p className="text-xs text-muted-foreground">每个备份目录留这么多；0 = 不按容量判。</p>
              </div>
            </div>

            {/* 清理顺序 —— 用户的要求：**从最旧的一批开始清**，不是「清到只剩一份」 */}
            <div className="rounded-lg border bg-muted/30 px-3 py-2.5 text-xs text-muted-foreground">
              超限时<strong className="text-foreground">从最旧的一批开始清</strong>，清到满足上面三条为止；
              <strong className="text-foreground">每个客户端最新的一批永远保留</strong>
              （那是「出事了能回滚」的唯一依赖）。清理页里也是
              <strong className="text-foreground">最旧的排在前面</strong>，一眼就能看到该先清哪些。
            </div>

            {/* 当前生效策略 —— 原文来自后端，与真正删文件的判据同源 */}
            <div data-policy-summary={snap.summary} className={cnPolicyBox(draft.enabled)}>
              <div className="text-xs font-medium">
                {draft.enabled ? "当前生效：" : "当前不生效："}
                {snap.summary}
              </div>
            </div>

            <div className="flex flex-wrap items-center gap-3">
              <Button onClick={() => void save()} disabled={saving || !dirty} data-action="save">
                {saving ? <Loader2 className="size-4 animate-spin" /> : <Save className="size-4" />}
                保存
              </Button>
              <Button
                variant="outline"
                disabled={saving || !dirty}
                onClick={() => setDraft(snap.backupRetention)}
                data-action="reset"
              >
                <RotateCcw className="size-4" />
                放弃修改
              </Button>
              <span className="min-w-0 text-xs text-muted-foreground break-all">
                默认：30 天 / 每客户端 3 份 / 每目录 4 GB（任一超限即清理）。
              </span>
            </div>

            <div className="min-w-0 text-xs text-muted-foreground break-all">
              设置文件：{snap.path}
            </div>
          </CardContent>
        </Card>
      ) : null}

      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2 text-base">
            <AlertTriangle className="size-4" />
            为什么备份会占这么多盘
          </CardTitle>
          <CardDescription>
            整库备份是<strong>整份</strong>数据库的副本，不是增量 —— 一次删会话就能落下 300 MB 上下。
            不设上限的话，改几次回合就能堆到 GB 级。清理页里「工具残留」那一类会把每一批
            列出来，并标出哪几批超出了这里的策略。
          </CardDescription>
        </CardHeader>
      </Card>
    </div>
  );
}

/** 策略框的配色：关掉时用中性的灰，开启时保持安静（只有真出问题才用琥珀/红）。 */
function cnPolicyBox(enabled: boolean): string {
  return enabled
    ? "grid gap-0 rounded-lg border bg-muted/30 px-3 py-2.5"
    : "grid gap-0 rounded-lg border border-dashed px-3 py-2.5";
}
