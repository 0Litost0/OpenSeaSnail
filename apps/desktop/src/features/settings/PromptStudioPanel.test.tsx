import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { CleanupSettings, CleanupTestResult } from "@/api/reasoning";
import { PromptStudioPanel } from "./PromptStudioPanel";

const state = vi.hoisted(() => ({
  settings: {} as CleanupSettings,
  putSettings: vi.fn(),
  testCleanup: vi.fn(),
}));

vi.mock("@/api/reasoning", () => ({
  getCleanupSettings: vi.fn(async () => state.settings),
  putCleanupSettings: state.putSettings,
  testCleanup: state.testCleanup,
}));
vi.mock("@/i18n/I18nProvider", () => ({
  useI18n: () => ({ t: (key: string, values?: Record<string, string | number>) => values?.elapsed == null ? key : `${key}:${values.elapsed}` }),
}));

function renderPanel() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  return render(<QueryClientProvider client={client}><PromptStudioPanel /></QueryClientProvider>);
}

function selectTab(name: string) {
  const tab = screen.getByRole("tab", { name });
  fireEvent.mouseDown(tab, { button: 0, ctrlKey: false });
  fireEvent.click(tab);
}

describe("PromptStudioPanel", () => {
  beforeEach(() => {
    state.settings = {
      enabled: true,
      selected_provider_config_id: "00000000-0000-4000-8000-000000000001",
      custom_prompt: "SAVED_PROMPT",
      default_prompt: "DEFAULT_PROMPT",
      protocol_prompt: "PROTOCOL_PROMPT",
      updated_at: "2026-09-08T00:00:00Z",
      selected_credential_state: "bound",
    };
    state.putSettings.mockReset();
    state.putSettings.mockImplementation(async (input) => ({
      ...state.settings,
      ...input,
      updated_at: "2026-09-08T00:00:01Z",
    }));
    state.testCleanup.mockReset();
    state.testCleanup.mockResolvedValue({
      cleaned_text: "Clean result",
      corrections: [{ original_text: "Sea Snail", corrected_text: "SeaSnail", kind: "proper_noun" }],
      elapsed_ms: 42,
    } satisfies CleanupTestResult);
  });

  it("shows the daemon protocol suffix as read-only", async () => {
    renderPanel();
    expect(await screen.findByLabelText("cleanup.prompt.semantic")).toHaveValue("SAVED_PROMPT");
    const protocol = screen.getByLabelText("cleanup.prompt.protocol");
    expect(protocol).toHaveValue("PROTOCOL_PROMPT");
    expect(protocol).toHaveAttribute("readonly");
  });

  it("tests the current draft without persisting it and renders validated output", async () => {
    renderPanel();
    await screen.findByRole("tab", { name: "cleanup.prompt.customize" });
    selectTab("cleanup.prompt.customize");
    fireEvent.change(screen.getByLabelText("cleanup.prompt.draft"), { target: { value: "UNSAVED_DRAFT" } });
    selectTab("cleanup.prompt.test");
    fireEvent.change(screen.getByLabelText("cleanup.prompt.test_input"), { target: { value: "Sea Snail" } });
    fireEvent.click(screen.getByRole("button", { name: "cleanup.prompt.run_test" }));

    await waitFor(() => expect(state.testCleanup).toHaveBeenCalledWith({
      provider_config_id: state.settings.selected_provider_config_id,
      text: "Sea Snail",
      prompt_draft: "UNSAVED_DRAFT",
    }));
    expect(state.putSettings).not.toHaveBeenCalled();
    expect(await screen.findByDisplayValue("Clean result")).toHaveAttribute("readonly");
    expect(screen.getByText("SeaSnail")).toBeInTheDocument();
    expect(screen.getByText("cleanup.prompt.elapsed:42")).toBeInTheDocument();
  });

  it("persists only an explicit save and can restore the daemon default", async () => {
    renderPanel();
    await screen.findByRole("tab", { name: "cleanup.prompt.customize" });
    selectTab("cleanup.prompt.customize");
    fireEvent.change(screen.getByLabelText("cleanup.prompt.draft"), { target: { value: "NEW_SAVED_PROMPT" } });
    fireEvent.click(screen.getByRole("button", { name: "cleanup.prompt.save" }));
    await waitFor(() => expect(state.putSettings).toHaveBeenCalledWith(expect.objectContaining({ custom_prompt: "NEW_SAVED_PROMPT" })));

    fireEvent.click(screen.getByRole("button", { name: "cleanup.prompt.restore" }));
    await waitFor(() => expect(state.putSettings).toHaveBeenLastCalledWith(expect.objectContaining({ custom_prompt: null })));
  });

  it("disables testing when the selected provider is not usable", async () => {
    state.settings = { ...state.settings, selected_credential_state: "missing" };
    renderPanel();
    await screen.findByRole("tab", { name: "cleanup.prompt.test" });
    selectTab("cleanup.prompt.test");
    expect(screen.getByLabelText("cleanup.prompt.test_input")).toBeDisabled();
    expect(screen.getByRole("button", { name: "cleanup.prompt.run_test" })).toBeDisabled();
    expect(screen.getByText("cleanup.prompt.no_provider")).toBeInTheDocument();
  });

  it("exposes a disabled loading state while a test is running", async () => {
    state.testCleanup.mockImplementation(() => new Promise(() => undefined));
    renderPanel();
    await screen.findByRole("tab", { name: "cleanup.prompt.test" });
    selectTab("cleanup.prompt.test");
    fireEvent.change(screen.getByLabelText("cleanup.prompt.test_input"), { target: { value: "sample" } });
    fireEvent.click(screen.getByRole("button", { name: "cleanup.prompt.run_test" }));
    const loading = (await screen.findByText("cleanup.prompt.testing")).closest("button");
    expect(loading).not.toBeNull();
    expect(loading).toBeDisabled();
    expect(loading?.querySelector("[data-slot=spinner]")).toBeInTheDocument();
  });
});
