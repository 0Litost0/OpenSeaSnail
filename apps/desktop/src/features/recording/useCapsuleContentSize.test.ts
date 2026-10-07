import { describe, expect, it } from "vitest";
import { CAPSULE_NOTICE_HEIGHT, CAPSULE_NOTICE_WIDTH, CAPSULE_MAX_WIDTH, CAPSULE_MIN_WIDTH, measureCapsuleWidth } from "./useCapsuleContentSize";

describe("measureCapsuleWidth", () => {
  it("uses intrinsic scroll width and clamps the result", () => {
    const element = { scrollWidth: CAPSULE_MIN_WIDTH + 20, getBoundingClientRect: () => ({ width: 10 }) } as HTMLElement;
    expect(measureCapsuleWidth(element)).toBe(CAPSULE_MIN_WIDTH + 20);
    expect(measureCapsuleWidth({ ...element, scrollWidth: 1_000 })).toBe(CAPSULE_MAX_WIDTH);
  });

  it("falls back to the narrow safe width when layout is unavailable", () => {
    const element = { scrollWidth: 0, getBoundingClientRect: () => ({ width: 0 }) } as HTMLElement;
    expect(measureCapsuleWidth(element)).toBe(CAPSULE_MIN_WIDTH);
  });

  it("measures intrinsic content instead of retaining the previous wide window", () => {
    const style = { width: "360px" };
    const element = {
      style,
      get scrollWidth() { return style.width === "max-content" ? 180 : 360; },
      getBoundingClientRect: () => ({ width: 180 }),
    } as HTMLElement;
    expect(measureCapsuleWidth(element)).toBe(180);
    expect(style.width).toBe("360px");
    expect([CAPSULE_NOTICE_WIDTH, CAPSULE_NOTICE_HEIGHT]).toEqual([360, 104]);
  });
});
