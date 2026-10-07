import { act, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { measureElementBorderBox, resolveWideLayout, useElementWidth } from "./use-element-width";

type ObserverRecord = {
  callback: ResizeObserverCallback;
  disconnect: () => void;
  observe: (target: Element, options?: ResizeObserverOptions) => void;
};

function installResizeObserver(): ObserverRecord[] {
  const records: ObserverRecord[] = [];
  vi.stubGlobal("ResizeObserver", class {
    private readonly record: ObserverRecord;
    constructor(callback: ResizeObserverCallback) {
      this.record = { callback, disconnect: vi.fn(), observe: vi.fn() };
      records.push(this.record);
    }
    observe(target: Element, options?: ResizeObserverOptions) { this.record.observe(target, options); }
    unobserve() {}
    disconnect() { this.record.disconnect(); }
  });
  return records;
}

function setBorderBoxWidth(element: Element, width: number) {
  vi.spyOn(element, "getBoundingClientRect").mockReturnValue({ width } as DOMRect);
}

function notify(record: ObserverRecord, element: Element, contentWidth: number) {
  record.callback(
    [{ target: element, contentRect: { width: contentWidth } } as unknown as ResizeObserverEntry],
    {} as ResizeObserver,
  );
}

describe("useElementWidth", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("uses border-box consistently for initial and observed measurements", () => {
    const records = installResizeObserver();
    const element = document.createElement("div");
    setBorderBoxWidth(element, 640);
    const hook = renderHook(() => useElementWidth<HTMLDivElement>());
    act(() => hook.result.current.ref(element));
    expect(hook.result.current.width).toBe(640);
    expect(records[0]?.observe).toHaveBeenCalledWith(element, { box: "border-box" });

    setBorderBoxWidth(element, 712);
    act(() => notify(records[0]!, element, 680));
    expect(hook.result.current.width).toBe(712);
    expect(measureElementBorderBox(element)).toBe(712);
  });

  it("disconnects and re-subscribes when the callback ref receives a new node", () => {
    const records = installResizeObserver();
    const first = document.createElement("div");
    const second = document.createElement("div");
    setBorderBoxWidth(first, 640);
    setBorderBoxWidth(second, 480);
    const hook = renderHook(() => useElementWidth<HTMLDivElement>());
    act(() => hook.result.current.ref(first));
    expect(hook.result.current.width).toBe(640);

    act(() => hook.result.current.ref(second));
    expect(records[0]?.disconnect).toHaveBeenCalledOnce();
    expect(records[1]?.observe).toHaveBeenCalledWith(second, { box: "border-box" });
    expect(hook.result.current.width).toBe(480);

    setBorderBoxWidth(first, 900);
    act(() => notify(records[0]!, first, 900));
    expect(hook.result.current.width).toBe(480);
    hook.unmount();
    expect(records[1]?.disconnect).toHaveBeenCalledOnce();
  });

  it("returns null when ResizeObserver is unavailable", () => {
    vi.stubGlobal("ResizeObserver", undefined);
    const element = document.createElement("div");
    setBorderBoxWidth(element, 640);
    const hook = renderHook(() => useElementWidth<HTMLDivElement>());
    act(() => hook.result.current.ref(element));
    expect(hook.result.current.width).toBeNull();
  });
});

describe("resolveWideLayout", () => {
  it("uses hysteresis at the boundary and treats unknown width as narrow", () => {
    expect(resolveWideLayout(null, true, 700)).toBe(false);
    expect(resolveWideLayout(690, false, 700)).toBe(false);
    expect(resolveWideLayout(700, false, 700)).toBe(true);
    expect(resolveWideLayout(685, true, 700)).toBe(true);
    expect(resolveWideLayout(675, true, 700)).toBe(false);
  });
});
