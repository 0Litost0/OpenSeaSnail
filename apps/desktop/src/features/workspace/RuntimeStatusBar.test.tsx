import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { probeDaemonStatus, RuntimeStatusBar } from "./RuntimeStatusBar";

const transport = vi.hoisted(() => ({
  daemonStatus: vi.fn(),
  permissionStatus: vi.fn(),
  recordingStatus: vi.fn(),
  onRecordingStatus: vi.fn(),
}));
vi.mock("@/api/transport", () => transport);
vi.mock("@/i18n/I18nProvider", () => ({ useI18n: () => ({ t: (key: string) => key }) }));

function renderBar() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={client}><RuntimeStatusBar /></QueryClientProvider>);
}

describe("RuntimeStatusBar", () => {
  afterEach(() => { cleanup(); vi.clearAllMocks(); });

  it("shows only ready/recording and daemon availability", async () => {
    transport.daemonStatus.mockResolvedValue({ connected: true, authenticated: true });
    transport.permissionStatus.mockResolvedValue({ microphone: { granted: true, status: "authorized" }, accessibility_granted: true });
    transport.recordingStatus.mockResolvedValue({ is_recording: false });
    transport.onRecordingStatus.mockResolvedValue(() => {});
    renderBar();
    await waitFor(() => expect(screen.getByText("runtime.connected")).toBeInTheDocument());
    expect(screen.getByText("runtime.ready")).toBeInTheDocument();
    expect(screen.queryByText(/transcrib|paste|submit/i)).not.toBeInTheDocument();
  });

  it("refreshes daemon status when the document becomes visible", async () => {
    transport.daemonStatus.mockResolvedValue({ connected: true, authenticated: true });
    transport.permissionStatus.mockResolvedValue({ microphone: { granted: true, status: "authorized" }, accessibility_granted: true });
    transport.recordingStatus.mockResolvedValue({ is_recording: true });
    transport.onRecordingStatus.mockResolvedValue(() => {});
    renderBar();
    await waitFor(() => expect(transport.daemonStatus).toHaveBeenCalledTimes(1));
    Object.defineProperty(document, "visibilityState", { configurable: true, value: "visible" });
    document.dispatchEvent(new Event("visibilitychange"));
    await waitFor(() => expect(transport.daemonStatus).toHaveBeenCalledTimes(2));
    expect(screen.getByText("runtime.recording")).toBeInTheDocument();
  });

  it("rejects an overdue daemon probe after 250ms", async () => {
    vi.useFakeTimers();
    transport.daemonStatus.mockReturnValue(new Promise(() => {}));
    const pending = probeDaemonStatus();
    const assertion = expect(pending).rejects.toThrow("daemon probe timeout");
    await vi.advanceTimersByTimeAsync(250);
    await assertion;
    vi.useRealTimers();
  });

  it("polls every two seconds and stops polling after unmount", async () => {
    vi.useFakeTimers();
    transport.daemonStatus.mockResolvedValue({ connected: true, authenticated: true });
    transport.recordingStatus.mockResolvedValue({ is_recording: false });
    transport.onRecordingStatus.mockResolvedValue(() => {});
    const view = renderBar();
    await act(async () => { await vi.advanceTimersByTimeAsync(2_000); });
    expect(transport.daemonStatus.mock.calls.length).toBeGreaterThanOrEqual(2);
    const calls = transport.daemonStatus.mock.calls.length;
    view.unmount();
    await act(async () => { await vi.advanceTimersByTimeAsync(4_000); });
    expect(transport.daemonStatus).toHaveBeenCalledTimes(calls);
    vi.useRealTimers();
  });
});
