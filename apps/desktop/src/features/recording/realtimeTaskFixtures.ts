import type { RealtimeTaskSnapshot } from "@/api/transport";

const base: RealtimeTaskSnapshot = {
  generation: 1,
  revision: 1,
  task_id: "task-fixture",
  phase: "idle",
  session_id: null,
  clipboard_context_count: 0,
  auto_paste_enabled: true,
  failure_code: null,
  fallback: "none",
};

export const realtimeTaskFixtures: Record<RealtimeTaskSnapshot["phase"], RealtimeTaskSnapshot> = {
  idle: { ...base, phase: "idle", task_id: null },
  preparing: { ...base, phase: "preparing" },
  recording: { ...base, phase: "recording", clipboard_context_count: 2 },
  submitting: { ...base, phase: "submitting" },
  transcribing: { ...base, phase: "transcribing", session_id: "session-fixture" },
  cleaning_up: { ...base, phase: "cleaning_up", session_id: "session-fixture" },
  auto_pasting: { ...base, phase: "auto_pasting", session_id: "session-fixture" },
  completed: { ...base, phase: "completed", session_id: "session-fixture" },
  failed: { ...base, phase: "failed", session_id: "session-fixture", failure_code: "recording_auto_paste_failed", fallback: "clipboard" },
};
