import { useEffect, useMemo, useRef, useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  createProviderConfig,
  deleteProviderConfig,
  getCleanupSettings,
  listProviderConfigs,
  listProviderKinds,
  probeProviderConfig,
  putCleanupSettings,
  replaceProviderConfig,
  type ProviderConfig,
  type ProviderConfigInput,
  type ProviderProbe,
  type ProviderType,
} from "@/api/reasoning";
import { deleteProviderCredential, setProviderCredential } from "@/api/transport";
import { localizedError } from "@/api/errors";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import {
  AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent,
  AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle,
  AlertDialogTrigger,
} from "@/components/ui/alert-dialog";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Field, FieldContent, FieldDescription, FieldError, FieldGroup, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { Spinner } from "@/components/ui/spinner";
import { Switch } from "@/components/ui/switch";
import { useI18n } from "@/i18n/I18nProvider";
import type { TranslationKey } from "@/i18n/dictionaries";

const EMPTY_DRAFT: ProviderConfigInput = {
  name: "",
  provider_type: "openai",
  endpoint: "https://api.openai.com/v1",
  model: "",
};

function configDraft(config?: ProviderConfig): ProviderConfigInput {
  return config ? {
    name: config.name,
    provider_type: config.provider_type,
    endpoint: config.endpoint,
    model: config.model,
  } : { ...EMPTY_DRAFT };
}

function credentialUsable(config?: ProviderConfig): boolean {
  return config?.credential_state === "bound" || config?.credential_state === "not_required";
}

export function endpointCredentialError(
  providerType: ProviderType,
  endpoint: string,
  hasNewCredential: boolean,
): TranslationKey | null {
  let url: URL;
  try {
    url = new URL(endpoint);
  } catch {
    return "cleanup.provider.endpoint_invalid";
  }
  if (!["http:", "https:"].includes(url.protocol) || url.username || url.password || url.search || url.hash) {
    return "cleanup.provider.endpoint_invalid";
  }
  if (providerType === "openai_compatible_cloud" || providerType === "openai_compatible_self_hosted_public") {
    return url.protocol === "https:" ? null : "cleanup.provider.https_required";
  }
  if (providerType === "openai_compatible_self_hosted_private" && url.protocol === "http:" && hasNewCredential) {
    const loopback = url.hostname === "localhost" || url.hostname === "127.0.0.1" || url.hostname === "[::1]";
    return loopback ? null : "cleanup.provider.http_credential_forbidden";
  }
  return null;
}

const credentialLabel = {
  not_required: "cleanup.provider.credential_not_required",
  missing: "cleanup.provider.credential_missing",
  bound: "cleanup.provider.credential_bound",
  stale: "cleanup.provider.credential_stale",
} as const;

export function CleanupProviderPanel() {
  const { t } = useI18n();
  const client = useQueryClient();
  const configs = useQuery({ queryKey: ["reasoning-provider-configs"], queryFn: listProviderConfigs });
  const kinds = useQuery({ queryKey: ["reasoning-provider-kinds"], queryFn: listProviderKinds });
  const settings = useQuery({ queryKey: ["cleanup-settings"], queryFn: getCleanupSettings });
  const [editingId, setEditingId] = useState<string | null>(null);
  const [draft, setDraft] = useState<ProviderConfigInput>(EMPTY_DRAFT);
  const [credential, setCredential] = useState("");
  const [noAuth, setNoAuth] = useState(false);
  const [probe, setProbe] = useState<ProviderProbe | null>(null);
  const initialized = useRef(false);

  const selected = configs.data?.find(({ id }) => id === editingId);
  const kind = kinds.data?.find(({ provider_type }) => provider_type === draft.provider_type);
  const canUseNoAuth = kind ? !kind.requires_credential : draft.provider_type.includes("self_hosted");

  useEffect(() => {
    if (initialized.current || !configs.data || !settings.data) return;
    initialized.current = true;
    const initial = settings.data.selected_provider_config_id ?? configs.data[0]?.id ?? null;
    if (initial) {
      const config = configs.data.find(({ id }) => id === initial);
      setEditingId(initial);
      setDraft(configDraft(config));
      setNoAuth(config?.credential_state === "not_required");
    }
  }, [configs.data, editingId, settings.data]);

  const refresh = async () => {
    await Promise.all([
      client.invalidateQueries({ queryKey: ["reasoning-provider-configs"] }),
      client.invalidateQueries({ queryKey: ["cleanup-settings"] }),
    ]);
  };

  const save = useMutation({
    mutationFn: async () => {
      const saved = editingId
        ? await replaceProviderConfig(editingId, draft)
        : await createProviderConfig(draft);
      if (credential) await setProviderCredential(saved.id, credential);
      else if (noAuth) await setProviderCredential(saved.id, null);
      else if (selected?.credential_state === "not_required") await deleteProviderCredential(saved.id);
      await putCleanupSettings({
        enabled: false,
        selected_provider_config_id: saved.id,
        custom_prompt: settings.data?.custom_prompt ?? null,
      });
      return saved.id;
    },
    onSuccess: async (id) => {
      setEditingId(id);
      setCredential("");
      setProbe(null);
      await refresh();
    },
  });

  const select = useMutation({
    mutationFn: async (id: string) => {
      const next = configs.data?.find((config) => config.id === id);
      await putCleanupSettings({
        enabled: Boolean(settings.data?.enabled && credentialUsable(next)),
        selected_provider_config_id: id,
        custom_prompt: settings.data?.custom_prompt ?? null,
      });
      return id;
    },
    onSuccess: async (id) => {
      const next = configs.data?.find((config) => config.id === id);
      setEditingId(id);
      setDraft(configDraft(next));
      setCredential("");
      setNoAuth(next?.credential_state === "not_required");
      setProbe(null);
      await refresh();
    },
  });

  const toggle = useMutation({
    mutationFn: (enabled: boolean) => putCleanupSettings({
      enabled,
      selected_provider_config_id: editingId,
      custom_prompt: settings.data?.custom_prompt ?? null,
    }),
    onSuccess: (value) => client.setQueryData(["cleanup-settings"], value),
  });
  const remove = useMutation({
    mutationFn: async () => { if (editingId) await deleteProviderConfig(editingId); },
    onSuccess: async () => {
      setEditingId(null);
      setDraft(configDraft());
      setCredential("");
      setNoAuth(false);
      setProbe(null);
      await refresh();
    },
  });
  const clearCredential = useMutation({
    mutationFn: async () => { if (editingId) await deleteProviderCredential(editingId); },
    onSuccess: refresh,
  });
  const testConnection = useMutation({
    mutationFn: async () => {
      if (!editingId) throw new Error("not_found");
      return probeProviderConfig(editingId);
    },
    onSuccess: setProbe,
  });
  const error = configs.error ?? kinds.error ?? settings.error ?? save.error ?? select.error
    ?? toggle.error ?? remove.error ?? clearCredential.error ?? testConnection.error;
  const busy = save.isPending || select.isPending || toggle.isPending || remove.isPending || clearCredential.isPending;
  const usable = credentialUsable(selected);
  const credentialState = useMemo(() => selected?.credential_state ?? "missing", [selected]);
  const endpointError = endpointCredentialError(draft.provider_type, draft.endpoint, credential.length > 0);

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (endpointError) return;
    save.mutate();
  };

  return <Card>
    <CardHeader>
      <CardTitle>{t("cleanup.provider.title")}</CardTitle>
      <CardDescription>{t("cleanup.provider.description")}</CardDescription>
    </CardHeader>
    <CardContent className="flex flex-col gap-5">
      {configs.isPending || kinds.isPending || settings.isPending ? <Skeleton className="h-48 w-full" /> : <>
        <Field orientation="horizontal" data-disabled={!usable || toggle.isPending || undefined}>
          <Switch id="cleanup-enabled" checked={settings.data?.enabled ?? false} disabled={!usable || toggle.isPending} onCheckedChange={(checked) => toggle.mutate(checked)} />
          <FieldContent>
            <FieldLabel htmlFor="cleanup-enabled">{t("cleanup.provider.enabled")}</FieldLabel>
            <FieldDescription>{usable ? t("cleanup.provider.enabled_hint") : t("cleanup.provider.incomplete")}</FieldDescription>
          </FieldContent>
        </Field>
        <Field>
          <FieldLabel htmlFor="cleanup-config">{t("cleanup.provider.configuration")}</FieldLabel>
          <div className="flex gap-2">
            <Select value={editingId ?? ""} onValueChange={(value) => { if (value) select.mutate(value); }} disabled={busy}>
              <SelectTrigger id="cleanup-config" className="flex-1"><SelectValue placeholder={t("cleanup.provider.select")} /></SelectTrigger>
              <SelectContent><SelectGroup>{configs.data?.map((config) => <SelectItem key={config.id} value={config.id}>{config.name}</SelectItem>)}</SelectGroup></SelectContent>
            </Select>
            <Button type="button" variant="outline" onClick={() => { setEditingId(null); setDraft(configDraft()); setCredential(""); setNoAuth(false); setProbe(null); }}>{t("cleanup.provider.new")}</Button>
          </div>
        </Field>
        <form onSubmit={submit}>
          <FieldGroup>
            <Field><FieldLabel htmlFor="cleanup-name">{t("cleanup.provider.name")}</FieldLabel><Input id="cleanup-name" value={draft.name} maxLength={128} required onChange={(event) => setDraft({ ...draft, name: event.target.value })} /></Field>
            <Field><FieldLabel htmlFor="cleanup-type">{t("cleanup.provider.type")}</FieldLabel><Select value={draft.provider_type} onValueChange={(value) => { if (!value) return; const providerType = value as ProviderType; setDraft({ ...draft, provider_type: providerType, endpoint: providerType === "openai" ? "https://api.openai.com/v1" : draft.endpoint }); setNoAuth(false); }}><SelectTrigger id="cleanup-type"><SelectValue /></SelectTrigger><SelectContent><SelectGroup>{kinds.data?.map((entry) => <SelectItem key={entry.provider_type} value={entry.provider_type}>{entry.provider_type}</SelectItem>)}</SelectGroup></SelectContent></Select></Field>
            <Field data-disabled={draft.provider_type === "openai" || undefined} data-invalid={endpointError ? true : undefined}><FieldLabel htmlFor="cleanup-endpoint">{t("cleanup.provider.endpoint")}</FieldLabel><Input id="cleanup-endpoint" type="url" value={draft.endpoint} maxLength={2048} required disabled={draft.provider_type === "openai"} aria-invalid={endpointError ? true : undefined} aria-describedby={endpointError ? "cleanup-endpoint-error" : "cleanup-endpoint-hint"} onChange={(event) => setDraft({ ...draft, endpoint: event.target.value })} /><FieldDescription id="cleanup-endpoint-hint">{t("cleanup.provider.endpoint_hint")}</FieldDescription>{endpointError && <FieldError id="cleanup-endpoint-error">{t(endpointError)}</FieldError>}</Field>
            <Field><FieldLabel htmlFor="cleanup-model">{t("cleanup.provider.model")}</FieldLabel><Input id="cleanup-model" value={draft.model} maxLength={256} required onChange={(event) => setDraft({ ...draft, model: event.target.value })} /><FieldDescription>{t("cleanup.provider.model_hint")}</FieldDescription></Field>
            <Field data-disabled={noAuth || undefined}><FieldLabel htmlFor="cleanup-key">{t("cleanup.provider.api_key")}</FieldLabel><Input id="cleanup-key" type="password" autoComplete="new-password" value={credential} maxLength={8192} disabled={noAuth} onChange={(event) => setCredential(event.target.value)} placeholder={selected?.credential_state === "bound" ? t("cleanup.provider.key_saved") : undefined} /><FieldDescription>{t("cleanup.provider.key_write_only")}</FieldDescription></Field>
            {canUseNoAuth && <Field orientation="horizontal"><Checkbox id="cleanup-no-auth" checked={noAuth} onCheckedChange={(checked) => { setNoAuth(checked === true); if (checked === true) setCredential(""); }} /><FieldContent><FieldLabel htmlFor="cleanup-no-auth">{t("cleanup.provider.no_auth")}</FieldLabel><FieldDescription>{t("cleanup.provider.no_auth_hint")}</FieldDescription></FieldContent></Field>}
            <div className="flex flex-wrap items-center gap-2">
              <Button type="submit" disabled={busy || Boolean(endpointError)}>{save.isPending && <Spinner data-icon="inline-start" />}{t("cleanup.provider.save")}</Button>
              {editingId && <Button type="button" variant="outline" disabled={testConnection.isPending || !usable} onClick={() => testConnection.mutate()}>{testConnection.isPending && <Spinner data-icon="inline-start" />}{t("cleanup.provider.probe")}</Button>}
              {editingId && credentialState !== "missing" && <Button type="button" variant="outline" disabled={busy} onClick={() => clearCredential.mutate()}>{t("cleanup.provider.clear_key")}</Button>}
              <Badge variant="secondary">{t(credentialLabel[credentialState])}</Badge>
            </div>
          </FieldGroup>
        </form>
        {probe && <Alert><AlertTitle>{t("cleanup.provider.probe_ok")}</AlertTitle><AlertDescription>{t("cleanup.provider.probe_result", { model: probe.model, elapsed: probe.elapsed_ms })}</AlertDescription></Alert>}
        {error && <Alert variant="destructive"><AlertTitle>{t("cleanup.provider.error")}</AlertTitle><AlertDescription>{localizedError(t, error)}</AlertDescription></Alert>}
      </>}
    </CardContent>
    {editingId && <CardFooter>
      <AlertDialog>
        <AlertDialogTrigger asChild><Button variant="destructive" disabled={busy}>{t("cleanup.provider.delete")}</Button></AlertDialogTrigger>
        <AlertDialogContent><AlertDialogHeader><AlertDialogTitle>{t("cleanup.provider.delete_title")}</AlertDialogTitle><AlertDialogDescription>{t("cleanup.provider.delete_description")}</AlertDialogDescription></AlertDialogHeader><AlertDialogFooter><AlertDialogCancel>{t("common.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={() => remove.mutate()}>{t("cleanup.provider.delete")}</AlertDialogAction></AlertDialogFooter></AlertDialogContent>
      </AlertDialog>
    </CardFooter>}
  </Card>;
}
