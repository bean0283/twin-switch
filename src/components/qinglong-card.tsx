import { useCallback, useEffect, useState } from "react";
import { Loader2, PlugZap, Upload } from "lucide-react";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import * as api from "@/lib/api";
import { copyText } from "@/lib/clipboard";

/**
 * 青龙面板配置卡（设置 → 青龙面板 tab）。
 *
 * 上下两块：
 * 1. 连接配置 —— 开关 / 面板地址 / Client ID+Secret / 三个产品的变量名 / 测试连接；
 * 2. 通用环境变量提交 —— DD_WEBHOOK 这类与账号无关的变量，填名/值/备注直接提交；
 *    同名已存在时后端返回清单，弹行内选择「覆盖哪条 / 追加一条」。
 *
 * ⚠️ Secret 只在「改过」时才随保存发后端：输入框留空 = 保留原值（界面上永远
 * 看不到明文，只显示打码），避免「打开设置页随手一存就把空串写进去」。
 */
export function QinglongCard() {
  const [cfg, setCfg] = useState<api.QinglongConfigView | null>(null);
  const [loadErr, setLoadErr] = useState<string | null>(null);
  const [url, setUrl] = useState("");
  const [clientId, setClientId] = useState("");
  const [secret, setSecret] = useState("");
  const [envTrae, setEnvTrae] = useState("TRAE_TOKEN");
  const [envWb, setEnvWb] = useState("WB_TOKEN");
  const [envQoder, setEnvQoder] = useState("QODER_TOKEN");
  const [saving, setSaving] = useState(false);
  const [testing, setTesting] = useState(false);
  const [status, setStatus] = useState<api.QinglongStatus | null>(null);

  // —— 通用环境变量提交 ——
  const [envName, setEnvName] = useState("");
  const [envValue, setEnvValue] = useState("");
  const [envRemarks, setEnvRemarks] = useState("");
  const [envBusy, setEnvBusy] = useState(false);
  const [choice, setChoice] = useState<api.QinglongEnvChoice | null>(null);

  const load = useCallback(async () => {
    try {
      const c = await api.qinglongConfigGet();
      setCfg(c);
      setUrl(c.url);
      setClientId(c.clientId);
      setEnvTrae(c.envTrae || "TRAE_TOKEN");
      setEnvWb(c.envWb || "WB_TOKEN");
      setEnvQoder(c.envQoder || "QODER_TOKEN");
      setLoadErr(null);
    } catch (cause) {
      setLoadErr(api.asError(cause));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  /** 保存（可选同时改开关），成功后刷新一次连接状态。 */
  async function save(patch: Partial<{ enabled: boolean }> = {}) {
    setSaving(true);
    try {
      await api.qinglongConfigSet({
        enabled: patch.enabled,
        url,
        clientId,
        clientSecret: secret.trim() ? secret : null,
        envTrae,
        envWb,
        envQoder,
      });
      setSecret("");
      await load();
      const st = await api.qinglongStatus();
      setStatus(st);
      if (st.enabled && st.connected) {
        toast.success("已保存并连上青龙", { description: st.base });
      } else if (st.enabled) {
        toast.warning("已保存，但连接失败", { description: st.error ?? "未知原因" });
      } else {
        toast.success("已保存（开关已关闭）");
      }
    } catch (cause) {
      toast.error("保存失败", { description: api.asError(cause) });
    } finally {
      setSaving(false);
    }
  }

  async function testConnection() {
    setTesting(true);
    try {
      // 先落盘再测：测试用的就是保存后的配置（与生效口径同源）。
      await save();
    } finally {
      setTesting(false);
    }
  }

  async function submitEnv(mode?: "update" | "append", envId?: number) {
    if (!envName.trim()) {
      toast.error("变量名不能为空");
      return;
    }
    if (!envValue.trim()) {
      toast.error("变量值不能为空");
      return;
    }
    setEnvBusy(true);
    try {
      const res = await api.qinglongSubmitEnv({
        name: envName,
        value: envValue,
        remarks: envRemarks,
        mode,
        envId,
      });
      if ("needChoice" in res) {
        setChoice(res);
        return;
      }
      setChoice(null);
      setEnvValue("");
      toast.success(
        res.action === "updated" ? `已覆盖 ${res.envName}（id=${res.id}）` : `已新增 ${res.envName}`,
      );
    } catch (cause) {
      toast.error("提交失败", { description: api.asError(cause) });
    } finally {
      setEnvBusy(false);
    }
  }

  if (loadErr) {
    return (
      <Card>
        <CardContent className="pt-6 text-sm text-destructive">读取配置失败：{loadErr}</CardContent>
      </Card>
    );
  }
  if (!cfg) {
    return (
      <Card>
        <CardContent className="flex items-center gap-2 pt-6 text-sm text-muted-foreground">
          <Loader2 className="size-4 animate-spin" /> 正在读取配置…
        </CardContent>
      </Card>
    );
  }

  const connText = !cfg.enabled
    ? "开关关闭：一切提交入口都不可见"
    : status?.connected
      ? `已连接 ${status.base}（面板共 ${status.envs ?? "?"} 条变量）`
      : status?.error
        ? `连接失败：${status.error}`
        : "尚未测试连接";

  return (
    <div className="grid gap-4">
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center justify-between">
            <span>青龙面板</span>
            <span className="flex items-center gap-2 text-sm font-normal">
              <span className="text-xs text-muted-foreground">{connText}</span>
              <Switch
                checked={cfg.enabled}
                onCheckedChange={(v) => void save({ enabled: v })}
                disabled={saving}
                aria-label="启用青龙提交"
              />
            </span>
          </CardTitle>
          <CardDescription>
            开启后，账号 token 弹窗会出现「提交到青龙」按钮；提交走面板的 openapi，
            同账号旧变量自动覆盖、没有才新增。开关关闭时界面上没有任何提交入口。
          </CardDescription>
        </CardHeader>
        <CardContent className="grid gap-4">
          {/* 第一行：面板地址（整行） */}
          <div className="grid gap-1.5">
            <Label htmlFor="ql-url">面板地址</Label>
            <Input
              id="ql-url"
              className="font-mono"
              placeholder="https://ql.example.com"
              value={url}
              onChange={(e) => setUrl(e.target.value)}
            />
            <p className="text-xs text-muted-foreground">
              可填穿透入口：会自动跟随跳转解出真实地址并记住（漂移时先试上次可用的）。
            </p>
          </div>

          {/* 第二行：Client ID + Client Secret */}
          <div className="grid gap-4 sm:grid-cols-2">
            <div className="grid gap-1.5">
              <Label htmlFor="ql-cid">Client ID</Label>
              <Input
                id="ql-cid"
                className="font-mono"
                placeholder="青龙「应用」页创建"
                value={clientId}
                onChange={(e) => setClientId(e.target.value)}
              />
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="ql-secret">Client Secret</Label>
              <Input
                id="ql-secret"
                type="password"
                className="font-mono"
                placeholder={cfg.hasSecret ? `已保存（${cfg.secretMasked}），留空 = 不改` : "青龙「应用」页创建"}
                value={secret}
                onChange={(e) => setSecret(e.target.value)}
              />
            </div>
          </div>

          {/* 第三行：三个产品的环境变量名 */}
          <div className="grid grid-cols-3 gap-2">
            <div className="grid gap-1.5">
              <Label htmlFor="ql-env-trae">Trae 变量名</Label>
              <Input id="ql-env-trae" className="font-mono" value={envTrae} onChange={(e) => setEnvTrae(e.target.value)} />
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="ql-env-wb">WB 变量名</Label>
              <Input id="ql-env-wb" className="font-mono" value={envWb} onChange={(e) => setEnvWb(e.target.value)} />
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="ql-env-qoder">Qoder 变量名</Label>
              <Input id="ql-env-qoder" className="font-mono" value={envQoder} onChange={(e) => setEnvQoder(e.target.value)} />
            </div>
          </div>

          <div className="flex flex-wrap items-center gap-2">
            <Button size="sm" variant="outline" disabled={saving} onClick={() => void testConnection()}>
              {testing ? <Loader2 className="size-4 animate-spin" /> : <PlugZap className="size-4" />}
              测试连接
            </Button>
            <Button size="sm" variant="outline" disabled={saving} onClick={() => void save()}>
              {saving ? <Loader2 className="size-4 animate-spin" /> : null}
              保存
            </Button>
            <span className="text-xs text-muted-foreground">
              「测试连接」= 先保存再连一次；连接成功会缓存真实基址，之后提交更快。
            </span>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>直接提交环境变量</CardTitle>
          <CardDescription>
            通用信息提交（DD_WEBHOOK 这类与账号无关的变量）。同名变量已存在时会先列出来让你选
            「覆盖哪一条」还是「追加一条」，绝不悄悄覆盖。
          </CardDescription>
        </CardHeader>
        <CardContent className="grid gap-3">
          <div className="grid gap-2 sm:grid-cols-3">
            <Input
              className="font-mono"
              placeholder="变量名，如 DD_WEBHOOK"
              value={envName}
              onChange={(e) => setEnvName(e.target.value)}
            />
            <Input
              className="font-mono"
              placeholder="变量值"
              value={envValue}
              onChange={(e) => setEnvValue(e.target.value)}
            />
            <Input placeholder="备注（可空）" value={envRemarks} onChange={(e) => setEnvRemarks(e.target.value)} />
          </div>

          {choice ? (
            <div className="grid gap-2 rounded-lg border border-amber-500/50 bg-amber-500/10 px-3 py-2.5 text-sm">
              <div className="font-medium">
                面板里已有 {choice.existing.length} 条 <span className="font-mono">{choice.envName}</span>，选一个动作：
              </div>
              <div className="grid gap-1.5">
                {choice.existing.map((e) => (
                  <div key={e.id} className="flex items-center gap-2">
                    <Button size="sm" variant="outline" disabled={envBusy} onClick={() => void submitEnv("update", e.id)}>
                      覆盖 id={e.id}
                    </Button>
                    <span className="text-xs text-muted-foreground">
                      备注：{e.remarks || "（无）"}｜现有值 {e.valuePreview.len} 字符
                    </span>
                  </div>
                ))}
                <div className="flex items-center gap-2">
                  <Button size="sm" variant="outline" disabled={envBusy} onClick={() => void submitEnv("append")}>
                    追加一条
                  </Button>
                  <Button size="sm" variant="ghost" onClick={() => setChoice(null)}>
                    取消
                  </Button>
                </div>
              </div>
            </div>
          ) : null}

          <div className="flex flex-wrap items-center gap-2">
            <Button size="sm" disabled={envBusy || !cfg.enabled} onClick={() => void submitEnv()}>
              {envBusy ? <Loader2 className="size-4 animate-spin" /> : <Upload className="size-4" />}
              提交到青龙
            </Button>
            {!cfg.enabled ? (
              <span className="text-xs text-muted-foreground">先打开上面的开关并保存，才能提交。</span>
            ) : (
              <span className="text-xs text-muted-foreground">
                提交前请确认变量名拼写：青龙按变量名喂给脚本。
              </span>
            )}
          </div>
          {envValue ? (
            <Button
              size="sm"
              variant="ghost"
              className="w-fit px-2 text-xs"
              onClick={async () => {
                const ok = await copyText(envValue);
                if (ok) toast.success("变量值已复制");
                else toast.error("复制失败");
              }}
            >
              复制变量值
            </Button>
          ) : null}
        </CardContent>
      </Card>
    </div>
  );
}
