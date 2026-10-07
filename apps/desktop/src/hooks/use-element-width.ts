import { useCallback, useLayoutEffect, useState } from "react";

export interface ElementWidthBinding<T extends Element> {
  ref: (element: T | null) => void;
  width: number | null;
}

/** 始终读取 border-box；ResizeObserver 只负责触发测量，避免盒模型混用。 */
export function measureElementBorderBox(element: Element): number {
  return Math.max(0, Math.round(element.getBoundingClientRect().width));
}

/** callback ref 使 DOM 节点替换触发重新订阅；Observer 不可用时返回 null。 */
export function useElementWidth<T extends Element = HTMLElement>(): ElementWidthBinding<T> {
  const [element, setElement] = useState<T | null>(null);
  const [width, setWidth] = useState<number | null>(null);
  const ref = useCallback((next: T | null) => setElement(next), []);

  useLayoutEffect(() => {
    setWidth(null);
    if (!element || typeof ResizeObserver === "undefined") return;

    let active = true;
    const update = () => {
      if (active) setWidth(measureElementBorderBox(element));
    };
    update();
    const observer = new ResizeObserver((entries) => {
      if (entries.some((entry) => entry.target === element)) update();
    });
    observer.observe(element, { box: "border-box" });
    return () => {
      active = false;
      observer.disconnect();
    };
  }, [element]);

  return { ref, width };
}

/** 用不同的进入/退出阈值避免侧栏或双栏在边界宽度反复振荡。 */
export function resolveWideLayout(
  width: number | null,
  wasWide: boolean,
  enterWidth: number,
  exitWidth = enterWidth - 24,
): boolean {
  if (width === null) return false;
  return wasWide ? width >= exitWidth : width >= enterWidth;
}
