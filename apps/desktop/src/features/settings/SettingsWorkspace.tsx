import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { AccountPanel } from "@/features/accounts/AccountPanel";
import { ExportPanel } from "@/features/export/ExportPanel";
import { TokenPanel } from "@/features/tokens/TokenPanel";
import { ClipboardContextPanel } from "./ClipboardContextPanel";
import { CleanupProviderPanel } from "./CleanupProviderPanel";
import { ModelManagementPanel } from "./ModelManagementPanel";
import { PermissionsPanel } from "./PermissionsPanel";
import { PromptStudioPanel } from "./PromptStudioPanel";
import { RecordingBehaviorPanel } from "./RecordingBehaviorPanel";
import { DictionaryLearningPanel } from "./DictionaryLearningPanel";
import { ShortcutPanel } from "./ShortcutPanel";
import { useI18n, type AppLocale } from "@/i18n/I18nProvider";
import { resolveWideLayout, useElementWidth } from "@/hooks/use-element-width";
import { cn } from "@/lib/utils";

export type SettingsGroup = "general" | "privacy" | "engine" | "account";

function LanguagePanel() {
  const { locale, setLocale, t } = useI18n();
  const [error, setError] = useState(false);
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("settings.language")}</CardTitle>
        <CardDescription>{t("settings.language.description")}</CardDescription>
      </CardHeader>
      <CardContent>
        <Select value={locale} onValueChange={(value) => {
          setError(false);
          void setLocale(value as AppLocale).catch(() => setError(true));
        }}>
          <SelectTrigger className="w-full max-w-64" aria-label={t("settings.language")}>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectGroup>
              <SelectItem value="zh-CN">{t("settings.language.zh")}</SelectItem>
              <SelectItem value="en-US">{t("settings.language.en")}</SelectItem>
            </SelectGroup>
          </SelectContent>
        </Select>
        {error && <p role="alert" className="mt-2 text-sm text-destructive">{t("error.locale_save_failed")}</p>}
      </CardContent>
    </Card>
  );
}

export function SettingsWorkspace({ epoch, initialGroup = "general" }: { epoch: number; initialGroup?: SettingsGroup }) {
  const { t } = useI18n();
  const { ref, width } = useElementWidth<HTMLElement>();
  const wide = resolveWideLayout(width, false, 700);
  const [group, setGroup] = useState<SettingsGroup>(initialGroup);
  const contentRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const viewport = contentRef.current?.querySelector<HTMLElement>('[data-slot="scroll-area-viewport"]');
    if (viewport) viewport.scrollTop = 0;
  }, [group]);
  useEffect(() => setGroup(initialGroup), [initialGroup]);
  const groups: Array<[SettingsGroup, string]> = [
    ["general", t("settings.general")],
    ["privacy", t("settings.privacy")],
    ["engine", t("settings.engine")],
    ["account", t("settings.account")],
  ];
  return (
    <section ref={ref} className="flex h-full min-h-0 flex-col gap-4 overflow-hidden" aria-labelledby="settings-title">
      <h2 id="settings-title" className="shrink-0 font-heading text-xl font-semibold">{t("settings.title")}</h2>
      <div hidden={wide} className="shrink-0">
        <Select value={group} onValueChange={(value) => setGroup(value as SettingsGroup)}>
        <SelectTrigger className="w-full" aria-label={t("settings.title")}><SelectValue /></SelectTrigger>
        <SelectContent><SelectGroup>{groups.map(([value, label]) => <SelectItem key={value} value={value}>{label}</SelectItem>)}</SelectGroup></SelectContent>
        </Select>
      </div>
      <div className={cn("grid min-h-0 flex-1 gap-4 overflow-hidden", wide && "grid-cols-[13rem_minmax(0,1fr)]")}>
        <nav hidden={!wide} className="flex flex-col gap-1" aria-label={t("settings.title")}>
          {groups.map(([value, label]) => <Button key={value} variant={group === value ? "secondary" : "ghost"} className="justify-start" onClick={() => setGroup(value)}>{label}</Button>)}
        </nav>
        <ScrollArea className="h-full min-h-0 min-w-0 pr-3" ref={contentRef} aria-label={groups.find(([value]) => value === group)?.[1]}>
          <div hidden={group !== "general"} className="flex flex-col gap-4"><LanguagePanel /><ShortcutPanel /><RecordingBehaviorPanel /></div>
          <div hidden={group !== "privacy"} className="flex flex-col gap-4"><PermissionsPanel /><ClipboardContextPanel /><DictionaryLearningPanel /></div>
          <div hidden={group !== "engine"} className="flex flex-col gap-4"><ModelManagementPanel /><CleanupProviderPanel /><PromptStudioPanel /></div>
          <div hidden={group !== "account"} className="flex flex-col gap-4"><AccountPanel /><TokenPanel key={`token-${epoch}`} /><ExportPanel key={`export-${epoch}`} /></div>
        </ScrollArea>
      </div>
    </section>
  );
}
