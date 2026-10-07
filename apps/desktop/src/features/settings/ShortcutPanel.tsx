import { useMutation, useQuery } from "@tanstack/react-query";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { recordingShortcut, setRecordingShortcut, setRecordingShortcutCapture } from "@/api/transport";
import { localizedError } from "@/api/errors";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field";
import { useI18n } from "@/i18n/I18nProvider";
import { displayShortcut, shortcutFromKey } from "./shortcutKeys";

const DEFAULT_SHORTCUT = "Command+Shift+Space";
export function ShortcutPanel() {
  const { t } = useI18n();
  const shortcut = useQuery({ queryKey: ["recording-shortcut"], queryFn: recordingShortcut });
  const [value, setValue] = useState("");
  const [editing, setEditing] = useState(false);
  const [starting, setStarting] = useState(false);
  const [captureError, setCaptureError] = useState<unknown>(null);
  const [invalid, setInvalid] = useState(false);
  const captureVersion = useRef(0);
  const update = useMutation({ mutationFn: (next: string) => setRecordingShortcut(next), onSuccess: (status) => {
    setValue(status.recording_shortcut);
  } });
  useEffect(() => { if (!editing) setValue(shortcut.data?.recording_shortcut ?? ""); }, [shortcut.data, editing]);
  const restore = () => {
    captureVersion.current += 1;
    setStarting(false);
    setEditing(false);
    setInvalid(false);
    void setRecordingShortcutCapture(false).catch(setCaptureError);
  };
  useEffect(() => () => {
    captureVersion.current += 1;
    void setRecordingShortcutCapture(false).catch(() => {});
  }, []);
  useEffect(() => {
    if (!editing && !starting) return;
    const blur = () => restore();
    window.addEventListener("blur", blur);
    return () => window.removeEventListener("blur", blur);
  }, [editing, starting]);
  // Attach before painting the capture UI so Escape also works while native
  // capture setup is in flight, and immediately when the recorder appears.
  useLayoutEffect(() => {
    if (!editing && !starting) return;
    const capture = (event: KeyboardEvent) => {
      if (update.isPending) return;
      if (event.key === "Escape") { event.preventDefault(); restore(); return; }
      if (!editing) return;
      if (["Meta", "Control", "Alt", "Shift"].includes(event.key)) return;
      // Leave Tab available for keyboard navigation; modified Tab can be captured.
      if (event.key === "Tab" && !event.metaKey && !event.ctrlKey && !event.altKey) return;
      if (event.target instanceof HTMLElement && event.target.closest("button")
        && (event.key === "Enter" || event.key === " ") && !event.metaKey && !event.ctrlKey && !event.altKey) return;
      event.preventDefault();
      event.stopPropagation();
      const next = shortcutFromKey(event);
      if (next) { setValue(next); setInvalid(false); } else if (!event.repeat) setInvalid(true);
    };
    window.addEventListener("keydown", capture, true);
    return () => window.removeEventListener("keydown", capture, true);
  }, [editing, starting, update.isPending]);
  const begin = async () => {
    const version = ++captureVersion.current;
    setCaptureError(null); update.reset(); setStarting(true);
    try {
      await setRecordingShortcutCapture(true);
      if (captureVersion.current !== version) { await setRecordingShortcutCapture(false); return; }
      setValue(""); setEditing(true);
    } catch (error) { if (captureVersion.current === version) setCaptureError(error); }
    finally { if (captureVersion.current === version) setStarting(false); }
  };
  const save = async (next: string) => {
    setCaptureError(null);
    try {
      const status = await update.mutateAsync(next);
      await shortcut.refetch();
      setValue(status.recording_shortcut);
      restore();
    } catch { /* Keep the candidate and original preference available for retry/cancel. */ }
  };
  const error = captureError ?? update.error ?? shortcut.error ?? shortcut.data?.registration_error;
  return <Card><CardHeader><CardTitle>{t("shortcut.title")}</CardTitle><CardDescription>{t("shortcut.description")}</CardDescription></CardHeader><CardContent>
    {error != null && <Alert variant="destructive"><AlertTitle>{t("shortcut.failed")}</AlertTitle><AlertDescription>{localizedError(t, error)}</AlertDescription></Alert>}
    <FieldGroup><Field data-invalid={invalid}>
      <FieldLabel htmlFor="shortcut-combination">{t("shortcut.label")}</FieldLabel>
      <output id="shortcut-combination" tabIndex={0} aria-invalid={invalid} aria-live="polite" aria-label={t("shortcut.label")}>{editing ? value ? displayShortcut(value) : t("shortcut.press") : displayShortcut(shortcut.data?.recording_shortcut ?? "")}</output>
      <FieldDescription>{invalid ? t("shortcut.invalid_hint") : editing ? t("shortcut.capture_hint") : t("shortcut.hint")}</FieldDescription>
      <div className="flex flex-wrap gap-2">
        {editing ? <><Button disabled={!value || update.isPending} onClick={() => void save(value)}>{update.isPending ? t("shortcut.updating") : t("shortcut.save")}</Button><Button variant="outline" disabled={update.isPending} onClick={restore}>{t("shortcut.cancel")}</Button></>
          : <><Button disabled={starting || update.isPending || !shortcut.data} onClick={() => void begin()}>{starting ? t("shortcut.starting") : t("shortcut.change")}</Button><Button variant="outline" disabled={starting || update.isPending || !shortcut.data} onClick={() => void save(DEFAULT_SHORTCUT)}>{t("shortcut.reset")}</Button></>}
      </div>
    </Field></FieldGroup>
  </CardContent></Card>;
}
