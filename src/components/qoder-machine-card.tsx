/**
 * Qoder 机器身份（`cosy-machine*` 头）—— 设备身份状态与跨机器迁移。
 *
 * ## 用户视角的问题
 *
 * 同一个 Qoder 账号，在 A 电脑能签到、在 B 电脑只显示「无进行中活动」。
 * 原因不是账号，也不是官方改规则，而是**这台机器缺了机器身份头**：
 * 服务端靠 `cosy-machine*` 判断「这是不是装了官方客户端的那台机器」，
 * 缺了就不下发「每天领积分」的活动（只给社区活动 VIEW_DETAILS）。
 *
 * ## 两条路（按优先级）
 *
 * 1. **本机现取**：装了 Qoder 客户端 → 调它自带的 `resources\umid\runtime-info`
 *    算出 `machineCode/Type/Token`。这是正路，新设备装好客户端就能签，不用搬配置。
 * 2. **手工搬**：机器上装不了客户端（无头机、公司电脑）时，从能签到的那台导出、这台导入。
 *
 * 以前这里写的是「三个值不落盘，所以没法自动读」——值确实不落盘，但组件能现算，
 * 2026-11-02 更正。
 */

import { useCallback, useEffect, useState } from "react";
import {
  AlertTriangle,
  Check,
  Cpu,
  Download,
  Loader2,
  Upload,
} from "lucide-react";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import * as api from "@/lib/api";
import { copyText } from "@/lib/clipboard";
import { toast } from "sonner";

export function QoderMachineCard() {
  const [st, setSt] = useState<api.QoderMachineStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<string | null>(null);
  const [pasted, setPasted] = useState("");

  const load = useCallback(async () => {
    setLoading(true);
    try {
      setSt(await api.qoderMachineStatus());
    } catch (cause) {
      toast.error("读取机器身份失败", { description: api.asError(cause) });
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  async function onExport() {
    setBusy("export");
    try {
      const v = await api.qoderMachineExport();
      const text = JSON.stringify(v, null, 2);
      const ok = await copyText(text);
      if (!ok) {
        // ⚠️ 复制失败必须说出来：静默失败会让用户以为已经复制好，粘过去却是空的。
        toast.error("复制失败", { description: "剪贴板不可用，请手动选择下方内容复制" });
        return;
      }
      toast.success("已复制机器身份", {
        description: "粘到另一台电脑的「导入」框里即可。内容含设备凭据，别外传。",
      });
    } catch (cause) {
      toast.error("导出失败", { description: api.asError(cause) });
    } finally {
      setBusy(null);
    }
  }

  async function onImport() {
    const text = pasted.trim();
    if (!text) {
      toast.error("请先粘贴另一台电脑导出的内容");
      return;
    }
    setBusy("import");
    try {
      const r = await api.qoderMachineImport(text);
      setSt(r.status);
      setPasted("");
      toast.success("机器身份已导入", {
        description: "现在重新点签到即可（本工具不会自动重启客户端）。",
      });
    } catch (cause) {
      toast.error("导入失败", { description: api.asError(cause) });
    } finally {
      setBusy(null);
    }
  }

  const missing = st
    ? [
        !st.machineCode && "machineCode",
        !st.machineToken && "machineToken",
        !st.machineType && "machineType",
      ].filter(Boolean)
    : [];

  return (
    <Card data-settings-qoder-machine="1">
      <CardHeader>
        <CardTitle className="flex items-center gap-2 text-base">
          <Cpu className="size-4" />
          Qoder 机器身份
          {st ? (
            st.ready ? (
              <Badge variant="secondary" className="gap-1">
                <Check className="size-3" />
                已就绪
              </Badge>
            ) : (
              <Badge variant="outline" className="gap-1 text-amber-600 dark:text-amber-400">
                <AlertTriangle className="size-3" />
                缺 {missing.length} 项
              </Badge>
            )
          ) : null}
        </CardTitle>
        <CardDescription>
          Qoder 服务端靠这组 <code className="text-xs">cosy-machine*</code> 头判断
          「是不是装了官方客户端的那台机器」。
          <b>缺了它，签到只会拿到社区活动，拿不到「每天领积分」</b>
          —— 换电脑后签不了通常就是这个原因，与账号和官方规则都无关。
          <br />
          本机装了 Qoder 客户端时会自动从客户端取（<code className="text-xs">umid/runtime-info</code>），
          不需要手动搬；只有装不了客户端的机器才需要下面那套导出/导入。
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        {loading ? (
          <div className="flex items-center gap-2 text-sm text-muted-foreground">
            <Loader2 className="size-4 animate-spin" />
            正在读取…
          </div>
        ) : st ? (
          <>
            <div className="grid gap-1.5 text-xs text-muted-foreground sm:grid-cols-2">
              <div>
                machineCode：{st.machineCode ? "✓" : <span className="text-amber-600">缺</span>}
              </div>
              <div>
                machineType：{st.machineType ? "✓" : <span className="text-amber-600">缺</span>}
              </div>
              <div>
                machineToken：{st.machineToken ? "✓" : <span className="text-amber-600">缺</span>}
              </div>
              <div>
                machineId：
                {st.machineId ? "✓" : <span className="text-amber-600">缺</span>}
                {st.autoMachineId.global || st.autoMachineId.cn ? "（自动读自客户端）" : ""}
              </div>
              <div className="sm:col-span-2">
                主机名：<code className="font-mono">{st.hostname}</code> · 系统：{" "}
                <code className="font-mono">{st.machineOs}</code>
              </div>
              <div className="break-all sm:col-span-2">
                配置：<code className="font-mono">{st.path}</code>
              </div>
              <div className="sm:col-span-2">
                取身份方式：
                {st.source?.cn === "umid" || st.source?.global === "umid" ? (
                  <span className="text-emerald-600 dark:text-emerald-400">
                    本机客户端现取（装了客户端即可，无需搬配置）
                  </span>
                ) : st.source?.cn === "config" || st.source?.global === "config" ? (
                  <span>配置文件（抓包手填 / 从别的机器导入）</span>
                ) : (
                  <span className="text-amber-600 dark:text-amber-400">
                    未取到 —— 装 Qoder 客户端并登录一次即可自动获取
                  </span>
                )}
                {st.clientVersion ? (
                  <>
                    {" · "}客户端版本 <code className="font-mono">{st.clientVersion}</code>
                  </>
                ) : null}
              </div>
            </div>

            <div className="flex flex-wrap items-center gap-2">
              <Button
                variant="outline"
                size="sm"
                onClick={() => void onExport()}
                disabled={busy !== null || !st.ready}
                title={
                  st.ready
                    ? "把本机机器身份复制出来，粘到另一台电脑导入"
                    : "本机没有可用身份：装 Qoder 客户端最省事；装不了才需要从别的机器导入"
                }
              >
                {busy === "export" ? (
                  <Loader2 className="size-4 animate-spin" />
                ) : (
                  <Download className="size-4" />
                )}
                导出（复制）
              </Button>
              <span className="text-xs text-muted-foreground">
                在<b>能正常签到</b>的那台电脑上点导出，粘到下面导入。
              </span>
            </div>

            <div className="space-y-2">
              <textarea
                className="h-24 w-full resize-y rounded-md border bg-background px-2 py-1.5 font-mono text-[11px] leading-relaxed"
                placeholder='把另一台电脑导出的 JSON 粘到这里，形如 {"format":"twinswitch.qoder-machine",...}'
                value={pasted}
                onChange={(e) => setPasted(e.target.value)}
                spellCheck={false}
              />
              <div className="flex items-center gap-2">
                <Button
                  size="sm"
                  onClick={() => void onImport()}
                  disabled={busy !== null || !pasted.trim()}
                >
                  {busy === "import" ? (
                    <Loader2 className="size-4 animate-spin" />
                  ) : (
                    <Upload className="size-4" />
                  )}
                  导入
                </Button>
                <span className="text-xs text-muted-foreground">
                  导入会覆盖本机的这 6 个字段（含 machineId，整套照搬才自洽）。
                </span>
              </div>
            </div>

            <p className="text-xs text-muted-foreground">
              ⚠️ 导出内容含 <code className="font-mono">machinetoken</code>
              （服务端签发的设备凭据），只在自己机器之间传递，别发给别人。
            </p>
          </>
        ) : (
          <div className="text-sm text-destructive">读取失败，请重试</div>
        )}
      </CardContent>
    </Card>
  );
}
