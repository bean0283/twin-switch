/**
 * Qoder 机器身份（`cosy-machine*` 头）—— 跨机器迁移卡片。
 *
 * ## 用户视角的问题
 *
 * 同一个 Qoder 账号，在 A 电脑能签到、在 B 电脑只显示「无进行中活动」。
 * 原因不是账号，也不是官方改规则，而是**这台机器缺了机器身份头**：
 * 服务端靠 `cosy-machine*` 判断「这是不是装了官方客户端的那台机器」，
 * 缺了就不下发「每天领积分」的活动（只给社区活动 VIEW_DETAILS）。
 *
 * ## 为什么必须手动搬
 *
 * `machinecode` / `machinetype` / `machinetoken` 三个值由客户端**运行时生成、不落盘**
 * （已实测扫过 `%APPDATA%\com.qoder.app.stable` 与 `~/.qoder` 共 1 万多个文件，零命中），
 * 所以没法在新电脑上自动读出来，只能从能签到的那台导出。
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
                    : "这台机器还没配置机器身份，没有可导出的内容"
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
