import { useCallback, useEffect, useState } from "react";

import { GatewayLivePanel } from "@/components/gateway-live-panel";
import { GatewayTabs } from "@/components/gateway-tabs";
import { gatewayStatus } from "@/lib/api";

/**
 * 实时监控（独立页面）。
 *
 * 和「积分共享」分开的理由：那一页是**配置**（开关、Key、账号池、策略），
 * 这一页是**观测**（每秒变化的数字）。混在一起时，配置区被实时刷新带着一起滚、
 * 也没法单独开一页盯着看。
 *
 * 这里只轮询「网关在不在跑」（3 秒一次，轻量）；秒级数字由面板自己拉。
 */
export default function GatewayLivePage() {
  const [running, setRunning] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const s = (await gatewayStatus()) as { running?: boolean };
      setRunning(Boolean(s?.running));
      setErr(null);
    } catch (e) {
      setErr(String(e));
    }
  }, []);

  useEffect(() => {
    void load();
    const id = window.setInterval(() => void load(), 3000);
    return () => window.clearInterval(id);
  }, [load]);

  return (
    <div className="mx-auto flex max-w-5xl flex-col gap-4 px-6 py-6">
      {/* 页头骨架与「积分共享」逐像素一致：标题行 h-8 → 描述 min-h-8（最多两行）→ 分段切换。
          三段的高度都是固定的，所以切换条在两页的 y 坐标相同；改动这里请同步另一页。 */}
      <header className="flex flex-col gap-2">
        <div className="flex h-8 items-center justify-between gap-3">
          <h1 className="truncate text-lg font-semibold leading-8">实时监控</h1>
        </div>

        <p className="line-clamp-2 min-h-8 text-xs text-muted-foreground">
          网关的秒级状态：请求速率、输入 / 输出 token 速率、正在跑的请求、按 Key 的用量与异常。
          数据在内存里统计，只反映<strong className="font-medium">本次启动以来</strong>；历史账本看
          <code className="mx-1 text-[11px]">gateway-metrics.jsonl</code>。
        </p>

        <GatewayTabs />
      </header>

      {err && (
        <p className="rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-xs text-destructive">
          读取网关状态失败：{err}
        </p>
      )}

      <GatewayLivePanel running={running} />
    </div>
  );
}
