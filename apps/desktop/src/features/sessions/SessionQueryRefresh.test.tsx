import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { SessionQueryRefresh } from "./SessionQueryRefresh";

const state = vi.hoisted(() => ({ snapshot: { session_id: null as string | null, phase: "idle" as string } }));
const invalidate = vi.hoisted(() => vi.fn());
vi.mock("@/features/recording/useRealtimeTaskSnapshot", () => ({ useRealtimeTaskSnapshot: () => state.snapshot }));

describe("SessionQueryRefresh", () => {
  it("refreshes workspace queries through cleaning up and the daemon terminal event", async () => {
    const client = new QueryClient();
    client.invalidateQueries = invalidate;
    const view = render(<QueryClientProvider client={client}><SessionQueryRefresh /></QueryClientProvider>);
    await waitFor(() => expect(invalidate).toHaveBeenCalledWith({ queryKey: ["sessions"] }));
    invalidate.mockClear();
    state.snapshot = { session_id: "session-1", phase: "transcribing" };
    view.rerender(<QueryClientProvider client={client}><SessionQueryRefresh /></QueryClientProvider>);
    await waitFor(() => expect(invalidate).toHaveBeenCalledWith({ queryKey: ["session"] }));
    invalidate.mockClear();
    state.snapshot = { session_id: "session-1", phase: "cleaning_up" };
    view.rerender(<QueryClientProvider client={client}><SessionQueryRefresh /></QueryClientProvider>);
    await waitFor(() => {
      expect(invalidate).toHaveBeenCalledWith({ queryKey: ["sessions"] });
      expect(invalidate).toHaveBeenCalledWith({ queryKey: ["session-workspace-detail"] });
      expect(invalidate).toHaveBeenCalledWith({ queryKey: ["session-cleanup-detail"] });
    });
    invalidate.mockClear();
    state.snapshot = { session_id: "session-1", phase: "completed" };
    view.rerender(<QueryClientProvider client={client}><SessionQueryRefresh /></QueryClientProvider>);
    await waitFor(() => {
      expect(invalidate).toHaveBeenCalledWith({ queryKey: ["session-workspace-detail"] });
      expect(invalidate).toHaveBeenCalledWith({ queryKey: ["session-cleanup-detail"] });
    });
    invalidate.mockClear();
    Object.defineProperty(document, "visibilityState", { configurable: true, value: "visible" });
    document.dispatchEvent(new Event("visibilitychange"));
    await waitFor(() => expect(invalidate).toHaveBeenCalledWith({ queryKey: ["sessions-search"] }));
  });
});
