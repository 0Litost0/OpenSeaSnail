import { useEffect, useState, type ReactNode } from "react";
import { BookOpenIcon, BookTypeIcon, MessagesSquareIcon, SettingsIcon } from "lucide-react";
import seaSnailMark from "@/assets/brand/seasnail-mark.svg";
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarHeader,
  SidebarInset,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarProvider,
  SidebarTrigger,
} from "@/components/ui/sidebar";
import { TooltipProvider } from "@/components/ui/tooltip";
import { useI18n } from "@/i18n/I18nProvider";
import { useElementWidth } from "@/hooks/use-element-width";
import { RuntimeStatusBar } from "./RuntimeStatusBar";
import { WorkspaceTaskStatusAnnouncer } from "./WorkspaceTaskStatusAnnouncer";

export type WorkspaceView = "sessions" | "dictionary" | "settings" | "help";
export const WORKSPACE_EXPANDED_SIDEBAR_MIN_WIDTH = 1_040;

export function shouldUseIconSidebar(width: number | null): boolean {
  return width === null || width < WORKSPACE_EXPANDED_SIDEBAR_MIN_WIDTH;
}

interface WorkspaceShellProps {
  views: Record<WorkspaceView, ReactNode>;
  initialView?: WorkspaceView;
  view?: WorkspaceView;
  onViewChange?: (view: WorkspaceView) => void;
  onOpenSettings?: () => void;
}

/** 工作台壳：页面内容与底部运行状态使用独立布局边界。 */
export function WorkspaceShell({ views, initialView = "sessions", view: controlledView, onViewChange, onOpenSettings }: WorkspaceShellProps) {
  const { t } = useI18n();
  const { ref, width } = useElementWidth<HTMLDivElement>();
  const [view, setView] = useState<WorkspaceView>(initialView);
  const selectedView = controlledView ?? view;
  const [sidebarOpen, setSidebarOpen] = useState(false);
  // 960px 窗口展开 16rem 侧栏后仍需给 Session 约 780px 双栏空间；不足时使用图标侧栏。
  const narrow = shouldUseIconSidebar(width);

  useEffect(() => {
    setSidebarOpen(!narrow);
  }, [narrow]);

  const selectView = (next: WorkspaceView) => {
    if (controlledView === undefined) setView(next);
    onViewChange?.(next);
  };

  return <TooltipProvider>
    <div ref={ref} data-testid="workspace-shell" className="flex h-full min-h-0 w-full min-w-0 bg-background text-foreground">
      <SidebarProvider className="flex h-full min-h-0 w-full" open={sidebarOpen} onOpenChange={setSidebarOpen}>
        <Sidebar collapsible="icon" variant="sidebar">
          <SidebarHeader className="flex-row items-center gap-2">
            <span
              aria-hidden="true"
              data-testid="seasnail-brand-mark"
              className="size-6 shrink-0 bg-foreground/50"
              style={{
                WebkitMaskImage: `url("${seaSnailMark}")`,
                maskImage: `url("${seaSnailMark}")`,
                WebkitMaskPosition: "center",
                maskPosition: "center",
                WebkitMaskRepeat: "no-repeat",
                maskRepeat: "no-repeat",
                WebkitMaskSize: "contain",
                maskSize: "contain",
              }}
            />
            <span className="truncate font-heading font-semibold group-data-[collapsible=icon]:hidden">SeaSnail</span>
          </SidebarHeader>
          <SidebarContent>
            <SidebarMenu>
              <SidebarMenuItem><SidebarMenuButton isActive={selectedView === "sessions"} tooltip={t("nav.sessions")} onClick={() => selectView("sessions")}><MessagesSquareIcon aria-hidden="true" />{t("nav.sessions")}</SidebarMenuButton></SidebarMenuItem>
              <SidebarMenuItem><SidebarMenuButton isActive={selectedView === "dictionary"} tooltip={t("nav.dictionary")} onClick={() => selectView("dictionary")}><BookTypeIcon aria-hidden="true" />{t("nav.dictionary")}</SidebarMenuButton></SidebarMenuItem>
            </SidebarMenu>
          </SidebarContent>
          <SidebarFooter className="mt-auto">
            <SidebarMenu>
              <SidebarMenuItem><SidebarMenuButton isActive={selectedView === "settings"} tooltip={t("nav.settings")} onClick={() => selectView("settings")}><SettingsIcon aria-hidden="true" />{t("nav.settings")}</SidebarMenuButton></SidebarMenuItem>
              <SidebarMenuItem><SidebarMenuButton isActive={selectedView === "help"} tooltip={t("nav.help")} onClick={() => selectView("help")}><BookOpenIcon aria-hidden="true" />{t("nav.help")}</SidebarMenuButton></SidebarMenuItem>
            </SidebarMenu>
          </SidebarFooter>
        </Sidebar>
        <SidebarInset className="min-h-0 min-w-0">
          <header className="flex h-12 shrink-0 items-center gap-2 border-b px-3"><SidebarTrigger aria-label={t("nav.label")} /><h1 className="truncate text-sm font-medium">{t(`nav.${selectedView}` as "nav.sessions" | "nav.dictionary" | "nav.settings" | "nav.help")}</h1></header>
          <div className={selectedView === "settings" ? "min-h-0 min-w-0 flex-1 overflow-hidden p-4" : "min-h-0 min-w-0 flex-1 overflow-auto p-4"}>{views[selectedView]}</div>
          <RuntimeStatusBar onOpenSettings={onOpenSettings} />
          <WorkspaceTaskStatusAnnouncer />
        </SidebarInset>
      </SidebarProvider>
    </div>
  </TooltipProvider>;
}
