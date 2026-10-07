import { useRef } from "react";
import { AlertCircleIcon, AudioLinesIcon, CheckIcon, ClipboardIcon, InfoIcon, MicIcon, PaperclipIcon, SparklesIcon, UploadIcon, type LucideIcon } from "lucide-react";
import type { RealtimeTaskPhase, RealtimeTaskSnapshot } from "@/api/transport";
import { Progress } from "@/components/ui/progress";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { cn } from "@/lib/utils";
import { useI18n } from "@/i18n/I18nProvider";
import type { TranslationKey } from "@/i18n/dictionaries";
import { CAPSULE_NORMAL_WIDTH, CAPSULE_HEIGHT, CAPSULE_NOTICE_HEIGHT, CAPSULE_NOTICE_WIDTH, useCapsuleContentSize } from "./useCapsuleContentSize";
import { useRealtimeContextCount } from "./useRealtimeContextCount";
import { useRealtimeTaskSnapshot } from "./useRealtimeTaskSnapshot";

type ActivePhase = Exclude<RealtimeTaskPhase, "idle" | "preparing" | "completed" | "failed">;
type Stage = { phase: ActivePhase; icon: LucideIcon };

const baseStages: Stage[] = [
  { phase: "recording", icon: MicIcon },
  { phase: "submitting", icon: UploadIcon },
  { phase: "transcribing", icon: AudioLinesIcon },
  { phase: "cleaning_up", icon: SparklesIcon },
];
const phaseLabels: Record<ActivePhase, TranslationKey> = {
  recording: "recording.phase_recording",
  submitting: "recording.phase_submitting",
  transcribing: "recording.phase_transcribing",
  cleaning_up: "recording.phase_cleaning_up",
  auto_pasting: "recording.phase_auto_pasting",
};

export function stagesFor(autoPasteEnabled: boolean): Stage[] {
  return autoPasteEnabled ? [...baseStages, { phase: "auto_pasting", icon: ClipboardIcon }] : baseStages;
}

export function failedAt(snapshot: RealtimeTaskSnapshot): ActivePhase {
  switch (snapshot.failure_code) {
    case "recording_device_error":
    case "recording_microphone_unavailable":
    case "recording_microphone_unauthorized":
      return "recording";
    case "recording_no_audio":
    case "recording_context_invalid":
    case "recording_submit_failed":
    case "recording_submit_duplicate":
      return "submitting";
    case "recording_accessibility_required":
    case "recording_auto_paste_failed":
    case "clipboard_restore_failed":
      return "auto_pasting";
    case "recording_stale_worker":
      return snapshot.fallback === "history" ? "auto_pasting" : "submitting";
    case "recording_cancelled":
    case "recording_status_failed":
    case "recording_status_invalid":
    case "recording_transcription_failed":
      return "transcribing";
    case "connection":
      return snapshot.session_id ? "transcribing" : "submitting";
    case "recording_failed":
    case null:
      return snapshot.session_id ? "transcribing" : "submitting";
    default:
      return snapshot.session_id ? "transcribing" : "submitting";
  }
}

export function failureLabelKey(snapshot: RealtimeTaskSnapshot): TranslationKey {
  if (snapshot.failure_code === "no_speech_detected" || snapshot.failure_code === "recording_no_audio") {
    return "recording.notice_no_speech";
  }
  if (snapshot.fallback === "clipboard") {
    return snapshot.failure_code === "recording_accessibility_required"
      ? "recording.failure_accessibility_clipboard"
      : "recording.failure_manual_paste";
  }
  if (snapshot.fallback === "history") {
    if (failedAt(snapshot) === "auto_pasting") return "recording.failure_copy_history";
    return snapshot.failure_code === "connection"
      ? "recording.failure_history_wait"
      : "recording.failure_history_status";
  }
  if (snapshot.failure_code === "recording_microphone_unauthorized") {
    return "recording.failure_microphone";
  }
  if (snapshot.failure_code === "recording_microphone_unavailable") {
    return "recording.failure_microphone_unavailable";
  }
  return "recording.failure_retry";
}

export function isHintLevelFailure(snapshot: RealtimeTaskSnapshot): boolean {
  return snapshot.phase === "failed" && (snapshot.failure_code === "no_speech_detected" || snapshot.failure_code === "recording_no_audio");
}

export function segmentValue(snapshot: RealtimeTaskSnapshot, stage: ActivePhase, stages: Stage[]): number | undefined {
  if (snapshot.phase === "completed") return 100;
  if (snapshot.phase === "idle") return 0;
  const current = snapshot.phase === "failed" ? failedAt(snapshot) : snapshot.phase;
  const currentIndex = stages.findIndex((item) => item.phase === current);
  const stageIndex = stages.findIndex((item) => item.phase === stage);
  if (stageIndex < currentIndex) return 100;
  if (stageIndex > currentIndex) return 0;
  return snapshot.phase === "failed" ? 50 : undefined;
}

interface RealtimeProgressCapsuleProps {
  onSize?: (width: number, height: number) => void;
  onResize?: (width: number, height: number, generation: number, revision: number) => void;
}

export function RealtimeProgressCapsule({ onSize, onResize }: RealtimeProgressCapsuleProps = {}) {
  const snapshot = useRealtimeTaskSnapshot();
  const contextCount = useRealtimeContextCount(snapshot);
  return <RealtimeProgressCapsuleView snapshot={snapshot} contextCount={contextCount} onSize={onSize} onResize={onResize} />;
}

interface RealtimeProgressCapsuleViewProps extends RealtimeProgressCapsuleProps {
  snapshot: RealtimeTaskSnapshot;
  contextCount: number;
}

/** 只读胶囊视图；生产订阅器与隔离 fixture harness 共用同一渲染路径。 */
export function RealtimeProgressCapsuleView({ snapshot, contextCount, onSize, onResize }: RealtimeProgressCapsuleViewProps) {
  const { t } = useI18n();
  const rootRef = useRef<HTMLElement>(null);
  const pillRef = useRef<HTMLDivElement>(null);
  const failed = snapshot.phase === "failed";
  const hint = isHintLevelFailure(snapshot);
  const stages = stagesFor(snapshot.auto_paste_enabled);
  const activeStage = snapshot.phase === "failed" ? failedAt(snapshot) : snapshot.phase === "completed" ? null : snapshot.phase === "preparing" ? "recording" : snapshot.phase as ActivePhase;
  const activeStageIcon = activeStage ? stages.find(({ phase }) => phase === activeStage)?.icon : null;
  const ActiveStageIcon = activeStageIcon;
  const label = failed
    ? t(failureLabelKey(snapshot))
    : snapshot.phase === "completed"
      ? t("recording.task_completed")
      : snapshot.phase === "idle"
        ? t("recording.idle")
        : snapshot.phase === "preparing" ? t("recording.phase_preparing") : t(phaseLabels[snapshot.phase as ActivePhase]);

  useCapsuleContentSize(pillRef, (width) => {
    const requestedWidth = failed ? CAPSULE_NOTICE_WIDTH : CAPSULE_NORMAL_WIDTH;
    const requestedHeight = failed ? CAPSULE_NOTICE_HEIGHT : CAPSULE_HEIGHT;
    rootRef.current?.style.setProperty("--capsule-pill-width", `${failed ? width : CAPSULE_NORMAL_WIDTH}px`);
    onSize?.(requestedWidth, requestedHeight);
    onResize?.(requestedWidth, requestedHeight, snapshot.generation, snapshot.revision);
  }, [snapshot.task_id, snapshot.generation, snapshot.revision, snapshot.phase, snapshot.failure_code, snapshot.fallback, contextCount, onSize, onResize, failed]);

  if (snapshot.phase === "idle") return null;

  return <section
    ref={rootRef}
    role={failed ? undefined : "status"}
    aria-live={failed ? undefined : "off"}
    aria-label={failed ? undefined : label}
    className="pointer-events-none flex flex-col items-center"
    style={{ width: failed ? CAPSULE_NOTICE_WIDTH : CAPSULE_NORMAL_WIDTH, height: failed ? CAPSULE_NOTICE_HEIGHT : CAPSULE_HEIGHT }}
  >
    {failed && <div data-capsule-notice className="flex h-14 w-80 flex-col items-center drop-shadow-sm">
      <Alert
        variant={hint ? "default" : "destructive"}
        role="status"
        aria-live="polite"
        aria-label={label}
        className={cn("h-12 w-80 overflow-hidden rounded-xl px-3 py-2", hint ? "bg-card" : "bg-destructive/10")}
      >
        {hint ? <InfoIcon aria-hidden="true" /> : <AlertCircleIcon aria-hidden="true" />}
        <AlertDescription className="line-clamp-2 text-xs leading-4 text-inherit">{label}</AlertDescription>
      </Alert>
      <span
        data-capsule-tail
        aria-hidden="true"
        className={cn("block h-2 w-4", hint ? "bg-card" : "bg-destructive/10")}
        style={{ clipPath: "polygon(0 0, 100% 0, 50% 100%)" }}
      />
    </div>}
    <div
      ref={pillRef}
      data-capsule-pill
      aria-hidden={failed ? "true" : undefined}
      className={cn("flex h-10 min-w-28 items-center gap-x-2 overflow-hidden rounded-full border bg-background px-3 py-2 shadow-sm", failed && !hint && "border-destructive/30 text-destructive")}
      style={{ width: failed ? "var(--capsule-pill-width, 120px)" : CAPSULE_NORMAL_WIDTH }}
    >
      {failed ? hint ? <InfoIcon aria-hidden="true" className="shrink-0" /> : <AlertCircleIcon aria-hidden="true" className="shrink-0" /> : snapshot.phase === "completed" ? <CheckIcon aria-hidden="true" className="shrink-0" /> : ActiveStageIcon ? <ActiveStageIcon aria-hidden="true" className="shrink-0" /> : null}
      {snapshot.phase === "preparing" ? <span className="text-xs">{label}</span> : <div className="flex min-w-0 flex-1 items-center gap-0.5" aria-hidden="true">
        {stages.map(({ phase }) => {
          const value = segmentValue(snapshot, phase, stages);
          return <Progress key={phase} data-stage={phase} value={value} className={cn("min-w-3 flex-1", failed && !hint && value === 50 && "[&_[data-slot=progress-indicator]]:bg-destructive")} />;
        })}
      </div>}
      <span className="flex shrink-0 items-center gap-1 tabular-nums text-xs" aria-label={t("recording.context_count", { count: contextCount })}><PaperclipIcon aria-hidden="true" />{contextCount}</span>
    </div>
  </section>;
}
