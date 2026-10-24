import { useCallback, useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { toast } from "sonner";
import {
  AlertTriangle,
  AppWindow,
  ArrowRight,
  DatabaseBackup,
  HardDrive,
  Loader2,
  RefreshCw,
  RotateCcw,
  Save,
  ShieldAlert,
  Wand2,
} from "lucide-react";

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
import { selectableClients } from "@/lib/trae-client-list";
import type {
  BackupRetention,
  SettingsSnapshot,
  TraeInstalledClient,
} from "@/lib/trae-types";

/**
 * 设置页（T62 起；T66 加了「写库前整库备份」开关）。
 *
 * 两项：**写库前是否留整库备份**（默认关）与**整库备份保留策略**（时间 / 容量 / 份数三重触发）。
 *
 * 保留策略管的是「删会话 / 编辑回写 / 导入 / 导出」这几条链路留下的整库备份。
 * T66 之后这些备份默认是**页级增量**（只存变动的页，通常几 MB），所以策略压力已经小得多；
 * 但**老版本留下的整份备份**还躺在盘上，策略仍然是它们的清理依据。
 *
 * ⚠️ 三条界面硬规则：
 *  1. **「空」和「失败」绝不许长得一样** —— 读失败时撤掉表单，明确写「读取失败，
 *     不是设置被重置」；绝不能拿默认值填进输入框假装正常。
 *  2. **界面上的策略原文一律用后端返回的 `summary` / `beforeWriteSummary`**，不自己拼 ——
 *     那句话与实际判据同源，自己拼就会变成「同一屏两个说法」。
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
  /** 写库前备份开关的草稿。`null` = 还没载入 / 载入失败。 */
  const [beforeWrite, setBeforeWrite] = useState<boolean | null>(null);
  /**
   * 客户端主程序路径的草稿：`clientKey → 绝对路径`。
   * `null` = 还没载入 / 载入失败；空串 = 「这个客户端走自动探测」。
   */
  const [exeDraft, setExeDraft] = useState<Record<string, string> | null>(null);
  /** 本机各客户端的探测结果（数据目录 / 自动探测到的 exe）。 */
  const [clients, setClients] = useState<TraeInstalledClient[] | null>(null);
  const [clientsError, setClientsError] = useState<string | null>(null);
  /** 正在「取运行中的路径」的客户端 key。 */
  const [probing, setProbing] = useState<string | null>(null);
  /** WorkBuddy 主程序路径的草稿（空串 = 走自动探测）。 */
  /** 切号是否同步定时任务（null = 还没读到设置，界面显示骨架）。 */
  const [syncAuto, setSyncAuto] = useState<boolean | null>(null);
  const [wbDraft, setWbDraft] = useState<string>("");
  const [probingWb, setProbingWb] = useState(false);
  const [redetecting, setRedetecting] = useState(false);

  /** 重新拉客户端探测结果（`refresh = 真重新看一遍系统`）。 */
  const loadClients = useCallback(async (refresh = false) => {
    try {
      const r = await api.traeListClients(refresh);
      setClients(r.clients);
      setClientsError(null);
    } catch (cause) {
      // ⚠️ 这里失败**不能**影响上面那份设置：两件事各自独立失败、各自如实报。
      setClients([]);
      setClientsError(api.asError(cause));
    }
  }, []);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const s = await api.settingsGet();
      setSnap(s);
      setDraft(s.backupRetention);
      setBeforeWrite(s.backupBeforeWrite);
      setExeDraft({ ...(s.traeExe ?? {}) });
      setWbDraft(s.workbuddyExe ?? "");
      setSyncAuto(s.syncWorkbuddyAutomations ?? true);
      void loadClients(false);
    } catch (cause) {
      // ⚠️ 撤掉表单与默认值：失败时给用户看「30 天 / 3 份 / 4 GB」，
      //    他会以为设置被重置了 —— 实际只是文件坏了。
      setSnap(null);
      setDraft(null);
      setBeforeWrite(null);
      setExeDraft(null);
      setSyncAuto(null);
      setError(api.asError(cause));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load, loadClients]);

  async function save() {
    if (!draft || beforeWrite === null || exeDraft === null) return;
    setSaving(true);
    try {
      // ⚠️ 传**补丁**而不是整份设置：这里三项都改了，但以后加字段时不会互相覆盖。
      const next = await api.settingsSet({
        backupRetention: draft,
        backupBeforeWrite: beforeWrite,
        // 空串视为「恢复自动探测」：后端会把它从映射里剔掉。
        traeExe: cleanExeDraft(exeDraft),
        workbuddyExe: wbDraft.trim(),
        syncWorkbuddyAutomations: syncAuto ?? true,
      });
      setSnap(next);
      // 用**后端回读的实际值**刷表单：手输了离谱的数会被后端夹到合理区间，
      // 界面必须显示夹过之后的值，否则用户以为自己的 99999 天生效了。
      setDraft(next.backupRetention);
      setBeforeWrite(next.backupBeforeWrite);
      setExeDraft({ ...(next.traeExe ?? {}) });
      setWbDraft(next.workbuddyExe ?? "");
      setSyncAuto(next.syncWorkbuddyAutomations ?? true);
      // 路径改了 ⇒ 让探测结果跟着刷新（否则下面那几行还挂着「未找到」）。
      void loadClients(true);
      toast.success("设置已保存", {
        // 只改了路径时别拿「备份开关」的摘要当前描述 —— 那是另一件事。
        description: sameExeMap(cleanExeDraft(exeDraft), next.traeExe ?? {})
          ? next.beforeWriteSummary
          : "客户端主程序路径已更新",
      });
    } catch (cause) {
      toast.error("保存失败", { description: api.asError(cause).split("\n")[0] });
    } finally {
      setSaving(false);
    }
  }

  const dirty =
    !!draft &&
    !!snap &&
    beforeWrite !== null &&
    exeDraft !== null &&
    ((wbDraft.trim() !== (snap.workbuddyExe ?? "").trim()) ||
      !sameExeMap(cleanExeDraft(exeDraft), snap.traeExe ?? {}) ||
      draft.enabled !== snap.backupRetention.enabled ||
      draft.keepDays !== snap.backupRetention.keepDays ||
      draft.keepCount !== snap.backupRetention.keepCount ||
      draft.maxTotalMb !== snap.backupRetention.maxTotalMb ||
      beforeWrite !== snap.backupBeforeWrite ||
      (syncAuto ?? true) !== (snap.syncWorkbuddyAutomations ?? true));

  /** 取某个客户端「正在运行的」主程序路径并填进草稿。 */
  async function pickRunning(key: string) {
    setProbing(key);
    try {
      const r = await api.traeExeFromRunning(key);
      setExeDraft((prev) => ({ ...(prev ?? {}), [key]: r.path }));
      toast.success("已填入运行中的客户端路径", { description: r.path });
    } catch (cause) {
      toast.error("没取到路径", { description: api.asError(cause).split("\n")[0] });
    } finally {
      setProbing(null);
    }
  }

  /** 取「正在运行的 WorkBuddy」的主程序路径。 */
  async function pickRunningWb() {
    setProbingWb(true);
    try {
      const r = await api.workbuddyExeFromRunning();
      setWbDraft(r.path);
      toast.success("已填入运行中的 WorkBuddy 路径", { description: r.path });
    } catch (cause) {
      toast.error("没取到路径", { description: api.asError(cause).split("\n")[0] });
    } finally {
      setProbingWb(false);
    }
  }

  async function redetect() {
    setRedetecting(true);
    try {
      await loadClients(true);
      toast.success("已重新检测本机客户端");
    } finally {
      setRedetecting(false);
    }
  }

  /**
   * 这个卡片里列哪些 Trae 客户端 —— **必须过 `selectableClients`**（唯一出口）。
   *
   * ⚠️ 2026-10 官方把 SOLO 并进了 Trae：`solo-cn` 只剩数据目录、没有自己的程序。
   * 若按「数据目录在」就列出来，用户会看到一个永远「未找到」的 TRAE SOLO CN
   * （2026-10-18 用户截图指出正是这个）。判据只认 `@/lib/trae-client-list`，
   * 不在这里另写一份。
   */
  const visibleTraeClients = selectableClients(clients ?? []).filter(
    (c) => c.installed || !!c.exe || !!exeDraft?.[c.key],
  );
  /** WorkBuddy 是否被手动指定。 */
  const wbManual = wbDraft.trim().length > 0;

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
      ) : draft && snap && beforeWrite !== null ? (
        <>
          {/* ── 客户端主程序路径（T69） ─────────────────────────────────── */}
          <Card data-settings-exe="1">
            <CardHeader>
              <CardTitle className="flex items-center gap-2 text-base">
                <AppWindow className="size-4" />
                客户端主程序路径
              </CardTitle>
              <CardDescription>
                Trae 与 WorkBuddy 都允许装在任何目录。装在自定义位置（例如{" "}
                <code className="rounded bg-muted px-1 py-0.5 text-xs">D://Trae CN</code>
                ）时自动探测可能找不到主程序，切换 / 导出就会报「未找到主程序」——
                在这里指定一次即可。<strong className="text-foreground">留空 = 自动探测。</strong>
                <br />
                已并入 Trae 的 SOLO 不再列出（官方已无该客户端，它只剩历史数据）。
              </CardDescription>
            </CardHeader>
            <CardContent className="grid min-w-0 gap-4">
              {clientsError ? (
                <p className="text-xs break-all text-destructive">
                  读取本机客户端失败（<strong>不影响下面的设置</strong>）：{clientsError}
                </p>
              ) : null}

              {visibleTraeClients.length === 0 ? (
                <p className="text-xs text-muted-foreground">
                  {clients === null ? "正在检测本机客户端…" : "没有检测到已安装的 Trae 客户端。"}
                </p>
              ) : (
                visibleTraeClients.map((c) => {
                    const draftPath = exeDraft?.[c.key] ?? "";
                    const manual = draftPath.trim().length > 0;
                    return (
                      <div
                        key={c.key}
                        className="grid min-w-0 gap-2 rounded-lg border bg-muted/30 px-3 py-3"
                        data-exe-client={c.key}
                      >
                        <div className="flex flex-wrap items-center gap-2">
                          <span className="text-sm font-medium">{c.label}</span>
                          <span
                            className={
                              manual
                                ? "rounded-full bg-[color-mix(in_oklab,var(--primary)_16%,transparent)] px-2 py-0.5 text-[11px] text-foreground"
                                : c.exe
                                  ? "rounded-full bg-muted px-2 py-0.5 text-[11px] text-muted-foreground"
                                  : "rounded-full bg-destructive/10 px-2 py-0.5 text-[11px] text-destructive"
                            }
                            data-exe-state={manual ? "manual" : c.exe ? "auto" : "missing"}
                          >
                            {manual ? "手动指定" : c.exe ? "自动探测" : "未找到"}
                          </span>
                          <span className="ml-auto flex items-center gap-1.5">
                            <Button
                              type="button"
                              size="sm"
                              variant="outline"
                              onClick={() => void pickRunning(c.key)}
                              disabled={probing === c.key}
                              title="取正在运行的客户端的主程序路径（它没在跑就取不到，会如实报错）"
                            >
                              {probing === c.key ? (
                                <Loader2 className="size-3.5 animate-spin" />
                              ) : (
                                <Wand2 className="size-3.5" />
                              )}
                              用运行中的
                            </Button>
                            {manual ? (
                              <Button
                                type="button"
                                size="sm"
                                variant="ghost"
                                onClick={() =>
                                  setExeDraft((prev) => {
                                    const next = { ...(prev ?? {}) };
                                    delete next[c.key];
                                    return next;
                                  })
                                }
                                title="清掉手动指定，回到自动探测"
                              >
                                恢复自动
                              </Button>
                            ) : null}
                          </span>
                        </div>

                        <Input
                          value={draftPath}
                          onChange={(e) =>
                            setExeDraft((prev) => ({ ...(prev ?? {}), [c.key]: e.target.value }))
                          }
                          placeholder={c.exe ?? "粘贴客户端主程序的完整路径（.exe）"}
                          spellCheck={false}
                          autoComplete="off"
                          data-exe-input={c.key}
                        />

                        <p className="text-xs break-all text-muted-foreground">
                          {manual ? (
                            <>
                              保存后以这个路径为准。文件不存在时会被忽略并退回自动探测 ——
                              这样拔了移动硬盘也不会让切换去 spawn 一个不存在的程序。
                            </>
                          ) : c.exe ? (
                            <>自动探测到：{c.exe}</>
                          ) : (
                            <>
                              自动探测没找到它。可以点上面的「用运行中的」，或右键桌面快捷方式 →
                              「打开文件所在位置」把 .exe 路径贴进来。
                            </>
                          )}
                        </p>
                      </div>
                    );
                  })
              )}

              {/* ── 切号时同步定时任务 ─────────────────────────────────────
                  WorkBuddy 的定时任务存在 workbuddy.db 的 automations 表里，**按账号归属**
                  （owner_user_id）。切到另一个账号后原任务在客户端里会「消失」。
                  默认开启同步：切过去还能看到原来的任务（按名称+周期+提示词去重，不会越积越多）。 */}
              <label className="flex cursor-pointer items-start gap-2.5 rounded-lg border bg-muted/30 px-3 py-3">
                <input
                  type="checkbox"
                  className="mt-0.5"
                  checked={syncAuto ?? true}
                  disabled={syncAuto === null}
                  onChange={(e) => setSyncAuto(e.target.checked)}
                />
                <span className="min-w-0">
                  <span className="text-sm font-medium">切换账号时同步 WorkBuddy 定时任务</span>
                  <span className="mt-0.5 block text-xs text-muted-foreground">
                    定时任务按账号归属，切号后在客户端里会「消失」。开启后本工具会把它们复制一份给新账号
                    （同名同周期的已存在就跳过）。关掉则**只切登录态**，不碰定时任务。
                  </span>
                </span>
              </label>

              {/* ── WorkBuddy ─────────────────────────────────────────────
                  与上面几行同一套交互：手动指定 → 运行中进程 → 自动探测。
                  WorkBuddy 也能装到自定义目录，同一个问题给它同一个入口。 */}
              <div
                className="grid min-w-0 gap-2 rounded-lg border bg-muted/30 px-3 py-3"
                data-exe-client="workbuddy"
              >
                <div className="flex flex-wrap items-center gap-2">
                  <span className="text-sm font-medium">WorkBuddy</span>
                  <span
                    className={
                      wbManual
                        ? "rounded-full bg-[color-mix(in_oklab,var(--primary)_16%,transparent)] px-2 py-0.5 text-[11px] text-foreground"
                        : snap.workbuddyDetected
                          ? "rounded-full bg-muted px-2 py-0.5 text-[11px] text-muted-foreground"
                          : "rounded-full bg-destructive/10 px-2 py-0.5 text-[11px] text-destructive"
                    }
                    data-exe-state={
                      wbManual ? "manual" : snap.workbuddyDetected ? "auto" : "missing"
                    }
                  >
                    {wbManual ? "手动指定" : snap.workbuddyDetected ? "自动探测" : "未找到"}
                  </span>
                  <span className="ml-auto flex items-center gap-1.5">
                    <Button
                      type="button"
                      size="sm"
                      variant="outline"
                      onClick={() => void pickRunningWb()}
                      disabled={probingWb}
                      title="取正在运行的 WorkBuddy 的主程序路径（它没在跑就取不到，会如实报错）"
                    >
                      {probingWb ? (
                        <Loader2 className="size-3.5 animate-spin" />
                      ) : (
                        <Wand2 className="size-3.5" />
                      )}
                      用运行中的
                    </Button>
                    {wbManual ? (
                      <Button
                        type="button"
                        size="sm"
                        variant="ghost"
                        onClick={() => setWbDraft("")}
                        title="清掉手动指定，回到自动探测"
                      >
                        恢复自动
                      </Button>
                    ) : null}
                  </span>
                </div>

                <Input
                  value={wbDraft}
                  onChange={(e) => setWbDraft(e.target.value)}
                  placeholder={snap.workbuddyDetected ?? "粘贴 WorkBuddy.exe 的完整路径"}
                  spellCheck={false}
                  autoComplete="off"
                  data-exe-input="workbuddy"
                />

                <p className="text-xs break-all text-muted-foreground">
                  {wbManual ? (
                    <>
                      保存后以这个路径为准。文件不存在时会被忽略并退回自动探测。
                    </>
                  ) : snap.workbuddyDetected ? (
                    <>自动探测到：{snap.workbuddyDetected}</>
                  ) : (
                    <>
                      自动探测没找到它。可以先打开一次 WorkBuddy，再点上面的「用运行中的」。
                    </>
                  )}
                </p>
              </div>

              <div className="flex flex-wrap items-center gap-2">
                <Button
                  type="button"
                  size="sm"
                  variant="outline"
                  onClick={() => void redetect()}
                  disabled={redetecting}
                  title="重新看一遍本机（注册表 / 运行中的进程 / 常见目录）"
                >
                  {redetecting ? (
                    <Loader2 className="size-3.5 animate-spin" />
                  ) : (
                    <RefreshCw className="size-3.5" />
                  )}
                  重新检测
                </Button>
                <span className="text-xs text-muted-foreground">
                  改完点下面的「保存」才生效。
                </span>
              </div>
            </CardContent>
          </Card>

          {/* ── 写库前整库备份（T66，默认关） ─────────────────────────────── */}
          <Card data-settings-loaded="1">
            <CardHeader>
              <CardTitle className="flex items-center gap-2 text-base">
                <DatabaseBackup className="size-4" />
                写库前整库备份
              </CardTitle>
              <CardDescription>
                删除会话 / 改回合 / 导入这几条链路在动库之前，要不要先留一份库的副本。
                <strong className="text-foreground">默认关闭。</strong>
              </CardDescription>
            </CardHeader>
            <CardContent className="grid min-w-0 gap-4">
              <div
                className={
                  beforeWrite
                    ? "grid gap-2 rounded-lg border bg-muted/30 px-3 py-3"
                    : "grid gap-2 rounded-lg border border-dashed px-3 py-3"
                }
              >
                <label className="grid grid-cols-[auto_minmax(0,1fr)] items-start gap-3">
                  <input
                    type="checkbox"
                    className="mt-0.5 size-4 shrink-0 accent-[var(--primary)]"
                    checked={beforeWrite}
                    onChange={(e) => setBeforeWrite(e.target.checked)}
                    data-field="backupBeforeWrite"
                    title="开 / 关「写库前先留一份备份」"
                  />
                  <span className="min-w-0">
                    <span className="block text-sm font-medium">
                      写库前先备份（现在：{beforeWrite ? "已开启" : "已关闭"})
                    </span>
                    <span
                      className="mt-0.5 block text-xs text-muted-foreground"
                      data-before-write-summary={snap.beforeWriteSummary}
                    >
                      {snap.beforeWriteSummary}
                    </span>
                  </span>
                </label>
              </div>

              {/* 关掉之后会怎样 / 为什么关了也安全 —— 用户要求「引导用户知道这个开关」，
                  所以这里必须把两种选择的后果都写清，而不是只写「默认关闭」。 */}
              <div className="grid min-w-0 gap-2 rounded-lg border bg-muted/30 px-3 py-2.5 text-xs text-muted-foreground">
                <div>
                  <strong className="text-foreground">关掉之后：</strong>
                  删除 / 改回合 / 导入之后<strong className="text-foreground">无法把库退回到操作前</strong>
                  （操作本身的可见结果不受影响，只是没有历史版本可回退）。
                </div>
                <div>
                  <strong className="text-foreground">为什么关了也不容易出事：</strong>
                  换库位走的是<strong className="text-foreground">原子替换</strong> ——
                  旧库先改名让位、新库再改名就位，任何一步失败都会自动回滚。
                  「备份」只负责留历史版本，不再负责「防止写坏」。
                </div>
                <div>
                  <strong className="text-foreground">打开之后会有多大：</strong>
                  T66 起备份是<strong className="text-foreground">页级增量</strong> ——
                  只存跟原库不同的那几个页（Trae 库逐页加密，写库也只会改动几页），
                  通常几 MB；只有变动超过一半页时才退回整份。
                </div>
              </div>
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle className="flex items-center gap-2 text-base">
                <HardDrive className="size-4" />
                整库备份保留策略
              </CardTitle>
              <CardDescription>
                上面那个开关打开时，每次写库都会落下一份备份。这里设定它保留多久、留几份、
                最多占多少。<strong className="text-foreground">三重触发，任一超限即清理。</strong>
                早先版本的「整份」备份（每份 300 MB 上下）也由这套策略清理。
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
                onClick={() => {
                  setDraft(snap.backupRetention);
                  setBeforeWrite(snap.backupBeforeWrite);
                  setExeDraft({ ...(snap.traeExe ?? {}) });
                  setWbDraft(snap.workbuddyExe ?? "");
                }}
                data-action="reset"
              >
                <RotateCcw className="size-4" />
                放弃修改
              </Button>
              <span className="min-w-0 text-xs text-muted-foreground break-all">
                默认：不备份 / 30 天 / 每客户端 3 份 / 每目录 4 GB（任一超限即清理）。
              </span>
            </div>

            <div className="min-w-0 text-xs text-muted-foreground break-all">
              设置文件：{snap.path}
            </div>
            </CardContent>
          </Card>
        </>
      ) : null}

      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2 text-base">
            <AlertTriangle className="size-4" />
            备份为什么会占盘
          </CardTitle>
          <CardDescription>
            <strong className="text-foreground">新备份（T66 起）</strong>是页级增量：只存跟原库不同的页，
            通常几 MB —— 打开「写库前整库备份」也不至于再堆出 GB。
            <br />
            <strong className="text-foreground">老备份（T66 之前）</strong>是整份库副本，一次删会话就落下
            300 MB 上下，改几次回合就能堆到 GB 级 —— 盘上现存的那批就是这些。
            清理页里「工具残留」那一类会把每一批列出来（
            <strong className="text-foreground">最旧的排在前面</strong>），并标出哪几批超出了上面的策略；
            每一批都能单独「还原」回实时库。
          </CardDescription>
        </CardHeader>
        <CardContent>
          <Link
            to="/trae-cleanup"
            className="inline-flex items-center gap-1 text-xs font-medium text-primary hover:underline"
          >
            去清理页看每一批备份
            <ArrowRight className="size-3.5" />
          </Link>
        </CardContent>
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

/** 去掉空项：空串 = 「恢复自动探测」，不写进设置文件。 */
function cleanExeDraft(m: Record<string, string>): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(m)) {
    const t = v.trim();
    if (t) out[k] = t;
  }
  return out;
}

/** 两个「路径映射」是否等价（与键的顺序无关）—— 「有没有改动」的判据用它。 */
function sameExeMap(a: Record<string, string>, b: Record<string, string>): boolean {
  const ka = Object.keys(a).sort();
  const kb = Object.keys(b).sort();
  if (ka.length !== kb.length) return false;
  return ka.every((k, i) => k === kb[i] && a[k] === b[k]);
}
