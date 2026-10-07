import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { CleanupSettings, ProviderConfig } from "@/api/reasoning";
import { CleanupProviderPanel, endpointCredentialError } from "./CleanupProviderPanel";

const state = vi.hoisted(() => ({
  configs: [] as ProviderConfig[],
  settings: {} as CleanupSettings,
  replace: vi.fn(),
  deleteConfig: vi.fn(),
  putSettings: vi.fn(),
  setCredential: vi.fn(async () => "bound"),
  deleteCredential: vi.fn(async () => "missing"),
}));

vi.mock("@/api/reasoning", () => ({
  listProviderKinds: vi.fn(async () => [
    { provider_type: "openai", requires_credential: true },
    { provider_type: "openai_compatible_self_hosted_private", requires_credential: false },
  ]),
  listProviderConfigs: vi.fn(async () => state.configs),
  getCleanupSettings: vi.fn(async () => state.settings),
  createProviderConfig: vi.fn(),
  replaceProviderConfig: state.replace,
  deleteProviderConfig: state.deleteConfig,
  probeProviderConfig: vi.fn(),
  putCleanupSettings: state.putSettings,
}));
vi.mock("@/api/transport", () => ({
  setProviderCredential: state.setCredential,
  deleteProviderCredential: state.deleteCredential,
}));
vi.mock("@/i18n/I18nProvider", () => ({ useI18n: () => ({ t: (key: string) => key }) }));

const config = (credential_state: ProviderConfig["credential_state"]): ProviderConfig => ({
  id: "00000000-0000-4000-8000-000000000001",
  name: "Primary",
  provider_type: "openai",
  endpoint: "https://api.openai.com/v1",
  model: "gpt-test",
  created_at: "2026-09-08T00:00:00Z",
  updated_at: "2026-09-08T00:00:00Z",
  credential_state,
});

function renderPanel() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  return render(<QueryClientProvider client={client}><CleanupProviderPanel /></QueryClientProvider>);
}

describe("CleanupProviderPanel", () => {
  beforeEach(() => {
    vi.stubGlobal("ResizeObserver", class {
      observe() {}
      unobserve() {}
      disconnect() {}
    });
    state.configs = [config("bound")];
    state.settings = {
      enabled: false,
      selected_provider_config_id: state.configs[0].id,
      custom_prompt: null,
      default_prompt: "DEFAULT_PROMPT",
      protocol_prompt: "PROTOCOL_PROMPT",
      updated_at: "2026-09-08T00:00:00Z",
      selected_credential_state: "bound",
    };
    state.replace.mockReset();
    state.replace.mockImplementation(async (_id, input) => ({ ...state.configs[0], ...input }));
    state.putSettings.mockReset();
    state.putSettings.mockImplementation(async (input) => ({ ...state.settings, ...input }));
    state.setCredential.mockClear();
    state.deleteCredential.mockClear();
    state.deleteConfig.mockReset();
    state.deleteConfig.mockResolvedValue(undefined);
  });

  afterEach(() => vi.unstubAllGlobals());

  it("never backfills a saved API key and enables cleanup only for a usable configuration", async () => {
    renderPanel();
    const key = await screen.findByLabelText("cleanup.provider.api_key");
    expect(key).toHaveValue("");
    expect(key).toHaveAttribute("type", "password");
    expect(key).toHaveAttribute("placeholder", "cleanup.provider.key_saved");
    expect(screen.getByRole("switch", { name: "cleanup.provider.enabled" })).toBeEnabled();
  });

  it("keeps the cleanup switch disabled while the selected credential is missing", async () => {
    state.configs = [config("missing")];
    state.settings = { ...state.settings, selected_credential_state: "missing" };
    renderPanel();
    expect(await screen.findByRole("switch", { name: "cleanup.provider.enabled" })).toBeDisabled();
  });

  it("sends a replacement key only through the dedicated native command", async () => {
    renderPanel();
    const key = await screen.findByLabelText("cleanup.provider.api_key");
    fireEvent.change(key, { target: { value: "SECRET_SENTINEL" } });
    fireEvent.click(screen.getByRole("button", { name: "cleanup.provider.save" }));
    await waitFor(() => expect(state.replace).toHaveBeenCalled());
    expect(state.replace.mock.calls[0]?.[1]).not.toHaveProperty("credential");
    expect(state.setCredential).toHaveBeenCalledWith(state.configs[0].id, "SECRET_SENTINEL");
    expect(state.putSettings).toHaveBeenCalledWith(expect.objectContaining({ enabled: false }));
  });

  it("removes an existing no-auth binding when the user unchecks no authentication", async () => {
    state.configs = [{
      ...config("not_required"),
      provider_type: "openai_compatible_self_hosted_private",
      endpoint: "http://127.0.0.1:8080/v1",
    }];
    state.settings = {
      ...state.settings,
      selected_provider_config_id: state.configs[0].id,
      selected_credential_state: "not_required",
    };
    renderPanel();

    await waitFor(() => expect(screen.getByLabelText("cleanup.provider.endpoint")).toHaveValue("http://127.0.0.1:8080/v1"));
    const noAuth = await screen.findByRole("checkbox", { name: "cleanup.provider.no_auth" });
    expect(noAuth).toBeChecked();
    fireEvent.click(noAuth);
    fireEvent.click(screen.getByRole("button", { name: "cleanup.provider.save" }));

    await waitFor(() => expect(state.deleteCredential).toHaveBeenCalledWith(state.configs[0].id));
    expect(state.setCredential).not.toHaveBeenCalled();
  });

  it("associates endpoint guidance and requires a titled confirmation before deletion", async () => {
    renderPanel();
    const endpoint = await screen.findByLabelText("cleanup.provider.endpoint");
    expect(endpoint).toHaveAttribute("aria-describedby", "cleanup-endpoint-hint");
    expect(endpoint.closest("[data-slot=field]")).toHaveAttribute("data-disabled", "true");

    fireEvent.click(screen.getByRole("button", { name: "cleanup.provider.delete" }));
    expect(await screen.findByRole("alertdialog")).toBeInTheDocument();
    expect(screen.getByText("cleanup.provider.delete_title")).toBeInTheDocument();
    expect(state.deleteConfig).not.toHaveBeenCalled();
  });
});

describe("provider endpoint guidance", () => {
  it("mirrors the public HTTPS and private HTTP credential boundaries", () => {
    expect(endpointCredentialError("openai_compatible_self_hosted_public", "http://example.com/v1", false)).toBe("cleanup.provider.https_required");
    expect(endpointCredentialError("openai_compatible_self_hosted_private", "http://192.168.1.2/v1", true)).toBe("cleanup.provider.http_credential_forbidden");
    expect(endpointCredentialError("openai_compatible_self_hosted_private", "http://127.0.0.1:8080/v1", true)).toBeNull();
    expect(endpointCredentialError("openai_compatible_cloud", "https://example.com/v1?secret=x", false)).toBe("cleanup.provider.endpoint_invalid");
  });
});
