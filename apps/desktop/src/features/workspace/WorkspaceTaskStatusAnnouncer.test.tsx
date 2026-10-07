import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { WorkspaceTaskStatusAnnouncer } from "./WorkspaceTaskStatusAnnouncer";
import type { RealtimeTaskSnapshot } from "@/api/transport";

const state = vi.hoisted(() => ({ snapshot: {
  generation: 0,
  revision: 0,
  task_id: null,
  phase: "idle",
  session_id: null,
  clipboard_context_count: 0,
  auto_paste_enabled: false,
  failure_code: null,
  fallback: "none",
} as RealtimeTaskSnapshot }));
vi.mock("@/features/recording/useRealtimeTaskSnapshot", () => ({ useRealtimeTaskSnapshot: () => state.snapshot }));
vi.mock("@/i18n/I18nProvider", () => ({ useI18n: () => ({ t: (key: string) => key }) }));

describe("WorkspaceTaskStatusAnnouncer", () => {
  it("announces verified clipboard fallback failures", () => {
    state.snapshot = { ...state.snapshot, phase: "failed", failure_code: "recording_auto_paste_failed", fallback: "clipboard" };
    render(<WorkspaceTaskStatusAnnouncer />);
    expect(screen.getByRole("status")).toHaveAttribute("aria-live", "polite");
    expect(screen.getByRole("status")).toHaveTextContent("recording.failure_manual_paste");
  });
});
