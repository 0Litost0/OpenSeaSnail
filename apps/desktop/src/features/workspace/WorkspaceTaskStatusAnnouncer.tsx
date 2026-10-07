import { useI18n } from "@/i18n/I18nProvider";
import { failureLabelKey } from "@/features/recording/RealtimeProgressCapsule";
import { useRealtimeTaskSnapshot } from "@/features/recording/useRealtimeTaskSnapshot";

/** 主工作台的等价隐藏反馈；胶囊处于独立 NSPanel 时仍能提供失败语义。 */
export function WorkspaceTaskStatusAnnouncer() {
  const { t } = useI18n();
  const snapshot = useRealtimeTaskSnapshot();
  if (snapshot.phase !== "failed") return <p className="sr-only" role="status" aria-live="polite" />;
  return <p className="sr-only" role="status" aria-live="polite">{t(failureLabelKey(snapshot))}</p>;
}
