import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import { I18nProvider } from "@/i18n/I18nProvider";
import { Onboarding } from "./Onboarding";

const api = vi.hoisted(() => ({ setup: vi.fn(), permissions: vi.fn(), microphone: vi.fn(), accessibility: vi.fn(), shortcut: vi.fn(), context: vi.fn(), updateContext: vi.fn() }));
vi.mock("@/api/auth", () => ({ setupAccount: api.setup }));
vi.mock("@/api/transport", () => ({ permissionStatus: api.permissions, requestMicrophonePermission: api.microphone, openAccessibilitySettings: api.accessibility, recordingShortcut: api.shortcut, clipboardContextStatus: api.context, setClipboardContextEnabled: api.updateContext }));

beforeEach(() => {
  Object.values(api).forEach((mock) => mock.mockReset());
  api.setup.mockResolvedValue({});
  api.permissions.mockResolvedValue({ microphone: { granted: false, status: "not_determined" }, accessibility_granted: false });
  api.microphone.mockResolvedValue({ granted: true, status: "authorized" });
  api.accessibility.mockResolvedValue(undefined);
  api.shortcut.mockResolvedValue({ recording_shortcut: "Command+Alt+Space", registered: true, registration_error: null });
  api.context.mockResolvedValue({ enabled: true });
  api.updateContext.mockImplementation(async (enabled: boolean) => ({ enabled }));
});

function mount(locale: "en-US" | "zh-CN" = "en-US") {
  const onComplete = vi.fn();
  const onAccountCreated = vi.fn();
  render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } })}><I18nProvider localeOverride={locale}><Onboarding onComplete={onComplete} onAccountCreated={onAccountCreated} /></I18nProvider></QueryClientProvider>);
  return { onComplete, onAccountCreated };
}
async function reviewPermissions() {
  fireEvent.click(screen.getByRole("button", { name: "Start setup" }));
  fireEvent.change(screen.getByLabelText("Username"), { target: { value: "new-user" } });
  fireEvent.change(screen.getByLabelText("Password", { exact: true }), { target: { value: "test-password" } });
  fireEvent.change(screen.getByLabelText("Confirm password"), { target: { value: "test-password" } });
  fireEvent.click(screen.getByRole("button", { name: "Review privacy and permissions" }));
  await screen.findByText("Understand permissions before allowing access");
}
async function createAccount(waitForCreation = true) {
  await reviewPermissions();
  fireEvent.click(screen.getByRole("button", { name: "Create account and enable secure storage" }));
  if (waitForCreation) await screen.findByRole("button", { name: "Secure storage is ready" });
}

it("explains local accounts and permissions before any permission request", async () => {
  const { onAccountCreated } = mount();
  expect(screen.getByText(/No online registration is needed/)).toBeInTheDocument();
  expect(api.permissions).not.toHaveBeenCalled();
  await reviewPermissions();
  expect(api.setup).not.toHaveBeenCalled();
  expect(onAccountCreated).not.toHaveBeenCalled();
  expect(screen.getByText(/macOS Keychain under com.seasnail/)).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Continue to the guide" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "Skip for now" })).toBeDisabled();
  expect(api.microphone).not.toHaveBeenCalled();
  expect(api.accessibility).not.toHaveBeenCalled();
  api.setup.mockImplementation(async () => {
    expect(screen.getByText(/macOS Keychain under com.seasnail/)).toBeInTheDocument();
    return {};
  });
  fireEvent.click(screen.getByRole("button", { name: "Create account and enable secure storage" }));
  await screen.findByRole("button", { name: "Secure storage is ready" });
  expect(onAccountCreated).toHaveBeenCalledOnce();
  expect(screen.getByText(/Without access, microphone dictation is unavailable/)).toBeInTheDocument();
  expect(screen.getByText("System Settings → Privacy & Security → Accessibility → SeaSnail")).toBeInTheDocument();
  expect(api.microphone).not.toHaveBeenCalled();
  expect(api.accessibility).not.toHaveBeenCalled();
  fireEvent.click(await screen.findByRole("button", { name: "Allow microphone" }));
  await waitFor(() => expect(api.microphone).toHaveBeenCalledOnce());
});

it("allows permission deferral, shows the real shortcut and saves the context choice", async () => {
  const { onComplete } = mount();
  await createAccount();
  fireEvent.click(screen.getByRole("button", { name: "Skip for now" }));
  expect(await screen.findByText("Your shortcut: ⌘ ⌥ Space")).toBeInTheDocument();
  expect(screen.getByRole("list", { name: "Clipboard context example" }).children).toHaveLength(3);
  expect(screen.getByText(/without the actual clipboard context/)).toBeInTheDocument();
  const toggle = await screen.findByRole("checkbox", { name: "Capture clipboard context while recording" });
  await waitFor(() => expect(toggle).toBeEnabled());
  fireEvent.click(toggle);
  await waitFor(() => expect(toggle).not.toBeChecked());
  expect(api.updateContext).toHaveBeenCalledWith(false, expect.anything());
  expect(onComplete).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Finish and enter workspace" }));
  expect(onComplete).toHaveBeenCalledOnce();
});

it("refreshes system permission state on return and reports request failures", async () => {
  mount();
  await createAccount();
  await waitFor(() => expect(screen.getByRole("button", { name: "Allow microphone" })).toBeEnabled());
  api.microphone.mockRejectedValue(new Error("internal"));
  fireEvent.click(screen.getByRole("button", { name: "Allow microphone" }));
  expect(await screen.findByText("Could not check or update permissions")).toBeInTheDocument();
  api.permissions.mockResolvedValue({ microphone: { granted: true, status: "authorized" }, accessibility_granted: true });
  fireEvent(window, new Event("focus"));
  await waitFor(() => expect(screen.getAllByText("Allowed")).toHaveLength(2));
  expect(api.accessibility).not.toHaveBeenCalled();
});

it("keeps the saved context preference if a change fails", async () => {
  mount();
  await createAccount();
  fireEvent.click(screen.getByRole("button", { name: "Continue to the guide" }));
  const toggle = await screen.findByRole("checkbox", { name: "Capture clipboard context while recording" });
  await waitFor(() => expect(toggle).toBeEnabled());
  let reject!: (error: Error) => void;
  api.updateContext.mockImplementation(() => new Promise((_resolve, fail) => { reject = fail; }));
  fireEvent.click(toggle);
  await waitFor(() => expect(screen.getByRole("button", { name: "Finish and enter workspace" })).toBeDisabled());
  await act(async () => reject(new Error("internal")));
  await screen.findByText("Unable to update clipboard context settings");
  expect(toggle).toBeChecked();
  expect(screen.getByRole("button", { name: "Finish and enter workspace" })).toBeEnabled();
});

it("preserves the account form after setup fails", async () => {
  api.setup.mockRejectedValue(new Error("internal"));
  mount();
  await createAccount(false);
  expect(await screen.findByText("Account was not created")).toBeInTheDocument();
  expect(screen.getByText(/macOS Keychain under com.seasnail/)).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Continue to the guide" })).toBeDisabled();
  fireEvent.click(screen.getByRole("button", { name: "Back" }));
  expect(screen.getByLabelText("Username")).toHaveValue("new-user");
  expect(screen.getByLabelText("Password", { exact: true })).toHaveValue("test-password");
  expect(api.microphone).not.toHaveBeenCalled();
});

it("shows the introductory account and privacy copy in Chinese", () => {
  mount("zh-CN");
  expect(screen.getByText("欢迎使用 SeaSnail")).toBeInTheDocument();
  expect(screen.getByText(/无需注册在线服务/)).toBeInTheDocument();
  expect(screen.getByText(/AI 文本整理是可选功能，默认关闭/)).toBeInTheDocument();
});

it("stays on the privacy page and prevents duplicate creation while Keychain access is pending", async () => {
  let finish!: (value: object) => void;
  api.setup.mockImplementation(() => new Promise((resolve) => { finish = resolve; }));
  const { onAccountCreated } = mount();
  await createAccount(false);
  const creating = await screen.findByRole("button", { name: "Creating…" });
  expect(creating).toBeDisabled();
  expect(screen.getByRole("button", { name: "Back" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "Continue to the guide" })).toBeDisabled();
  fireEvent.click(creating);
  expect(api.setup).toHaveBeenCalledOnce();
  expect(onAccountCreated).not.toHaveBeenCalled();
  expect(screen.getByText(/macOS Keychain under com.seasnail/)).toBeInTheDocument();
  await act(async () => finish({}));
  await screen.findByRole("button", { name: "Secure storage is ready" });
  expect(onAccountCreated).toHaveBeenCalledOnce();
});

it("does not recreate the account when returning from the tutorial", async () => {
  mount();
  await createAccount();
  fireEvent.click(screen.getByRole("button", { name: "Continue to the guide" }));
  await screen.findByText("Your shortcut: ⌘ ⌥ Space");
  fireEvent.click(screen.getByRole("button", { name: "Back" }));
  expect(screen.getByRole("button", { name: "Secure storage is ready" })).toBeDisabled();
  expect(api.setup).toHaveBeenCalledOnce();
});
