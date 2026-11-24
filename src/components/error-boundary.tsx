import { Component, type ErrorInfo, type ReactNode } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import { AlertTriangle, Home, RotateCw } from "lucide-react";

import { Button } from "@/components/ui/button";

/**
 * 页面级错误边界。
 *
 * ## 为什么必须有（issue #26）
 *
 * 用户反馈：`/workbuddy-cleanup` 抛 `TypeError: Cannot read properties of undefined`
 * 之后**整个界面白屏，连首页都点不回去，只能退出重开**。
 *
 * 一个页面的字段缺失不该让整个应用失去导航能力 —— 错误边界把「页面崩了」限制在
 * 内容区，并给出两条出路：回首页 / 重新加载。
 *
 * ## 为什么 key 用路由路径
 *
 * 边界一旦捕获错误会停在错误态；换路由时必须**重新挂载**，否则用户点侧栏
 * 切到别的页面还是这块错误屏（那和「只能退出重开」没区别）。
 */
class Boundary extends Component<
  { children: ReactNode; onHome: () => void; path: string },
  { error: Error | null }
> {
  state = { error: null as Error | null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    // 只打日志，不上报：本工具不联网收集数据。
    console.error("[ui] 页面渲染失败", error, info.componentStack);
  }

  render() {
    if (!this.state.error) return this.props.children;
    return (
      <div className="mx-auto flex min-h-[60vh] w-full max-w-2xl flex-col items-center justify-center gap-3 px-6 py-10 text-center">
        <AlertTriangle className="size-6 text-amber-600" />
        <h2 className="text-sm font-medium">这个页面出错了</h2>
        <p className="max-w-lg break-words text-xs text-muted-foreground">
          {this.state.error.message || String(this.state.error)}
        </p>
        <p className="text-[11px] text-muted-foreground/80">
          路径：{this.props.path}
          <br />
          其它页面不受影响，可以直接切过去；也可以回首页重新进入。
        </p>
        <div className="mt-1 flex items-center gap-2">
          <Button size="sm" onClick={() => this.props.onHome()}>
            <Home className="size-4" />
            回首页
          </Button>
          <Button size="sm" variant="outline" onClick={() => window.location.reload()}>
            <RotateCw className="size-4" />
            重新加载
          </Button>
        </div>
      </div>
    );
  }
}

/** 包在 `<Routes>` 外层用；`key` 跟着路由走，切页自动复位。 */
export function AppErrorBoundary({ children }: { children: ReactNode }) {
  const navigate = useNavigate();
  const location = useLocation();
  return (
    <Boundary
      key={location.pathname}
      path={location.pathname}
      onHome={() => navigate("/")}
    >
      {children}
    </Boundary>
  );
}
