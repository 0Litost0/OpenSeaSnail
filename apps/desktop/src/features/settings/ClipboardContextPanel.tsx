import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ClipboardContextStatus, clipboardContextStatus, setClipboardContextEnabled } from "@/api/transport";
import { localizedError } from "@/api/errors";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Field, FieldContent, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Skeleton } from "@/components/ui/skeleton";
import { useI18n } from "@/i18n/I18nProvider";

export function ClipboardContextPanel() {
  const { t } = useI18n();
  const queryClient = useQueryClient();
  const settings = useQuery({ queryKey: ["clipboard-context-status"], queryFn: clipboardContextStatus });
  const update = useMutation({
    mutationFn: setClipboardContextEnabled,
    onSuccess: (data) => queryClient.setQueryData<ClipboardContextStatus>(["clipboard-context-status"], data),
  });
  const enabled = settings.data?.enabled ?? true;

  return <Card>
    <CardHeader>
      <CardTitle>{t("clipboard.title")}</CardTitle>
      <CardDescription>{t("clipboard.description")}</CardDescription>
    </CardHeader>
    <CardContent className="flex flex-col gap-4">
      {settings.isPending ? <Skeleton className="h-12 w-full" /> : <Field orientation="horizontal" data-disabled={update.isPending || undefined}>
        <Checkbox
          id="clipboard-context-enabled"
          checked={enabled}
          disabled={update.isPending}
          onCheckedChange={(checked) => update.mutate(checked === true)}
        />
        <FieldContent>
          <FieldLabel htmlFor="clipboard-context-enabled">{t("clipboard.capture")}</FieldLabel>
          <FieldDescription>{t("clipboard.hint")}</FieldDescription>
        </FieldContent>
      </Field>}
      {enabled ? <Alert><AlertTitle>{t("clipboard.image.title")}</AlertTitle><AlertDescription>{t("clipboard.image.description")}</AlertDescription></Alert> : null}
      {settings.error || update.error ? <Alert variant="destructive"><AlertTitle>{t("clipboard.update_failed")}</AlertTitle><AlertDescription>{localizedError(t, update.error ?? settings.error)}</AlertDescription></Alert> : null}
    </CardContent>
  </Card>;
}
