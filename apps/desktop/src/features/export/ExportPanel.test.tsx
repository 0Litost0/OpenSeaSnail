import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { I18nProvider } from "@/i18n/I18nProvider";

const api = vi.hoisted(() => ({ exportToFile: vi.fn() }));
vi.mock("@/api/transport", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/api/transport")>()),
  exportToFile: api.exportToFile,
}));
vi.mock("@/features/sessions/queries", () => ({ useSessions: () => ({
  data: { pages: [{ items: [{ id: "session-1", created_at: "2026-10-07T00:00:00Z", preview: "Example transcript" }] }] },
  hasNextPage: false,
}) }));
import { ExportPanel } from "./ExportPanel";

function renderPanel() {
  const client = new QueryClient({ defaultOptions: { mutations: { retry: false } } });
  return render(<QueryClientProvider client={client}><I18nProvider localeOverride="en-US"><ExportPanel /></I18nProvider></QueryClientProvider>);
}

describe("ExportPanel confirmation", () => {
  beforeEach(() => { api.exportToFile.mockReset(); });

  it("requests a password only after the export action and cancels without invoking native export", async () => {
    renderPanel();
    expect(screen.queryByLabelText("Account password")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Choose location and export" }));
    const password = await screen.findByLabelText("Account password");
    expect(password).toHaveFocus();
    expect(screen.getByRole("button", { name: "Continue to save location" })).toBeDisabled();
    fireEvent.change(password, { target: { value: "temporary-password" } });
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByLabelText("Account password")).not.toBeInTheDocument());
    expect(api.exportToFile).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Choose location and export" }));
    expect(await screen.findByLabelText("Account password")).toHaveValue("");
  });

  it("confirms the selection, closes the password dialog, prevents duplicate exports and reports cancellation", async () => {
    let finish!: (saved: boolean) => void;
    api.exportToFile.mockImplementation(() => new Promise<boolean>((resolve) => { finish = resolve; }));
    renderPanel();
    fireEvent.click(screen.getByRole("checkbox"));
    fireEvent.click(screen.getByRole("button", { name: "Choose location and export" }));
    fireEvent.change(await screen.findByLabelText("Account password"), { target: { value: "test-password" } });
    fireEvent.click(screen.getByRole("button", { name: "Continue to save location" }));
    await waitFor(() => expect(api.exportToFile).toHaveBeenCalledWith({ password: "test-password", session_ids: ["session-1"] }));
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
    expect(await screen.findByRole("button", { name: "Preparing export…" })).toBeDisabled();
    expect(screen.getByRole("checkbox")).toBeDisabled();
    finish(false);
    expect(await screen.findByText("Saving was cancelled.")).toBeInTheDocument();
    expect(api.exportToFile).toHaveBeenCalledTimes(1);
  });

  it("reports a native failure and requires fresh password confirmation to retry", async () => {
    api.exportToFile.mockRejectedValue(new Error("connection"));
    renderPanel();
    fireEvent.click(screen.getByRole("button", { name: "Choose location and export" }));
    fireEvent.change(await screen.findByLabelText("Account password"), { target: { value: "test-password" } });
    fireEvent.click(screen.getByRole("button", { name: "Continue to save location" }));
    expect(await screen.findByRole("alert")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Choose location and export" }));
    expect(await screen.findByLabelText("Account password")).toHaveValue("");
    expect(api.exportToFile).toHaveBeenCalledTimes(1);
  });
});
