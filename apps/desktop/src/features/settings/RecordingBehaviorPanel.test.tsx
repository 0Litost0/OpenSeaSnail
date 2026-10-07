import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { RecordingBehaviorPanel } from "./RecordingBehaviorPanel";

const transport = vi.hoisted(() => ({
  get: vi.fn(async () => ({ auto_paste_enabled: true, keep_transcription_in_clipboard: true })),
  set: vi.fn(async (enabled: boolean) => ({ auto_paste_enabled: enabled, keep_transcription_in_clipboard: true })),
  setKeep: vi.fn(async (enabled: boolean) => ({ auto_paste_enabled: true, keep_transcription_in_clipboard: enabled })),
}));

vi.mock("@/api/transport", () => ({
  recordingBehaviorStatus: transport.get,
  setAutoPasteEnabled: transport.set,
  setKeepTranscriptionInClipboard: transport.setKeep,
}));

vi.mock("@/i18n/I18nProvider", () => ({
  useI18n: () => ({ t: (key: string) => key }),
}));

function renderPanel() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={client}><RecordingBehaviorPanel /></QueryClientProvider>);
}

describe("RecordingBehaviorPanel", () => {
  beforeEach(() => {
    transport.get.mockClear();
    transport.set.mockClear();
  });

  it("loads and persists the automatic-paste preference", async () => {
    renderPanel();
    const checkbox = await screen.findByRole("checkbox", { name: "recording.behavior.auto_paste" });
    expect(checkbox).toBeChecked();
    fireEvent.click(checkbox);
    await waitFor(() => expect(transport.set.mock.calls[0]?.[0]).toBe(false));
    await waitFor(() => expect(checkbox).not.toBeChecked());
  });
});
