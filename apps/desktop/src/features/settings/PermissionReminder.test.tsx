import { act, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { PermissionReminder } from "./PermissionReminder";

const transport = vi.hoisted(() => ({
  errorHandler: undefined as ((code: string) => void) | undefined,
  requestMicrophonePermission: vi.fn(),
}));

vi.mock("@/api/transport", () => ({
  onRecordingError: async (handler: (code: string) => void) => {
    transport.errorHandler = handler;
    return () => {};
  },
  requestMicrophonePermission: transport.requestMicrophonePermission,
}));

vi.mock("@/i18n/I18nProvider", () => ({
  useI18n: () => ({
    t: (key: string) => ({
      "reminder.microphone_unavailable_title": "No microphone detected",
      "reminder.microphone_unavailable_description": "Connect a microphone, then use the shortcut again.",
      "reminder.title": "Microphone permission required",
      "reminder.description": "Grant microphone permission.",
      "reminder.open_settings": "Open settings",
      "reminder.later": "Later",
      "permission.request": "Request access",
      "onboarding.microphone.requesting": "Requesting",
    }[key] ?? key),
  }),
}));

describe("PermissionReminder", () => {
  it("shows a connection prompt without permission actions when no microphone exists", async () => {
    render(<PermissionReminder />);
    await act(async () => {
      transport.errorHandler?.("recording_microphone_unavailable");
    });

    expect(screen.getByText("No microphone detected")).toBeInTheDocument();
    expect(screen.getByText("Connect a microphone, then use the shortcut again.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Request access" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Open settings" })).not.toBeInTheDocument();
  });

  it("keeps permission recovery actions for an authorization failure", async () => {
    render(<PermissionReminder />);
    await act(async () => {
      transport.errorHandler?.("recording_microphone_unauthorized");
    });

    expect(screen.getByText("Microphone permission required")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Request access" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Open settings" })).toBeInTheDocument();
  });
});
