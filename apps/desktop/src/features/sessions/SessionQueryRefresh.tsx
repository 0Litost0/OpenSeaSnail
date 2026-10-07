import { useEffect, useRef } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useRealtimeTaskSnapshot } from "@/features/recording/useRealtimeTaskSnapshot";

/** 只读同步器：原生任务事件只触发历史查询刷新，不提交、轮询任务或执行注入。 */
export function SessionQueryRefresh() {
  const client = useQueryClient();
  const snapshot = useRealtimeTaskSnapshot();
  const previous = useRef({ sessionId: snapshot.session_id, phase: snapshot.phase });
  const currentSnapshot = useRef(snapshot);
  currentSnapshot.current = snapshot;

  const refresh = () => {
    void client.invalidateQueries({ queryKey: ["sessions"] });
    void client.invalidateQueries({ queryKey: ["sessions-search"] });
    if (currentSnapshot.current.session_id) {
      void client.invalidateQueries({ queryKey: ["session"] });
      void client.invalidateQueries({ queryKey: ["session-workspace-detail"] });
      void client.invalidateQueries({ queryKey: ["session-cleanup-detail"] });
    }
  };

  useEffect(() => {
    // 挂载时刷新一次，覆盖 WebView 未挂载期间漏掉的实时事件。
    refresh();
    const onVisible = () => { if (document.visibilityState === "visible") refresh(); };
    document.addEventListener("visibilitychange", onVisible);
    return () => document.removeEventListener("visibilitychange", onVisible);
  }, []);

  useEffect(() => {
    const changed = previous.current.sessionId !== snapshot.session_id || previous.current.phase !== snapshot.phase;
    previous.current = { sessionId: snapshot.session_id, phase: snapshot.phase };
    if (changed && (snapshot.session_id !== null || ["completed", "failed"].includes(snapshot.phase))) refresh();
  }, [snapshot.phase, snapshot.session_id]);

  return null;
}
