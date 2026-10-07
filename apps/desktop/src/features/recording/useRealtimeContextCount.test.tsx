import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { RecordingStatus, RealtimeTaskSnapshot } from "@/api/transport";
import { realtimeTaskFixtures } from "./realtimeTaskFixtures";
import { useRealtimeContextCount } from "./useRealtimeContextCount";

const transport = vi.hoisted(() => ({
  handler: undefined as ((status: RecordingStatus) => void) | undefined,
  query: vi.fn(),
  unlisten: vi.fn(),
}));

vi.mock("@/api/transport", async (importOriginal) => {
  const original = await importOriginal<typeof import("@/api/transport")>();
  return {
    ...original,
    onRecordingStatus: vi.fn(async (handler: (status: RecordingStatus) => void) => {
      transport.handler = handler;
      return transport.unlisten;
    }),
    recordingStatus: transport.query,
  };
});

const status = (count: number): RecordingStatus => ({
  is_recording: true,
  elapsed_ms: 0,
  level: 0,
  input_device: null,
  sample_rate: null,
  error: null,
  clipboard_context_count: count,
  clipboard_context_error: null,
});

describe("useRealtimeContextCount", () => {
  beforeEach(() => {
    transport.handler = undefined;
    transport.query.mockReset();
    transport.query.mockResolvedValue(status(2));
    transport.unlisten.mockClear();
  });

  it("uses the live recording count, then freezes the task snapshot count after stop", async () => {
    const recording = { ...realtimeTaskFixtures.recording, task_id: "task-count" };
    const { result, rerender } = renderHook(
      ({ snapshot }: { snapshot: RealtimeTaskSnapshot }) => useRealtimeContextCount(snapshot),
      { initialProps: { snapshot: recording } },
    );

    await waitFor(() => expect(result.current).toBe(2));
    act(() => transport.handler?.(status(4)));
    expect(result.current).toBe(4);

    rerender({ snapshot: { ...realtimeTaskFixtures.submitting, task_id: "task-count", clipboard_context_count: 7 } });
    expect(result.current).toBe(7);
    expect(transport.unlisten).toHaveBeenCalledOnce();
  });

  it("does not let an old query overwrite a newer recording event", async () => {
    let resolveQuery: ((value: RecordingStatus) => void) | undefined;
    transport.query.mockImplementation(() => new Promise((resolve) => { resolveQuery = resolve; }));
    const { result } = renderHook(() => useRealtimeContextCount({ ...realtimeTaskFixtures.recording, task_id: "task-race" }));

    await waitFor(() => expect(transport.handler).toBeTypeOf("function"));
    act(() => transport.handler?.(status(5)));
    expect(result.current).toBe(5);
    act(() => resolveQuery?.(status(1)));
    await waitFor(() => expect(result.current).toBe(5));
  });

  it("shows zero instead of the previous count while a new task initializes", async () => {
    const first = { ...realtimeTaskFixtures.recording, task_id: "task-first" };
    const { result, rerender } = renderHook(
      ({ snapshot }: { snapshot: RealtimeTaskSnapshot }) => useRealtimeContextCount(snapshot),
      { initialProps: { snapshot: first } },
    );
    await waitFor(() => expect(result.current).toBe(2));
    act(() => transport.handler?.(status(6)));
    expect(result.current).toBe(6);

    rerender({ snapshot: { ...realtimeTaskFixtures.recording, task_id: "task-second" } });
    expect(result.current).toBe(0);
    await waitFor(() => expect(result.current).toBe(2));
  });
});
