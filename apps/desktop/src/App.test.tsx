import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";
import { I18nProvider } from "@/i18n/I18nProvider";
const api = vi.hoisted(() => ({ status: vi.fn(), action: vi.fn() }));
vi.mock("@/api/transport", async (original) => ({ ...(await original<typeof import("@/api/transport")>()), desktopAuthStatus: api.status, desktopAccountAction: api.action }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn().mockResolvedValue(() => undefined) }));
vi.mock("@/features/onboarding/Onboarding", () => ({ Onboarding: () => <p>First account onboarding</p> }));
vi.mock("@/features/workspace/WorkspaceShell", async () => {
  const { AccountPanel } = await import("@/features/accounts/AccountPanel");
  return { WorkspaceShell: () => <><p>Private workspace</p><AccountPanel /></> };
});
import App from "./App";
beforeEach(() => { api.status.mockReset(); api.action.mockReset(); });
function mount() { render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}><I18nProvider localeOverride="en-US"><App /></I18nProvider></QueryClientProvider>); }
it("gates an initialized but logged-out account before mounting the private workspace", async () => {
  api.status.mockResolvedValue({ initialized: true, authenticated: false, accounts: [{ id: "a", username: "alice", is_active: false }] });
  mount();
  expect(await screen.findByText("Welcome to SeaSnail")).toBeInTheDocument();
  expect(screen.queryByText("Private workspace")).not.toBeInTheDocument();
  expect(screen.queryByText("First account onboarding")).not.toBeInTheDocument();
});
it("preserves first-account onboarding for an uninitialized installation", async () => {
  api.status.mockResolvedValue({ initialized: false, authenticated: false, accounts: [] });
  mount();
  expect(await screen.findByText("First account onboarding")).toBeInTheDocument();
  expect(screen.queryByText("Private workspace")).not.toBeInTheDocument();
});

it("retries credential synchronization without another password login", async () => {
  api.status.mockResolvedValueOnce({ initialized: true, authenticated: true, credential_ready: false, accounts: [] })
    .mockResolvedValue({ initialized: true, authenticated: true, credential_ready: true, accounts: [] });
  mount();
  fireEvent.click(await screen.findByRole("button", { name: "Retry" }));
  expect(await screen.findByText("Private workspace")).toBeInTheDocument();
});

it("removes private query data and prevents a delayed response from restoring it after logout", async () => {
  const out = { initialized: true, authenticated: false, accounts: [{ id: "a", username: "alice", is_active: false }] };
  api.status.mockResolvedValue({ ...out, authenticated: true, accounts: [{ ...out.accounts[0], is_active: true }] });
  api.action.mockImplementation(async () => { api.status.mockResolvedValue(out); return out; });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  client.setQueryData(["sessions", "cached"], ["private transcript"]);
  let finish!: (value: string[]) => void;
  const pending = client.fetchQuery({ queryKey: ["sessions", "late"], queryFn: () => new Promise<string[]>((resolve) => { finish = resolve; }) }).catch(() => undefined);
  render(<QueryClientProvider client={client}><I18nProvider localeOverride="en-US"><App /></I18nProvider></QueryClientProvider>);
  fireEvent.click(await screen.findByRole("button", { name: "Log out" }));
  expect(await screen.findByText("Welcome to SeaSnail")).toBeInTheDocument();
  await act(async () => { finish(["late private transcript"]); await pending; });
  await waitFor(() => expect(client.getQueryData(["sessions", "cached"])).toBeUndefined());
  expect(client.getQueryData(["sessions", "late"])).toBeUndefined();
  expect(screen.queryByText("Private workspace")).not.toBeInTheDocument();
});

it("clears private caches when a failed logout is later confirmed signed out", async () => {
  const out = { initialized: true, authenticated: false, accounts: [{ id: "a", username: "alice", is_active: false }] };
  api.status.mockResolvedValue({ ...out, authenticated: true, accounts: [{ ...out.accounts[0], is_active: true }] });
  api.action.mockImplementation(async () => { api.status.mockResolvedValue(out); throw new Error("internal"); });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  client.setQueryData(["sessions", "cached"], ["private transcript"]);
  render(<QueryClientProvider client={client}><I18nProvider localeOverride="en-US"><App /></I18nProvider></QueryClientProvider>);
  fireEvent.click(await screen.findByRole("button", { name: "Log out" }));
  expect(await screen.findByText("Welcome to SeaSnail")).toBeInTheDocument();
  await waitFor(() => expect(client.getQueryData(["sessions", "cached"])).toBeUndefined());
});
