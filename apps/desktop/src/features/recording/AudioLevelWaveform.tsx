const multipliers = [0.42, 0.78, 1, 0.68, 0.36];
import { useI18n } from "@/i18n/I18nProvider";

/** 将原生层 100ms 音量快照映射为紧凑声波；不传递任何音频样本给 WebView。 */
export function AudioLevelWaveform({ level, active }: { level: number; active: boolean }) {
  const { t } = useI18n();
  const normalized = Math.min(1, Math.max(0, level));
  return <div className="flex h-5 shrink-0 items-center gap-0.5" role="img" aria-label={active ? t("recording.wave_active", { volume: Math.round(normalized * 100) }) : t("recording.wave_inactive")}>{multipliers.map((multiplier, index) => <span key={index} className={`w-0.5 rounded-full transition-[height,background-color] duration-100 ${active ? "bg-primary" : "bg-muted-foreground/40"}`} style={{ height: `${Math.round(3 + normalized * 17 * multiplier)}px` }} />)}</div>;
}
