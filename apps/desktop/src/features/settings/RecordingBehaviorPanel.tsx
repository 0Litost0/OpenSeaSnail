import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { localizedError } from "@/api/errors";
import {
  recordingBehaviorStatus,
  setAutoPasteEnabled,
  setKeepTranscriptionInClipboard,
  type RecordingBehaviorStatus,
} from "@/api/transport";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Field, FieldContent, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Skeleton } from "@/components/ui/skeleton";
import { useI18n } from "@/i18n/I18nProvider";

export function RecordingBehaviorPanel() {
  const { t } = useI18n();
  const queryClient = useQueryClient();
  const behavior = useQuery({
    queryKey: ["recording-behavior-status"],
    queryFn: recordingBehaviorStatus,
  });
  const update = useMutation({
    mutationFn: setAutoPasteEnabled,
    onSuccess: (data) =>
      queryClient.setQueryData<RecordingBehaviorStatus>(["recording-behavior-status"], data),
  });
  const clipboardUpdate = useMutation({
    mutationFn: setKeepTranscriptionInClipboard,
    onSuccess: (data) =>
      queryClient.setQueryData<RecordingBehaviorStatus>(["recording-behavior-status"], data),
  });
  const enabled = behavior.data?.auto_paste_enabled ?? true;
  const keepTranscription = behavior.data?.keep_transcription_in_clipboard ?? true;

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("recording.behavior.title")}</CardTitle>
        <CardDescription>{t("recording.behavior.description")}</CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        {behavior.isPending ? (
          <Skeleton className="h-12 w-full" />
        ) : (
          <div className="flex flex-col gap-4">
          <Field orientation="horizontal" data-disabled={update.isPending || undefined}>
            <Checkbox
              id="auto-paste-enabled"
              checked={enabled}
              disabled={update.isPending}
              onCheckedChange={(checked) => update.mutate(checked === true)}
            />
            <FieldContent>
              <FieldLabel htmlFor="auto-paste-enabled">{t("recording.behavior.auto_paste")}</FieldLabel>
              <FieldDescription>{t("recording.behavior.auto_paste.description")}</FieldDescription>
            </FieldContent>
          </Field>
          <Field orientation="horizontal" data-disabled={clipboardUpdate.isPending || undefined}>
            <Checkbox
              id="keep-transcription-in-clipboard"
              checked={keepTranscription}
              disabled={clipboardUpdate.isPending}
              onCheckedChange={(checked) => clipboardUpdate.mutate(checked === true)}
            />
            <FieldContent>
              <FieldLabel htmlFor="keep-transcription-in-clipboard">{t("recording.behavior.keep_clipboard")}</FieldLabel>
              <FieldDescription>{t("recording.behavior.keep_clipboard.description")}</FieldDescription>
            </FieldContent>
          </Field>
          </div>
        )}
        {behavior.error || update.error || clipboardUpdate.error ? (
          <Alert variant="destructive">
            <AlertTitle>{t("recording.behavior.update_failed")}</AlertTitle>
            <AlertDescription>{localizedError(t, update.error ?? clipboardUpdate.error ?? behavior.error)}</AlertDescription>
          </Alert>
        ) : null}
      </CardContent>
    </Card>
  );
}
