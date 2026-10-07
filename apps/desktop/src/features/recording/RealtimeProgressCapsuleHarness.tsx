import { useCallback } from "react";
import { resizeRealtimeProgressCapsule } from "@/api/transport";
import { RealtimeProgressCapsule } from "./RealtimeProgressCapsule";

/** 胶囊 WebView 的窗口适配器：内容尺寸变化时同步调整原生窗口。 */
export function RealtimeProgressCapsuleHarness() {
  const resizeWindow = useCallback((width: number, height: number, generation: number, revision: number) => {
    void resizeRealtimeProgressCapsule(width, height, generation, revision).catch((error) => {
      console.warn("无法按进度胶囊内容调整窗口尺寸", error);
    });
  }, []);

  return <main className="flex h-screen w-full items-center justify-center text-foreground"><RealtimeProgressCapsule onResize={resizeWindow} /></main>;
}
