import { renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { acceptSnapshot, idleRealtimeTaskSnapshot, useRealtimeTaskSnapshot } from "./useRealtimeTaskSnapshot";
import type { RealtimeTaskSnapshot } from "@/api/transport";

const transport = vi.hoisted(() => ({
  listen: vi.fn(),
  query: vi.fn(),
  order: [] as string[],
  handler: undefined as ((snapshot: RealtimeTaskSnapshot) => void) | undefined,
}));

vi.mock("@/api/transport", () => ({
  onRealtimeTaskSnapshot: transport.listen,
  realtimeTaskSnapshot: transport.query,
}));

const snapshot = (revision: number, phase: RealtimeTaskSnapshot["phase"] = "recording"): RealtimeTaskSnapshot => ({
  ...idleRealtimeTaskSnapshot,
  revision,
  phase,
  task_id: revision ? `task-${revision}` : null,
});

describe("useRealtimeTaskSnapshot", () => {
  beforeEach(() => {
    transport.order = [];
    transport.handler = undefined;
    transport.listen.mockReset();
    transport.query.mockReset();
    transport.listen.mockImplementation(async (handler: (snapshot: RealtimeTaskSnapshot) => void) => {
      transport.order.push("listen");
      transport.handler = handler;
      return () => transport.order.push("unlisten");
    });
    transport.query.mockImplementation(async () => {
      transport.order.push("query");
      return snapshot(1, "submitting");
    });
  });

  it("registers listener before querying initial snapshot", async () => {
    const { result } = renderHook(() => useRealtimeTaskSnapshot());
    await waitFor(() => expect(result.current.revision).toBe(1));
    expect(transport.order.slice(0, 2)).toEqual(["listen", "query"]);
  });

  it("cleans the listener on unmount", async () => {
    const hook = renderHook(() => useRealtimeTaskSnapshot());
    await waitFor(() => expect(transport.order).toContain("listen"));
    hook.unmount();
    expect(transport.order).toContain("unlisten");
  });

  it("does not let a queried revision zero overwrite an event revision zero", async () => {
    let resolveQuery: ((value: RealtimeTaskSnapshot) => void) | undefined;
    transport.query.mockImplementation(() => new Promise<RealtimeTaskSnapshot>((resolve) => { resolveQuery = resolve; }));
    const { result } = renderHook(() => useRealtimeTaskSnapshot());
    await waitFor(() => expect(transport.handler).toBeTypeOf("function"));
    transport.handler?.(snapshot(0, "recording"));
    resolveQuery?.(snapshot(0, "idle"));
    await waitFor(() => expect(result.current.phase).toBe("recording"));
  });

  it("keeps a safe idle snapshot when the initial query fails", async () => {
    transport.query.mockRejectedValue(new Error("unavailable"));
    const { result } = renderHook(() => useRealtimeTaskSnapshot());
    await waitFor(() => expect(transport.query).toHaveBeenCalledOnce());
    expect(result.current).toEqual(idleRealtimeTaskSnapshot);
  });
});

describe("acceptSnapshot", () => {
  it("accepts revision zero once and rejects duplicates or stale events", () => {
    const initial = idleRealtimeTaskSnapshot;
    expect(acceptSnapshot(initial, { ...initial, phase: "failed" }, false).phase).toBe("failed");
    expect(acceptSnapshot(initial, { ...initial, phase: "failed" }, true)).toBe(initial);
    const current = snapshot(3);
    expect(acceptSnapshot(current, snapshot(2))).toBe(current);
    expect(acceptSnapshot(current, snapshot(3))).toBe(current);
    expect(acceptSnapshot(current, snapshot(4)).revision).toBe(4);
  });

  it("rejects an old generation even when its revision is larger", () => {
    const current = { ...snapshot(2), generation: 2 };
    const stale = { ...snapshot(99), generation: 1 };
    expect(acceptSnapshot(current, stale)).toBe(current);
    const replacement = { ...snapshot(1), generation: 3 };
    expect(acceptSnapshot(current, replacement)).toBe(replacement);
  });
});
