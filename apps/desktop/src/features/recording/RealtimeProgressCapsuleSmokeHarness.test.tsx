import { act, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { RealtimeTaskSnapshot } from "@/api/transport";
import {
  CAPSULE_SMOKE_STEP_MS,
  capsuleSmokeSequence,
  initialCapsuleSmokeStep,
  RealtimeProgressCapsuleSmokeHarness,
} from "./RealtimeProgressCapsuleSmokeHarness";

const state = vi.hoisted(() => ({
  snapshots: [] as RealtimeTaskSnapshot[],
  resize: vi.fn(async () => undefined),
}));

vi.mock("@/api/transport", () => ({ resizeRealtimeProgressCapsule: state.resize }));
vi.mock("./RealtimeProgressCapsule", () => ({
  RealtimeProgressCapsuleView: ({ snapshot, onResize }: {
    snapshot: RealtimeTaskSnapshot;
    onResize: (width: number, height: number, generation: number, revision: number) => void;
  }) => {
    state.snapshots.push(snapshot);
    onResize(snapshot.phase === "failed" ? 360 : 320, snapshot.phase === "failed" ? 104 : 40, snapshot.generation, snapshot.revision);
    return <section role="status">{snapshot.phase}</section>;
  },
}));

describe("capsule fixture smoke harness", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    state.snapshots = [];
    state.resize.mockClear();
  });

  afterEach(() => {
    vi.useRealTimers();
    window.history.replaceState(null, "", "/");
  });

  it("contains every phase, terminal states and a second task with frozen auto-paste off", () => {
    expect(new Set(capsuleSmokeSequence.map(({ phase }) => phase))).toEqual(new Set([
      "idle", "recording", "submitting", "transcribing", "cleaning_up", "auto_pasting", "completed", "failed",
    ]));
    expect(capsuleSmokeSequence.map(({ revision }) => revision)).toEqual(
      capsuleSmokeSequence.map((_, index) => index + 1),
    );
    expect(capsuleSmokeSequence.some(({ task_id, phase }) => task_id === "smoke-task-1" && phase === "completed")).toBe(true);
    expect(capsuleSmokeSequence.some(({ task_id, phase, auto_paste_enabled }) =>
      task_id === "smoke-task-2" && phase === "recording" && !auto_paste_enabled)).toBe(true);
    expect(capsuleSmokeSequence.some(({ failure_code }) => failure_code === "no_speech_detected")).toBe(true);
    expect(capsuleSmokeSequence.some(({ failure_code }) => failure_code === "recording_no_audio")).toBe(true);
    expect(capsuleSmokeSequence.slice(-2).map(({ failure_code }) => failure_code)).toEqual([
      "recording_auto_paste_failed", "recording_auto_paste_failed",
    ]);
  });

  it("can start at a specific fixture for visual inspection", () => {
    expect(initialCapsuleSmokeStep("?capsule=1&smokeStep=13")).toBe(12);
    expect(initialCapsuleSmokeStep("?smokeStep=999")).toBe(0);
    expect(initialCapsuleSmokeStep("?smokeStep=garbage")).toBe(0);
  });

  it("uses the Chinese page language for the final fixture", () => {
    window.history.replaceState(null, "", "/?smokeStep=17");
    render(<RealtimeProgressCapsuleSmokeHarness />);
    expect(screen.getByRole("main")).toHaveAttribute("data-smoke-step", `17/${capsuleSmokeSequence.length}`);
    expect(document.documentElement.lang).toBe("zh-CN");
  });

  it("advances deterministically and loops without invoking a production task hook", () => {
    render(<RealtimeProgressCapsuleSmokeHarness />);
    expect(screen.getByRole("status")).toHaveTextContent("recording");
    expect(screen.getByRole("main")).toHaveAttribute("data-smoke-step", `1/${capsuleSmokeSequence.length}`);

    act(() => vi.advanceTimersByTime(CAPSULE_SMOKE_STEP_MS));
    expect(screen.getByRole("status")).toHaveTextContent("submitting");

    act(() => vi.advanceTimersByTime(CAPSULE_SMOKE_STEP_MS * capsuleSmokeSequence.length));
    expect(screen.getByRole("status")).toHaveTextContent("submitting");
    expect(state.resize).toHaveBeenCalled();
  });
});
