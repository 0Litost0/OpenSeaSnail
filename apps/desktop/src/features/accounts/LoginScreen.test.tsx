import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import { I18nProvider } from "@/i18n/I18nProvider";
import { AccountEpochContext } from "./account-context";
const api = vi.hoisted(() => ({ action: vi.fn(), status: vi.fn() }));
vi.mock("@/api/transport", () => ({ desktopAccountAction: api.action, desktopAuthStatus: api.status }));
import { LoginScreen } from "./LoginScreen";
import { AccountPanel } from "./AccountPanel";
const signedOut = { initialized: true, authenticated: false, accounts: [{ id: "alice-id", username: "alice", is_active: false }] };
const signedIn = { ...signedOut, authenticated: true, accounts: [{ ...signedOut.accounts[0], is_active: true }] };
function mount(node: React.ReactNode) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  render(<QueryClientProvider client={client}><I18nProvider localeOverride="en-US">{node}</I18nProvider></QueryClientProvider>);
  return client;
}
beforeEach(() => { api.action.mockReset(); api.status.mockReset(); });
it("logs into the selected local account without exposing a token", async () => {
  api.action.mockResolvedValue(signedIn);
  const done = vi.fn();
  const client = mount(<LoginScreen status={signedOut} onAuthenticated={done} />);
  fireEvent.change(screen.getByLabelText("Password"), { target: { value: "password" } });
  fireEvent.click(screen.getByRole("button", { name: /^Log in$/ }));
  await waitFor(() => expect(done).toHaveBeenCalledOnce());
  expect(api.action).toHaveBeenCalledWith("login", { id: "alice-id", password: "password" });
  expect(client.getQueryData(["desktop-auth-status"])).toEqual(signedIn);
});
it("requires matching passwords to create an account and clears passwords on changing flow", async () => {
  api.action.mockResolvedValue(signedIn);
  mount(<LoginScreen status={signedOut} onAuthenticated={vi.fn()} />);
  fireEvent.change(screen.getByLabelText("Password"), { target: { value: "old-password" } });
  fireEvent.click(screen.getByRole("button", { name: "Create a new account" }));
  expect(screen.getByLabelText("Password")).toHaveValue("");
  fireEvent.change(screen.getByLabelText("Username"), { target: { value: "bob" } });
  fireEvent.change(screen.getByLabelText("Password"), { target: { value: "new-password" } });
  const confirm = screen.getByLabelText(/Confirm password/i);
  fireEvent.change(confirm, { target: { value: "mismatch" } });
  expect(screen.getByRole("button", { name: "Create and continue" })).toBeDisabled();
  fireEvent.change(confirm, { target: { value: "new-password" } });
  fireEvent.click(screen.getByRole("button", { name: "Create and continue" }));
  await waitFor(() => expect(api.action).toHaveBeenCalledWith("create", { username: "bob", password: "new-password" }));
});
it("keeps the login screen and clears password after failed authentication", async () => {
  api.action.mockRejectedValue("wrong_password");
  const done = vi.fn();
  mount(<LoginScreen status={signedOut} onAuthenticated={done} />);
  fireEvent.change(screen.getByLabelText("Password"), { target: { value: "wrong" } });
  fireEvent.click(screen.getByRole("button", { name: /^Log in$/ }));
  expect(await screen.findByRole("alert")).toHaveTextContent(/password/i);
  expect(screen.getByLabelText("Password")).toHaveValue("");
  expect(done).not.toHaveBeenCalled();
});
it("replaces additional-account settings with logout and resets account data", async () => {
  api.status.mockResolvedValue(signedIn); api.action.mockResolvedValue(signedOut);
  const reset = vi.fn();
  const client = mount(<AccountEpochContext.Provider value={{ epoch: 0, resetAccountData: reset }}><AccountPanel /></AccountEpochContext.Provider>);
  expect(await screen.findByText("alice")).toBeInTheDocument();
  expect(screen.queryByText("Additional account")).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Log out" }));
  await waitFor(() => expect(reset).toHaveBeenCalledOnce());
  expect(api.action).toHaveBeenCalledWith("logout");
  expect(client.getQueryData(["desktop-auth-status"])).toEqual(signedOut);
});

it("accepts existing passwords longer than the new-account limit", async () => {
  api.action.mockResolvedValue(signedIn);
  mount(<LoginScreen status={signedOut} onAuthenticated={vi.fn()} />);
  const password = "p".repeat(1100);
  expect(screen.getByLabelText("Password")).not.toHaveAttribute("maxlength");
  fireEvent.change(screen.getByLabelText("Password"), { target: { value: password } });
  fireEvent.click(screen.getByRole("button", { name: /^Log in$/ }));
  await waitFor(() => expect(api.action).toHaveBeenCalledWith("login", { id: "alice-id", password }));
});
