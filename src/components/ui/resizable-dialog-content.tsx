import { useCallback, useEffect, useRef, useState, type ComponentProps } from "react";

import { DialogContent } from "@/components/ui/dialog";
import {
  STORAGE_PREFIX,
  readStored as readStoredKey,
  writeStored as writeStoredKey,
} from "@/lib/storage-keys";
import { cn } from "@/lib/utils";

/**
 * 可调尺寸的弹窗内容容器。
 *
 * 默认放大到「屏幕宽 90% × 高 86%」——此前的 `sm:max-w-lg` 只有 512px，
 * 会话迁移这类长内容必须不停拖动滚动条。在默认尺寸之外，右下角提供拖拽手柄手动调节，
 * 尺寸按 `storageKey` 记在 localStorage 里，下次打开沿用。
 *
 * **锚点**：内容按「顶部固定、向右下生长」放置。默认屏幕居中，拖大时只往下往右长，
 * 手柄始终跟手；若用默认的 `place-items-center` 居中锚定，拖大时四条边同时外扩，
 * 手柄会以两倍速度跑掉。
 */

const STORE_PREFIX = `${STORAGE_PREFIX}:dialog-size:`;
/** 与窗口边缘的最小留白。 */
const GAP = 16;

type Size = { w: number; h: number };

function defaultSize(): Size {
  if (typeof window === "undefined") return { w: 960, h: 720 };
  return {
    w: Math.round(window.innerWidth * 0.9),
    h: Math.round(window.innerHeight * 0.86),
  };
}

function readStored(key?: string): Size | null {
  if (!key || typeof window === "undefined") return null;
  // 走 storage-keys：产品改名后键前缀变了，这里会自动回退读旧前缀，
  // 否则用户升级后会发现「弹窗尺寸全被重置了」。
  const raw = readStoredKey(STORE_PREFIX + key);
  if (!raw) return null;
  try {
    const v = JSON.parse(raw) as Partial<Size>;
    if (typeof v?.w === "number" && typeof v?.h === "number" && v.w > 0 && v.h > 0) {
      return { w: v.w, h: v.h };
    }
  } catch {
    /* 存储损坏时退回默认尺寸，不打断弹窗打开 */
  }
  return null;
}

function writeStored(key: string | undefined, size: Size) {
  if (!key || typeof window === "undefined") return;
  writeStoredKey(STORE_PREFIX + key, JSON.stringify(size));
}

export function ResizableDialogContent({
  storageKey,
  minWidth = 420,
  minHeight = 300,
  className,
  children,
  ...props
}: ComponentProps<typeof DialogContent> & {
  /** localStorage 里的键后缀；同一个弹窗固定用一个，不同弹窗互不干扰。 */
  storageKey?: string;
  minWidth?: number;
  minHeight?: number;
}) {
  const [size, setSize] = useState<Size>(() => readStored(storageKey) ?? defaultSize());
  // 顶部锚点（px）。默认垂直居中，拖大时保持不变 —— 于是内容只往右下长。
  const [top, setTop] = useState(() => {
    const { h } = readStored(storageKey) ?? defaultSize();
    if (typeof window === "undefined") return GAP;
    return Math.max(GAP, Math.round((window.innerHeight - h) / 2));
  });

  const drag = useRef<{ x: number; y: number; w: number; h: number } | null>(null);

  /** 把尺寸收敛到「当前窗口装得下」的范围内。 */
  const clamp = useCallback(
    (next: Size, anchorTop: number): Size => {
      if (typeof window === "undefined") return next;
      const maxW = Math.max(320, window.innerWidth - GAP * 2);
      const maxH = Math.max(240, window.innerHeight - anchorTop - GAP);
      return {
        w: Math.min(Math.max(next.w, minWidth), maxW),
        h: Math.min(Math.max(next.h, minHeight), maxH),
      };
    },
    [minWidth, minHeight],
  );

  // 窗口变化时重新收敛（否则缩小窗口后手柄会跑到屏幕外）。
  useEffect(() => {
    const onResize = () => {
      setTop((prevTop) => {
        const nextTop = Math.min(prevTop, Math.max(GAP, window.innerHeight - minHeight - GAP));
        setSize((s) => clamp(s, nextTop));
        return nextTop;
      });
    };
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, [clamp, minHeight]);

  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    e.preventDefault();
    e.stopPropagation();
    (e.target as HTMLElement).setPointerCapture?.(e.pointerId);
    drag.current = { x: e.clientX, y: e.clientY, w: size.w, h: size.h };
  };

  const onPointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    if (!d) return;
    e.preventDefault();
    setSize(clamp({ w: d.w + (e.clientX - d.x), h: d.h + (e.clientY - d.y) }, top));
  };

  const endDrag = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!drag.current) return;
    drag.current = null;
    (e.target as HTMLElement).releasePointerCapture?.(e.pointerId);
    writeStored(storageKey, size);
  };

  /** 双击手柄恢复默认尺寸与居中位置。 */
  const reset = () => {
    const next = defaultSize();
    const nextTop = Math.max(GAP, Math.round((window.innerHeight - next.h) / 2));
    setTop(nextTop);
    setSize(clamp(next, nextTop));
    writeStored(storageKey, next);
  };

  return (
    <DialogContent
      // 内联尺寸压过基类的 max-h-[85vh] / max-w-[calc(100%-2rem)] / sm:max-w-lg。
      style={{ width: size.w, height: size.h, maxWidth: "none", maxHeight: "none" }}
      layoutStyle={{ alignItems: "flex-start", justifyItems: "center", paddingTop: top }}
      className={cn("grid-rows-[auto_minmax(0,1fr)_auto] overflow-hidden", className)}
      {...props}
    >
      {children}
      <div
        data-slot="dialog-resize-handle"
        role="separator"
        aria-label="拖动调整弹窗大小，双击恢复默认"
        title="拖动调整大小（双击恢复默认）"
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={endDrag}
        onPointerCancel={endDrag}
        onDoubleClick={reset}
        className="absolute right-0 bottom-0 z-10 size-4 cursor-nwse-resize touch-none select-none rounded-br-lg"
        style={{
          // 两道对角斜线，和窗口右下角的手柄观感一致
          backgroundImage:
            "linear-gradient(135deg, transparent 0 45%, var(--border) 45% 55%, transparent 55% 100%)",
        }}
      />
    </DialogContent>
  );
}
