import type { components } from "./openapi";
import { unwrap } from "./errors";
import { request } from "./transport";

export type ProviderType = components["schemas"]["ProviderType"];
export type ProviderKind = components["schemas"]["ProviderKind"];
export type ProviderConfig = components["schemas"]["ProviderConfig"];
export type ProviderConfigInput = components["schemas"]["ProviderConfigInput"];
export type CleanupSettings = components["schemas"]["CleanupSettings"];
export type CleanupSettingsInput = components["schemas"]["CleanupSettingsInput"];
export type ProviderProbe = components["schemas"]["ProviderProbe"];
export type CleanupTestInput = components["schemas"]["CleanupTestRequest"];
export type CleanupTestResult = components["schemas"]["CleanupTestResult"];

export async function listProviderKinds(): Promise<ProviderKind[]> {
  return unwrap(request<ProviderKind[], "/reasoning/providers">({ method: "GET", path: "/reasoning/providers" }));
}

export async function listProviderConfigs(): Promise<ProviderConfig[]> {
  return unwrap(request<ProviderConfig[], "/reasoning/provider-configs">({ method: "GET", path: "/reasoning/provider-configs" }));
}

export async function createProviderConfig(input: ProviderConfigInput): Promise<ProviderConfig> {
  return unwrap(request<ProviderConfig, "/reasoning/provider-configs">({ method: "POST", path: "/reasoning/provider-configs", body: input }));
}

export async function replaceProviderConfig(id: string, input: ProviderConfigInput): Promise<ProviderConfig> {
  return unwrap(request<ProviderConfig, "/reasoning/provider-configs/{id}">({
    method: "PUT",
    path: `/reasoning/provider-configs/${encodeURIComponent(id)}` as "/reasoning/provider-configs/{id}",
    body: input,
  }));
}

export async function deleteProviderConfig(id: string): Promise<void> {
  await unwrap(request<unknown, "/reasoning/provider-configs/{id}">({
    method: "DELETE",
    path: `/reasoning/provider-configs/${encodeURIComponent(id)}` as "/reasoning/provider-configs/{id}",
  }));
}

export async function probeProviderConfig(id: string): Promise<ProviderProbe> {
  return unwrap(request<ProviderProbe, "/reasoning/provider-configs/{id}/probe">({
    method: "POST",
    path: `/reasoning/provider-configs/${encodeURIComponent(id)}/probe` as "/reasoning/provider-configs/{id}/probe",
  }));
}

export async function getCleanupSettings(): Promise<CleanupSettings> {
  return unwrap(request<CleanupSettings, "/cleanup/settings">({ method: "GET", path: "/cleanup/settings" }));
}

export async function putCleanupSettings(input: CleanupSettingsInput): Promise<CleanupSettings> {
  return unwrap(request<CleanupSettings, "/cleanup/settings">({ method: "PUT", path: "/cleanup/settings", body: input }));
}

export async function testCleanup(input: CleanupTestInput): Promise<CleanupTestResult> {
  return unwrap(request<CleanupTestResult, "/cleanup/test">({ method: "POST", path: "/cleanup/test", body: input }));
}
