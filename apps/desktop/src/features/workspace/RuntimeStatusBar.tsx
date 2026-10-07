import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { CircleCheckIcon, CircleIcon, MicIcon, PlugZapIcon } from "lucide-react";
import { daemonStatus, onRecordingStatus, permissionStatus, recordingStatus, type RecordingStatus } from "@/api/transport";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { useI18n } from "@/i18n/I18nProvider";

const DAEMON_PROBE_TIMEOUT_MS = 250;

const initialRecordingStatus: Pick<RecordingStatus, "is_recording"> = { is_recording: false };

export function probeDaemonStatus() {
  let timer: number | undefined;
  const timeout = new Promise<never>((_, reject) => {
    timer = window.setTimeout(() => reject(new Error("daemon probe timeout")), DAEMON_PROBE_TIMEOUT_MS);
  });
  return Promise.race([daemonStatus(), timeout]).finally(() => {
    if (timer !== undefined) window.clearTimeout(timer);
  });
}

export function RuntimeStatusBar({ onOpenSettings }: { onOpenSettings?: () => void } = {}) {
  const { t } = useI18n();
  const queryClient = useQueryClient();
  const daemon = useQuery({
    queryKey: ["daemon-status"],
    queryFn: probeDaemonStatus,
    refetchInterval: 2_000,
    refetchOnWindowFocus: false,
  });
  const permissions = useQuery({ queryKey: ["permissions-status"], queryFn: permissionStatus, refetchInterval: 5_000, refetchOnWindowFocus: false });
  const [recording, setRecording] = useState(initialRecordingStatus);

  useEffect(() => {
    let active = true;
    void recordingStatus().then((status) => active && setRecording(status)).catch(() => undefined);
    let dispose: (() => void) | undefined;
    void onRecordingStatus((status) => active && setRecording(status)).then((unlisten) => {
      if (active) dispose = unlisten;
      else unlisten();
    }).catch(() => undefined);
    return () => {
      active = false;
      dispose?.();
    };
  }, []);

  useEffect(() => {
    const refresh = () => {
      if (document.visibilityState === "visible") void queryClient.invalidateQueries({ queryKey: ["daemon-status"] });
    };
    document.addEventListener("visibilitychange", refresh);
    return () => document.removeEventListener("visibilitychange", refresh);
  }, [queryClient]);

  const daemonConnected = daemon.data?.connected === true && daemon.data.authenticated === true;
  return (
    <footer className="flex min-h-9 shrink-0 items-center justify-between gap-2 border-t px-3 py-1.5 text-xs text-muted-foreground" aria-label={t("runtime.title")}>
      <div className="flex min-w-0 items-center gap-2">
        <Badge variant="outline" className="gap-1">
          <MicIcon aria-hidden="true" />
          <span>{recording.is_recording ? t("runtime.recording") : t("runtime.ready")}</span>
        </Badge>
        {permissions.data?.microphone.granted === false && <Button size="sm" variant="destructive" className="h-6 px-2 text-xs" onClick={onOpenSettings}>{t("runtime.microphone_required")}</Button>}
        <Badge variant={daemonConnected ? "outline" : "destructive"} className="gap-1">
          {daemonConnected ? <CircleCheckIcon aria-hidden="true" /> : <PlugZapIcon aria-hidden="true" />}
          <span>{daemonConnected ? t("runtime.connected") : t("runtime.unavailable")}</span>
        </Badge>
      </div>
      {!daemon.isFetching && daemon.data === undefined && <CircleIcon className="size-3 animate-pulse" aria-hidden="true" />}
    </footer>
  );
}
