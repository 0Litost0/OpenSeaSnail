import { useLayoutEffect, type RefObject } from "react";

export const CAPSULE_MIN_WIDTH = 120;
export const CAPSULE_MAX_WIDTH = 360;
export const CAPSULE_HEIGHT = 40;
export const CAPSULE_NORMAL_WIDTH = 260;
export const CAPSULE_NOTICE_WIDTH = 360;
export const CAPSULE_NOTICE_HEIGHT = 104;

export function measureCapsuleWidth(element: HTMLElement): number {
  const previousWidth = element.style?.width;
  if (element.style) element.style.width = "max-content";
  const intrinsic = Math.max(element.scrollWidth, Math.round(element.getBoundingClientRect().width));
  if (element.style) element.style.width = previousWidth ?? "";
  return Math.max(CAPSULE_MIN_WIDTH, Math.min(CAPSULE_MAX_WIDTH, intrinsic || CAPSULE_MIN_WIDTH));
}

export function useCapsuleContentSize(
  ref: RefObject<HTMLElement | null>,
  onSize: (width: number, height: number) => void,
  dependencies: readonly unknown[] = [],
  height = CAPSULE_HEIGHT,
) {
  useLayoutEffect(() => {
    const element = ref.current;
    if (!element) return;
    const apply = () => onSize(measureCapsuleWidth(element), height);
    apply();
    // 原生窗口尺寸由 onSize 回写；继续观察自身尺寸会形成
    // “窗口调整 → WebView 重排 → ResizeObserver → 再调整”的反馈环，
    // 在失败态尤其容易让多个异步 resize 命令交错，产生重叠视觉。
    // 内容变化已由 dependencies 驱动，因此此处不再观察自身尺寸。
    // The caller controls measurement when intrinsic content changes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [...dependencies, height]);
}
