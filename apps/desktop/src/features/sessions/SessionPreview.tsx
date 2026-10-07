import { useEffect, useRef, useState } from "react";
import { FileIcon } from "lucide-react";
import type { SessionList } from "@/api/sessions";
import { type ContextItem } from "@/api/transport";
import { useI18n } from "@/i18n/I18nProvider";
import { useSessionWorkspaceDetail } from "./queries";

/** 工作台列表专用：正文摘要与上下文各占一行，避免上下文被正文截断隐藏。 */
export function SessionPreview({ sessionId, preview, status }: { sessionId: string; preview?: string; status: SessionList["items"][number]["status"] }) {
  const { t } = useI18n();
  const ref = useRef<HTMLSpanElement>(null);
  const [visible, setVisible] = useState(false);
  useEffect(() => {
    if (!ref.current || typeof IntersectionObserver === "undefined") { setVisible(true); return; }
    const observer = new IntersectionObserver(([entry]) => {
      if (entry.isIntersecting) { setVisible(true); observer.disconnect(); }
    });
    observer.observe(ref.current);
    return () => observer.disconnect();
  }, []);
  const detail = useSessionWorkspaceDetail(sessionId, visible && status === "completed");
  const contexts = detail.data ? [...detail.data.display_items.filter((item): item is ContextItem => item.kind !== "transcript" && item.kind !== "final_text"), ...detail.data.separate_contexts] : [];
  return <span ref={ref} className="flex min-w-0 max-w-64 flex-col gap-1">
    <span className="truncate" title={preview}>{preview || t(`sessions.status.${status}`)}</span>
    <span id={`preview-context-${sessionId}`} className="flex min-w-0 flex-col gap-1 text-xs text-muted-foreground">
      {contexts.slice(0, 3).map((item) => <span key={item.sequence} className="flex min-w-0 items-center gap-1" aria-label={t("sessions.context.label")}>
        {item.kind === "context_image" || item.kind === "context_file" ? <><FileIcon aria-hidden="true" /><span className="line-clamp-2 whitespace-pre-wrap break-all" title={item.resources.map((resource) => resource.path).join("\n")}>{item.resources.map((resource) => resource.path).join("\n")}</span></>
          : <span className="line-clamp-2 whitespace-normal break-all" title={item.kind === "context_rich_text" ? item.plain_text : item.kind === "context_link" ? item.url : item.text}>“{item.kind === "context_rich_text" ? item.plain_text : item.kind === "context_link" ? item.url : item.text}”</span>}
      </span>)}
      {contexts.length > 3 && <span>{t("sessions.context.more", { count: contexts.length - 3 })}</span>}
      {(detail.isError || detail.data?.context_degraded) && <span>{t("sessions.context.degraded")}</span>}
    </span>
  </span>;
}
