import { useEffect, useState } from "react";
import { onRecordingStatus, recordingStatus, type RealtimeTaskSnapshot } from "@/api/transport";

export function useRealtimeContextCount(snapshot: RealtimeTaskSnapshot): number {
  const [live, setLive] = useState<{ taskId: string | null; count: number }>({ taskId: null, count: 0 });
  useEffect(() => {
    if (snapshot.phase !== "recording") return;
    let active = true;
    let unlisten: (() => void) | undefined;
    let poller: number | undefined;
    const taskId = snapshot.task_id;
    setLive({ taskId, count: 0 });
    const refresh = async () => {
      try {
        const status = await recordingStatus();
        if (active) setLive({ taskId, count: status.clipboard_context_count });
      } catch {
        // The event stream remains the primary source when status polling is unavailable.
      }
    };
    void (async () => {
      try {
        unlisten = await onRecordingStatus((status) => {
          if (active) setLive({ taskId, count: status.clipboard_context_count });
        });
      } catch {
        // 隔离窗口可能无法注册事件；下面的命令轮询仍必须继续。
      }
      if (!active) {
        unlisten?.();
        return;
      }
      await refresh();
      // The capsule is a separate webview; polling closes the event-delivery gap
      // when recording-status events are consumed by the main window first.
      poller = window.setInterval(() => void refresh(), 250);
    })();
    return () => {
      active = false;
      if (poller !== undefined) window.clearInterval(poller);
      unlisten?.();
    };
  }, [snapshot.phase, snapshot.task_id]);
  return snapshot.phase === "recording" ? (live.taskId === snapshot.task_id ? live.count : 0) : snapshot.clipboard_context_count;
}
