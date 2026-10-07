import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { RealtimeTaskSnapshot } from "@/api/transport";
import { failedAt, failureLabelKey, isHintLevelFailure, RealtimeProgressCapsule, segmentValue, stagesFor } from "./RealtimeProgressCapsule";
import { realtimeTaskFixtures } from "./realtimeTaskFixtures";

const state = vi.hoisted(() => ({ snapshot: {} as RealtimeTaskSnapshot }));

vi.mock("./useRealtimeTaskSnapshot", () => ({ useRealtimeTaskSnapshot: () => state.snapshot }));
vi.mock("./useRealtimeContextCount", () => ({ useRealtimeContextCount: (snapshot: RealtimeTaskSnapshot) => snapshot.clipboard_context_count }));
vi.mock("@/i18n/I18nProvider", () => ({ useI18n: () => ({ t: (key: string, variables?: { count?: number }) => ({ "recording.task_completed": "completed", "recording.notice_no_speech": "No speech detected. Try recording again when you're ready.", "recording.failure_retry": "Processing failed. Please try recording again.", "recording.failure_microphone_unavailable": "No available microphone was detected. Connect a microphone and try again.", "recording.failure_manual_paste": "Automatic paste failed. Press Command+V to paste the text from the clipboard.", "recording.failure_accessibility_clipboard": "Allow Accessibility access, or press Command+V to paste the text from the clipboard.", "recording.failure_copy_history": "Automatic paste failed. Copy the transcript from Sessions.", "recording.failure_history_wait": "The local service is unavailable. The session remains available in Sessions.", "recording.failure_history_status": "Processing stopped. Check the latest session status in Sessions.", "recording.idle": "ready", "recording.phase_recording": "recording label", "recording.phase_submitting": "submitting label", "recording.phase_transcribing": "transcribing label", "recording.phase_cleaning_up": "cleaning label up to 15 seconds", "recording.phase_auto_pasting": "auto paste label", "recording.context_count": `context ${variables?.count ?? 0}` }[key] ?? key) }) }));
vi.mock("@/components/ui/progress", () => ({ Progress: ({ value, ...props }: { value?: number; "data-stage"?: string }) => <div {...props} data-progress={value == null ? "current" : value} /> }));

describe("RealtimeProgressCapsule", () => {
  beforeEach(() => { state.snapshot = { ...realtimeTaskFixtures.transcribing }; });

  it("shows preparation without claiming audio capture is ready", () => {
    state.snapshot = { ...realtimeTaskFixtures.preparing };
    render(<RealtimeProgressCapsule />);
    expect(screen.getByRole("status")).toHaveAttribute("aria-label", "recording.phase_preparing");
    expect(screen.getByText("recording.phase_preparing")).toBeInTheDocument();
  });
  it("keeps preparation and recording at the same normal width", () => {
    const onSize = vi.fn();
    state.snapshot = { ...realtimeTaskFixtures.preparing };
    const view = render(<RealtimeProgressCapsule onSize={onSize} />);
    expect(onSize).toHaveBeenLastCalledWith(260, 40);
    state.snapshot = { ...realtimeTaskFixtures.recording, revision: 2 };
    view.rerender(<RealtimeProgressCapsule onSize={onSize} />);
    expect(onSize).toHaveBeenLastCalledWith(260, 40);
    expect(view.container.querySelector("[data-capsule-pill]")).toHaveStyle({ width: "260px" });
  });

  it("renders a five-stage track without visible phase text", () => {
    const { container } = render(<RealtimeProgressCapsule />);
    expect(container.querySelectorAll("[data-stage]")).toHaveLength(5);
    expect(screen.getByRole("status")).toHaveAttribute("aria-label", "transcribing label");
    expect(screen.getByRole("status")).toHaveAttribute("aria-live", "off");
    expect(screen.queryByText("transcribing label")).not.toBeInTheDocument();
  });

  it("uses four stages when automatic paste was frozen off", () => {
    state.snapshot = { ...realtimeTaskFixtures.submitting, auto_paste_enabled: false };
    const { container } = render(<RealtimeProgressCapsule />);
    expect(container.querySelectorAll("[data-stage]")).toHaveLength(4);
  });

  it("announces cleaning up as a bounded display-only progress phase", () => {
    state.snapshot = { ...realtimeTaskFixtures.cleaning_up };
    const { container } = render(<RealtimeProgressCapsule />);
    expect(screen.getByRole("status")).toHaveAttribute("aria-label", "cleaning label up to 15 seconds");
    expect(container.querySelector('[data-stage="cleaning_up"]')).toHaveAttribute("data-progress", "current");
  });

  it("always shows paperclip count including zero", () => {
    state.snapshot = { ...realtimeTaskFixtures.submitting, clipboard_context_count: 0 };
    const view = render(<RealtimeProgressCapsule />);
    expect(view.container.querySelector('[aria-label="context 0"]')).toHaveTextContent("0");
  });

  it("keeps the full clipboard fallback in the card's accessible label", () => {
    state.snapshot = { ...realtimeTaskFixtures.failed };
    const view = render(<RealtimeProgressCapsule />);
    const message = view.getByText("Automatic paste failed. Press Command+V to paste the text from the clipboard.");
    expect(message).toHaveClass("line-clamp-2");
    expect(view.getByRole("status")).toHaveAttribute("aria-label", message.textContent);
    expect(view.getByRole("status")).toHaveAttribute("aria-live", "polite");
    expect(view.container.querySelector("[data-capsule-notice]")).toHaveClass("h-14", "w-80");
    expect(view.container.querySelector("[data-capsule-tail]")).toBeInTheDocument();
    expect(view.container.querySelector("[data-capsule-pill]")).toHaveClass("h-10");
  });

  it("renders the complete missing microphone prompt and remeasures repeated failures", () => {
    const onSize = vi.fn();
    state.snapshot = {
      ...realtimeTaskFixtures.failed,
      revision: 2,
      task_id: "missing-microphone-1",
      session_id: null,
      failure_code: "recording_microphone_unavailable",
      fallback: "none",
    };
    const view = render(<RealtimeProgressCapsule onSize={onSize} />);
    const message = view.getByText("No available microphone was detected. Connect a microphone and try again.");
    expect(message).toHaveClass("line-clamp-2");
    expect(view.getByRole("status")).toHaveAttribute("aria-label", message.textContent);
    expect(view.getByRole("status")).toHaveAttribute("aria-live", "polite");
    expect(onSize).toHaveBeenLastCalledWith(360, 104);

    state.snapshot = {
      ...state.snapshot,
      revision: 4,
      task_id: "missing-microphone-2",
    };
    view.rerender(<RealtimeProgressCapsule onSize={onSize} />);
    expect(onSize).toHaveBeenCalledTimes(2);
    expect(onSize).toHaveBeenLastCalledWith(360, 104);
  });

  it.each(["no_speech_detected", "recording_no_audio"])("shows %s as a neutral hint", (failure_code) => {
    state.snapshot = { ...realtimeTaskFixtures.failed, failure_code, fallback: "none" };
    const onSize = vi.fn();
    const view = render(<RealtimeProgressCapsule onSize={onSize} />);
    expect(view.getByRole("status")).toHaveAttribute("aria-label", "No speech detected. Try recording again when you're ready.");
    expect(view.getByRole("status")).toHaveAttribute("data-slot", "alert");
    expect(view.getByRole("status")).toHaveClass("bg-card");
    expect(view.container.querySelector("section")).not.toHaveClass("bg-background");
    expect(view.container.querySelector("[data-capsule-pill]")).not.toHaveClass("text-destructive");
    expect(view.container.querySelector(`[data-stage="${failure_code === "recording_no_audio" ? "submitting" : "transcribing"}"]`)).toHaveAttribute("data-progress", "50");
    expect(onSize).toHaveBeenLastCalledWith(360, 104);
  });

  it("returns to a 40px pill immediately when a new task starts", () => {
    state.snapshot = { ...realtimeTaskFixtures.failed, failure_code: "no_speech_detected", fallback: "none" };
    const onSize = vi.fn();
    const view = render(<RealtimeProgressCapsule onSize={onSize} />);
    state.snapshot = { ...realtimeTaskFixtures.recording, task_id: "new-task", revision: 2 };
    view.rerender(<RealtimeProgressCapsule onSize={onSize} />);
    expect(view.container.querySelector("[data-capsule-notice]")).not.toBeInTheDocument();
    expect(view.container.querySelector("[data-capsule-pill]")).toHaveClass("h-10");
    expect(onSize).toHaveBeenLastCalledWith(260, 40);
  });

  it("renders every active and terminal fixture while keeping idle visually hidden", () => {
    for (const [phase, snapshot] of Object.entries(realtimeTaskFixtures)) {
      state.snapshot = { ...snapshot };
      const view = render(<RealtimeProgressCapsule />);
      expect(view.container.querySelector('[role="status"]') !== null).toBe(phase !== "idle");
      view.unmount();
    }
  });
});

describe("segmented task track", () => {
  it("marks previous, current and future stages independently", () => {
    const stages = stagesFor(true);
    expect(stages.map(({ phase }) => segmentValue(realtimeTaskFixtures.transcribing, phase, stages))).toEqual([100, 100, undefined, 0, 0]);
    expect(stages.map(({ phase }) => segmentValue(realtimeTaskFixtures.cleaning_up, phase, stages))).toEqual([100, 100, 100, undefined, 0]);
  });

  it("maps stable failures explicitly, using session presence only for connection scope", () => {
    expect(failedAt({ ...realtimeTaskFixtures.failed, failure_code: "recording_device_error", session_id: null })).toBe("recording");
    expect(failedAt({ ...realtimeTaskFixtures.failed, failure_code: "recording_submit_failed", session_id: null })).toBe("submitting");
    expect(failedAt({ ...realtimeTaskFixtures.failed, failure_code: "connection", session_id: null })).toBe("submitting");
    expect(failedAt({ ...realtimeTaskFixtures.failed, failure_code: "connection", session_id: "session" })).toBe("transcribing");
    expect(failedAt({ ...realtimeTaskFixtures.failed, failure_code: "recording_auto_paste_failed" })).toBe("auto_pasting");
    expect(failedAt({ ...realtimeTaskFixtures.failed, failure_code: "recording_stale_worker", fallback: "history" })).toBe("auto_pasting");
    expect(failedAt({ ...realtimeTaskFixtures.failed, failure_code: "recording_status_failed" })).toBe("transcribing");
    expect(failedAt({ ...realtimeTaskFixtures.failed, failure_code: "recording_status_invalid" })).toBe("transcribing");
  });

  it("chooses an actionable label from verified fallback facts", () => {
    expect(failureLabelKey({ ...realtimeTaskFixtures.failed, failure_code: "recording_microphone_unavailable", fallback: "none" })).toBe("recording.failure_microphone_unavailable");
    expect(failureLabelKey({ ...realtimeTaskFixtures.failed, fallback: "clipboard" })).toBe("recording.failure_manual_paste");
    expect(failureLabelKey({ ...realtimeTaskFixtures.failed, failure_code: "recording_accessibility_required", fallback: "clipboard" })).toBe("recording.failure_accessibility_clipboard");
    expect(failureLabelKey({ ...realtimeTaskFixtures.failed, fallback: "history" })).toBe("recording.failure_copy_history");
    expect(failureLabelKey({ ...realtimeTaskFixtures.failed, fallback: "none" })).toBe("recording.failure_retry");
    expect(failureLabelKey({ ...realtimeTaskFixtures.failed, failure_code: "connection", fallback: "history" })).toBe("recording.failure_history_wait");
    expect(failureLabelKey({ ...realtimeTaskFixtures.failed, failure_code: "recording_cancelled", fallback: "history" })).toBe("recording.failure_history_status");
    expect(failureLabelKey({ ...realtimeTaskFixtures.failed, failure_code: "no_speech_detected", fallback: "none" })).toBe("recording.notice_no_speech");
    expect(isHintLevelFailure({ ...realtimeTaskFixtures.failed, failure_code: "recording_no_audio" })).toBe(true);
    expect(isHintLevelFailure({ ...realtimeTaskFixtures.failed, failure_code: "recording_transcription_failed" })).toBe(false);
  });
});
