import { useEffect, useState } from "react";
import { MicIcon } from "lucide-react";
import { onRecordingError, requestMicrophonePermission } from "@/api/transport";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { useI18n } from "@/i18n/I18nProvider";

export function PermissionReminder({ onOpenSettings }: { onOpenSettings?: () => void } = {}) {
  const { t } = useI18n();
  const [reason, setReason] = useState<"permission" | "unavailable" | null>(null);
  const [requesting, setRequesting] = useState(false);
  useEffect(() => {
    let dispose: (() => void) | undefined;
    void onRecordingError((message) => {
      if (message === "recording_microphone_unauthorized") setReason("permission");
      if (message === "recording_microphone_unavailable") setReason("unavailable");
    }).then((unlisten) => { dispose = unlisten; });
    return () => dispose?.();
  }, []);
  if (!reason) return null;
  const request = async () => { setRequesting(true); try { const result = await requestMicrophonePermission(); if (result.granted) setReason(null); } finally { setRequesting(false); } };
  const unavailable = reason === "unavailable";
  return <Alert><MicIcon /><AlertTitle>{t(unavailable ? "reminder.microphone_unavailable_title" : "reminder.title")}</AlertTitle><AlertDescription><div className="flex flex-wrap items-center gap-3"><span>{t(unavailable ? "reminder.microphone_unavailable_description" : "reminder.description")}</span>{!unavailable && <><Button size="sm" variant="outline" onClick={() => onOpenSettings?.()}>{t("reminder.open_settings")}</Button><Button size="sm" variant="secondary" onClick={() => void request()} disabled={requesting}>{requesting ? t("onboarding.microphone.requesting") : t("permission.request")}</Button></>}<Button size="sm" variant="ghost" onClick={() => setReason(null)}>{t("reminder.later")}</Button></div></AlertDescription></Alert>;
}
