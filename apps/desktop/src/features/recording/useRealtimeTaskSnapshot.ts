import { useEffect, useState } from "react";
import {
  onRealtimeTaskSnapshot,
  realtimeTaskSnapshot,
  type RealtimeTaskSnapshot,
} from "@/api/transport";

export const idleRealtimeTaskSnapshot: RealtimeTaskSnapshot = {
  generation: 0,
  revision: 0,
  task_id: null,
  phase: "idle",
  session_id: null,
  clipboard_context_count: 0,
  auto_paste_enabled: false,
  failure_code: null,
  fallback: "none",
};

function acceptSnapshot(current: RealtimeTaskSnapshot, incoming: RealtimeTaskSnapshot, initialized = true): RealtimeTaskSnapshot {
  // 新 generation 永远胜过旧 worker；同代仅接受严格递增 revision。
  if (!initialized || incoming.generation > current.generation
    || (incoming.generation === current.generation && incoming.revision > current.revision)) {
    return incoming;
  }
  return current;
}

export function useRealtimeTaskSnapshot(): RealtimeTaskSnapshot {
  const [snapshot, setSnapshot] = useState(idleRealtimeTaskSnapshot);

  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | undefined;
    let initialized = false;

    const apply = (next: RealtimeTaskSnapshot) => {
      if (active) setSnapshot((current) => {
        const accepted = acceptSnapshot(current, next, initialized);
        if (accepted !== current) initialized = true;
        return accepted;
      });
    };

    const refresh = async () => {
      try {
        apply(await realtimeTaskSnapshot());
      } catch {
        // 保持最后一个安全快照；下一轮继续尝试，覆盖胶囊窗口刚创建时的 IPC 竞态。
      }
    };

    void (async () => {
      // 必须先完成 listen 注册，再查询快照，避免查询和首个事件之间丢状态。
      try {
        unlisten = await onRealtimeTaskSnapshot(apply);
        if (!active) {
          unlisten();
          return;
        }
        await refresh();
      } catch {
        // 保持最后一个安全快照；监听/查询会在下次 WebView 挂载时重新建立。
      }
    })();

    // 独立胶囊可能在生产入口触发后才完成 WebView 初始化；事件若早于监听注册，
    // 轮询仍能读到原生保存的权威快照，避免窗口已显示但内容长期停留为空白。
    const timer = window.setInterval(() => void refresh(), 250);

    return () => {
      active = false;
      window.clearInterval(timer);
      unlisten?.();
    };
  }, []);

  return snapshot;
}

export { acceptSnapshot };
