import { useCallback, useEffect, useMemo, useState } from "react";
import type { RealtimeTaskSnapshot } from "@/api/transport";
import { resizeRealtimeProgressCapsule } from "@/api/transport";
import { I18nProvider, type AppLocale } from "@/i18n/I18nProvider";
import { RealtimeProgressCapsuleView } from "./RealtimeProgressCapsule";
import { realtimeTaskFixtures } from "./realtimeTaskFixtures";

export const CAPSULE_SMOKE_STEP_MS = 3_000;

function fixture(
  source: RealtimeTaskSnapshot,
  revision: number,
  taskId: string | null,
  overrides: Partial<RealtimeTaskSnapshot> = {},
): RealtimeTaskSnapshot {
  return { ...source, revision, task_id: taskId, ...overrides };
}

/**
 * 确定性的真机冒烟序列：包含 cleanup 的完整任务、终态、idle、关闭自动粘贴的连续任务，
 * 最后循环展示无语音提示、连续相同错误和长 clipboard fallback，
 * 以两种语言检查 360×104 信息卡片。这里只注入只读 DTO，
 * 不调用录音、提交、轮询或文本注入入口。
 */
export const capsuleSmokeSequence: RealtimeTaskSnapshot[] = [
  fixture(realtimeTaskFixtures.recording, 1, "smoke-task-1", { clipboard_context_count: 12 }),
  fixture(realtimeTaskFixtures.submitting, 2, "smoke-task-1", { clipboard_context_count: 12 }),
  fixture(realtimeTaskFixtures.transcribing, 3, "smoke-task-1", { clipboard_context_count: 12 }),
  fixture(realtimeTaskFixtures.cleaning_up, 4, "smoke-task-1", { clipboard_context_count: 12 }),
  fixture(realtimeTaskFixtures.auto_pasting, 5, "smoke-task-1", { clipboard_context_count: 12 }),
  fixture(realtimeTaskFixtures.completed, 6, "smoke-task-1", { clipboard_context_count: 12 }),
  fixture(realtimeTaskFixtures.idle, 7, null),
  fixture(realtimeTaskFixtures.recording, 8, "smoke-task-2", { auto_paste_enabled: false, clipboard_context_count: 0 }),
  fixture(realtimeTaskFixtures.submitting, 9, "smoke-task-2", { auto_paste_enabled: false, clipboard_context_count: 0 }),
  fixture(realtimeTaskFixtures.transcribing, 10, "smoke-task-2", { auto_paste_enabled: false, clipboard_context_count: 0 }),
  fixture(realtimeTaskFixtures.cleaning_up, 11, "smoke-task-2", { auto_paste_enabled: false, clipboard_context_count: 0 }),
  fixture(realtimeTaskFixtures.completed, 12, "smoke-task-2", { auto_paste_enabled: false, clipboard_context_count: 0 }),
  fixture(realtimeTaskFixtures.failed, 13, "smoke-task-3", { failure_code: "no_speech_detected", fallback: "none" }),
  fixture(realtimeTaskFixtures.recording, 14, "smoke-task-4"),
  fixture(realtimeTaskFixtures.failed, 15, "smoke-task-4", { failure_code: "recording_no_audio", fallback: "none" }),
  fixture(realtimeTaskFixtures.failed, 16, "smoke-task-5", { clipboard_context_count: 123 }),
  fixture(realtimeTaskFixtures.failed, 17, "smoke-task-6", { clipboard_context_count: 123 }),
];

export function initialCapsuleSmokeStep(search: string): number {
  const requested = Number(new URLSearchParams(search).get("smokeStep"));
  return Number.isInteger(requested) && requested >= 1 && requested <= capsuleSmokeSequence.length
    ? requested - 1
    : 0;
}

export function RealtimeProgressCapsuleSmokeHarness() {
  const [step, setStep] = useState(() => initialCapsuleSmokeStep(window.location.search));
  const snapshot = capsuleSmokeSequence[step] ?? capsuleSmokeSequence[0];
  const locale: AppLocale = step === capsuleSmokeSequence.length - 1 ? "zh-CN" : "en-US";
  const resizeWindow = useCallback((width: number, height: number, generation: number, revision: number) => {
    void resizeRealtimeProgressCapsule(width, height, generation, revision).catch((error) => {
      console.warn("无法按冒烟 fixture 调整进度胶囊窗口尺寸", error);
    });
  }, []);
  const contextCount = useMemo(() => snapshot.clipboard_context_count, [snapshot]);

  useEffect(() => {
    const timer = window.setInterval(() => {
      setStep((current) => (current + 1) % capsuleSmokeSequence.length);
    }, CAPSULE_SMOKE_STEP_MS);
    return () => window.clearInterval(timer);
  }, []);

  return <main
    data-capsule-smoke="true"
    data-smoke-step={`${step + 1}/${capsuleSmokeSequence.length}`}
    data-smoke-phase={snapshot.phase}
    data-smoke-task={snapshot.task_id ?? "none"}
    className="flex h-screen w-full items-center justify-center text-foreground"
  >
    <I18nProvider key={locale} localeOverride={locale}>
      <RealtimeProgressCapsuleView snapshot={snapshot} contextCount={contextCount} onResize={resizeWindow} />
    </I18nProvider>
  </main>;
}
