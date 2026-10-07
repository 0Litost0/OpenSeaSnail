import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { ShortcutPanel } from "./ShortcutPanel";
import { shortcutFromKey } from "./shortcutKeys";
const api = vi.hoisted(() => ({ get: vi.fn(), set: vi.fn(), capture: vi.fn() }));
vi.mock("@/api/transport", () => ({ recordingShortcut: api.get, setRecordingShortcut: api.set, setRecordingShortcutCapture: api.capture }));
vi.mock("@/i18n/I18nProvider", () => ({ useI18n: () => ({ t: (key: string) => key }) }));
function show() {
  return render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 }, mutations: { retry: false } } })}><ShortcutPanel /></QueryClientProvider>);
}
beforeEach(() => {
  vi.resetAllMocks();
  api.get.mockResolvedValue({ recording_shortcut: "Command+Shift+Space", registered: true, registration_error: null });
  api.capture.mockResolvedValue(undefined);
  api.set.mockImplementation(async (recording_shortcut: string) => {
    const status = { recording_shortcut, registered: true, registration_error: null };
    api.get.mockResolvedValue(status);
    return status;
  });
});
async function begin() {
  const change = await screen.findByRole("button", { name: "shortcut.change" });
  await waitFor(() => expect(change).toBeEnabled());
  fireEvent.click(change);
  await screen.findByText("shortcut.press");
  expect(api.capture).toHaveBeenCalledWith(true);
}
describe("shortcut recorder", () => {
  it("captures physical keys, confirms before saving and resumes the shortcut", async () => {
    show(); await begin();
    fireEvent.keyDown(window, { key: "å", code: "KeyA", metaKey: true, altKey: true });
    expect(screen.getByText("⌘ ⌥ A")).toBeInTheDocument();
    expect(api.set).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "shortcut.save" }));
    await waitFor(() => expect(api.set).toHaveBeenCalledWith("Command+Alt+A"));
    await waitFor(() => expect(api.capture).toHaveBeenLastCalledWith(false));
    expect(await screen.findByRole("button", { name: "shortcut.change" })).toBeEnabled();
  });
  it.each(["escape", "blur", "unmount"])("restores the original shortcut on %s without saving", async (action) => {
    const view = show(); await begin();
    if (action === "escape") fireEvent.keyDown(window, { key: "Escape", code: "Escape" });
    else if (action === "blur") fireEvent.blur(window);
    else view.unmount();
    await waitFor(() => expect(api.capture).toHaveBeenLastCalledWith(false));
    expect(api.set).not.toHaveBeenCalled();
  });
  it("rejects plain text keys and keeps an unsuccessful candidate available", async () => {
    api.set.mockRejectedValue("shortcut_register_failed");
    show(); await begin();
    fireEvent.keyDown(window, { key: "a", code: "KeyA" });
    expect(screen.getByText("shortcut.invalid_hint")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "shortcut.save" })).toBeDisabled();
    fireEvent.keyDown(window, { key: "a", code: "KeyA", ctrlKey: true });
    fireEvent.click(screen.getByRole("button", { name: "shortcut.save" }));
    expect(await screen.findByRole("alert")).toBeInTheDocument();
    expect(screen.getByText("⌃ A")).toBeInTheDocument();
    expect(api.capture).toHaveBeenLastCalledWith(true);
    fireEvent.click(screen.getByRole("button", { name: "shortcut.cancel" }));
    await waitFor(() => expect(api.capture).toHaveBeenLastCalledWith(false));
  });
  it("restores the default through the same validated native setter", async () => {
    show();
    const reset = await screen.findByRole("button", { name: "shortcut.reset" });
    await waitFor(() => expect(reset).toBeEnabled());
    fireEvent.click(reset);
    await waitFor(() => expect(api.set).toHaveBeenCalledWith("Command+Shift+Space"));
  });
  it("restores registration if the panel closes during native capture setup", async () => {
    let resolve!: () => void;
    api.capture.mockImplementation((capturing: boolean) => capturing ? new Promise<void>((done) => { resolve = done; }) : Promise.resolve());
    const view = show();
    const change = await screen.findByRole("button", { name: "shortcut.change" });
    await waitFor(() => expect(change).toBeEnabled());
    fireEvent.click(change); view.unmount();
    await act(async () => resolve());
    expect(api.capture).toHaveBeenLastCalledWith(false);
  });
  it("cancels with Escape while native capture setup is still pending", async () => {
    let resolve!: () => void;
    api.capture.mockImplementation((capturing: boolean) => capturing ? new Promise<void>((done) => { resolve = done; }) : Promise.resolve());
    show();
    const change = await screen.findByRole("button", { name: "shortcut.change" });
    await waitFor(() => expect(change).toBeEnabled());
    fireEvent.click(change);
    fireEvent.keyDown(window, { key: "Escape", code: "Escape" });
    await act(async () => resolve());
    await waitFor(() => expect(api.capture).toHaveBeenLastCalledWith(false));
    expect(screen.getByRole("button", { name: "shortcut.change" })).toBeEnabled();
    expect(screen.queryByText("shortcut.press")).not.toBeInTheDocument();
    expect(api.set).not.toHaveBeenCalled();
  });
  it("ignores modifiers, composition and repeat while preserving physical digits", () => {
    const base = { code: "Digit2", metaKey: true, ctrlKey: false, altKey: false, shiftKey: true, isComposing: false, repeat: false };
    expect(shortcutFromKey(base)).toBe("Command+Shift+2");
    expect(shortcutFromKey({ ...base, code: "MetaLeft" })).toBeNull();
    expect(shortcutFromKey({ ...base, isComposing: true })).toBeNull();
    expect(shortcutFromKey({ ...base, repeat: true })).toBeNull();
  });
});
