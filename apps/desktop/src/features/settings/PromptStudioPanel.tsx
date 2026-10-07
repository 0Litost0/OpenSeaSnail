import { useEffect, useRef, useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { localizedError } from "@/api/errors";
import { getCleanupSettings, putCleanupSettings, testCleanup } from "@/api/reasoning";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field";
import { Spinner } from "@/components/ui/spinner";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Textarea } from "@/components/ui/textarea";
import { useI18n } from "@/i18n/I18nProvider";
import type { TranslationKey } from "@/i18n/dictionaries";

const correctionKindLabel = {
  phonetic: "cleanup.prompt.kind_phonetic",
  proper_noun: "cleanup.prompt.kind_proper_noun",
  other_asr: "cleanup.prompt.kind_other_asr",
} as const satisfies Record<string, TranslationKey>;

export function PromptStudioPanel() {
  const { t } = useI18n();
  const client = useQueryClient();
  const settings = useQuery({ queryKey: ["cleanup-settings"], queryFn: getCleanupSettings });
  const [draft, setDraft] = useState("");
  const [sample, setSample] = useState("");
  const [dirty, setDirty] = useState(false);
  const [saveNotice, setSaveNotice] = useState<"saved" | "restored" | null>(null);
  const initializedAt = useRef<string | null>(null);

  useEffect(() => {
    if (!settings.data || dirty || initializedAt.current === settings.data.updated_at) return;
    initializedAt.current = settings.data.updated_at;
    setDraft(settings.data.custom_prompt ?? settings.data.default_prompt);
  }, [dirty, settings.data]);

  const savePrompt = useMutation({
    mutationFn: (customPrompt: string | null) => {
      if (!settings.data) throw new Error("not_found");
      return putCleanupSettings({
        enabled: settings.data.enabled,
        selected_provider_config_id: settings.data.selected_provider_config_id,
        custom_prompt: customPrompt,
      });
    },
    onSuccess: (value, customPrompt) => {
      client.setQueryData(["cleanup-settings"], value);
      initializedAt.current = value.updated_at;
      setDraft(value.custom_prompt ?? value.default_prompt);
      setDirty(false);
      setSaveNotice(customPrompt == null ? "restored" : "saved");
    },
  });

  const runTest = useMutation({
    mutationFn: () => {
      if (!settings.data?.selected_provider_config_id) throw new Error("not_found");
      return testCleanup({
        provider_config_id: settings.data.selected_provider_config_id,
        text: sample,
        prompt_draft: draft,
      });
    },
  });

  const submitPrompt = (event: FormEvent) => {
    event.preventDefault();
    if (draft.trim()) savePrompt.mutate(draft);
  };
  const submitTest = (event: FormEvent) => {
    event.preventDefault();
    if (sample.trim()) runTest.mutate();
  };
  const selectedUsable = Boolean(
    settings.data?.selected_provider_config_id
      && ["bound", "not_required"].includes(settings.data.selected_credential_state ?? ""),
  );
  const error = settings.error ?? savePrompt.error ?? runTest.error;

  return <Card>
    <CardHeader>
      <CardTitle>{t("cleanup.prompt.title")}</CardTitle>
      <CardDescription>{t("cleanup.prompt.description")}</CardDescription>
    </CardHeader>
    <CardContent>
      {settings.isPending ? <div className="flex items-center gap-2 text-sm text-muted-foreground"><Spinner />{t("common.loading")}</div> : <Tabs defaultValue="view">
        <TabsList aria-label={t("cleanup.prompt.tabs_label")}>
          <TabsTrigger value="view">{t("cleanup.prompt.view")}</TabsTrigger>
          <TabsTrigger value="customize">{t("cleanup.prompt.customize")}</TabsTrigger>
          <TabsTrigger value="test">{t("cleanup.prompt.test")}</TabsTrigger>
        </TabsList>
        <TabsContent value="view">
          <FieldGroup>
            <Field>
              <FieldLabel htmlFor="cleanup-effective-prompt">{t("cleanup.prompt.semantic")}</FieldLabel>
              <Textarea id="cleanup-effective-prompt" readOnly value={settings.data?.custom_prompt ?? settings.data?.default_prompt ?? ""} rows={8} />
              <FieldDescription>{settings.data?.custom_prompt == null ? t("cleanup.prompt.using_default") : t("cleanup.prompt.using_custom")}</FieldDescription>
            </Field>
            <Field>
              <FieldLabel htmlFor="cleanup-protocol-prompt">{t("cleanup.prompt.protocol")}</FieldLabel>
              <Textarea id="cleanup-protocol-prompt" readOnly value={settings.data?.protocol_prompt ?? ""} rows={8} />
              <FieldDescription>{t("cleanup.prompt.protocol_hint")}</FieldDescription>
            </Field>
          </FieldGroup>
        </TabsContent>
        <TabsContent value="customize">
          <form onSubmit={submitPrompt}>
            <FieldGroup>
              <Field>
                <FieldLabel htmlFor="cleanup-prompt-draft">{t("cleanup.prompt.draft")}</FieldLabel>
                <Textarea id="cleanup-prompt-draft" value={draft} rows={12} maxLength={32768} required onChange={(event) => { setDraft(event.target.value); setDirty(true); setSaveNotice(null); }} />
                <FieldDescription>{t("cleanup.prompt.draft_hint")}</FieldDescription>
              </Field>
              <div className="flex flex-wrap gap-2">
                <Button type="submit" disabled={!dirty || !draft.trim() || savePrompt.isPending}>
                  {savePrompt.isPending && <Spinner data-icon="inline-start" />}{t("cleanup.prompt.save")}
                </Button>
                <Button type="button" variant="outline" disabled={savePrompt.isPending || settings.data?.custom_prompt == null} onClick={() => savePrompt.mutate(null)}>
                  {t("cleanup.prompt.restore")}
                </Button>
              </div>
              {saveNotice && <Alert><AlertTitle>{t(saveNotice === "saved" ? "cleanup.prompt.saved" : "cleanup.prompt.restored")}</AlertTitle></Alert>}
            </FieldGroup>
          </form>
        </TabsContent>
        <TabsContent value="test">
          <form onSubmit={submitTest}>
            <FieldGroup>
              <Field data-disabled={!selectedUsable || undefined}>
                <FieldLabel htmlFor="cleanup-test-input">{t("cleanup.prompt.test_input")}</FieldLabel>
                <Textarea id="cleanup-test-input" value={sample} rows={6} maxLength={65536} required disabled={!selectedUsable || runTest.isPending} onChange={(event) => setSample(event.target.value)} />
                <FieldDescription>{selectedUsable ? t("cleanup.prompt.test_hint") : t("cleanup.prompt.no_provider")}</FieldDescription>
              </Field>
              <Button type="submit" className="w-fit" disabled={!selectedUsable || !sample.trim() || !draft.trim() || runTest.isPending}>
                {runTest.isPending && <Spinner data-icon="inline-start" />}{runTest.isPending ? t("cleanup.prompt.testing") : t("cleanup.prompt.run_test")}
              </Button>
            </FieldGroup>
          </form>
          {runTest.data && <section className="mt-5 flex flex-col gap-4" aria-labelledby="cleanup-test-result" aria-live="polite">
            <div className="flex items-center justify-between gap-2">
              <h3 id="cleanup-test-result" className="font-medium">{t("cleanup.prompt.result")}</h3>
              <Badge variant="secondary">{t("cleanup.prompt.elapsed", { elapsed: runTest.data.elapsed_ms })}</Badge>
            </div>
            <Field>
              <FieldLabel htmlFor="cleanup-test-output">{t("cleanup.prompt.cleaned_text")}</FieldLabel>
              <Textarea id="cleanup-test-output" readOnly value={runTest.data.cleaned_text} rows={6} />
            </Field>
            {runTest.data.corrections.length > 0 ? <Table>
              <TableHeader><TableRow><TableHead>{t("cleanup.prompt.original")}</TableHead><TableHead>{t("cleanup.prompt.corrected")}</TableHead><TableHead>{t("cleanup.prompt.kind")}</TableHead></TableRow></TableHeader>
              <TableBody>{runTest.data.corrections.map((correction, index) => <TableRow key={`${correction.original_text}-${index}`}><TableCell>{correction.original_text}</TableCell><TableCell>{correction.corrected_text}</TableCell><TableCell>{t(correctionKindLabel[correction.kind])}</TableCell></TableRow>)}</TableBody>
            </Table> : <p className="text-sm text-muted-foreground">{t("cleanup.prompt.no_corrections")}</p>}
          </section>}
        </TabsContent>
      </Tabs>}
      {error && <Alert variant="destructive" className="mt-4"><AlertTitle>{t("cleanup.prompt.error")}</AlertTitle><AlertDescription>{localizedError(t, error)}</AlertDescription></Alert>}
    </CardContent>
  </Card>;
}
