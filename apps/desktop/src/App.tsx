import { useQuery, useQueryClient } from "@tanstack/react-query";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";
import { desktopAuthStatus } from "@/api/transport";
import { LoginScreen } from "@/features/accounts/LoginScreen";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { AccountEpochContext } from "@/features/accounts/account-context";
import { Onboarding } from "@/features/onboarding/Onboarding";
import { DictionaryWorkspace } from "@/features/dictionary/DictionaryWorkspace";
import { DictionaryLearningNotice } from "@/features/dictionary/DictionaryLearningNotice";
import { SessionWorkspace } from "@/features/sessions/SessionWorkspace";
import { SessionQueryRefresh } from "@/features/sessions/SessionQueryRefresh";
import { PermissionReminder } from "@/features/settings/PermissionReminder";
import { SettingsWorkspace, type SettingsGroup } from "@/features/settings/SettingsWorkspace";
import { HelpWorkspace } from "@/features/workspace/HelpWorkspace";
import { WorkspaceShell, type WorkspaceView } from "@/features/workspace/WorkspaceShell";
import { useI18n } from "@/i18n/I18nProvider";

const accountQueries = { predicate: (query: { queryKey: readonly unknown[] }) => !["daemon-status", "auth-status", "desktop-auth-status"].includes(String(query.queryKey[0])) };
function clearAccountQueries(client: ReturnType<typeof useQueryClient>) {
  void client.cancelQueries(accountQueries);
  client.removeQueries(accountQueries);
}

export default function App() {
  const { t } = useI18n();
  const client = useQueryClient();
  const [epoch, setEpoch] = useState(0);
  const [continueOnboarding, setContinueOnboarding] = useState(false);
  const [activeView, setActiveView] = useState<WorkspaceView>("sessions");
  const [settingsGroup, setSettingsGroup] = useState<SettingsGroup>("general");
  const auth = useQuery({ queryKey: ["desktop-auth-status"], queryFn: desktopAuthStatus });

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen("open-settings", () => setActiveView("settings")).then((dispose) => { unlisten = dispose; });
    return () => unlisten?.();
  }, []);

  useEffect(() => {
    // Also handles a logout that committed but reported persistence/transport
    // failure: authoritative status must still remove private cached data.
    if (auth.data?.authenticated === false) clearAccountQueries(client);
  }, [client, auth.data?.authenticated]);

  const resetAccountData = () => {
    clearAccountQueries(client);
    setEpoch((value) => value + 1);
    void client.invalidateQueries({ queryKey: ["auth-status"] });
    void client.invalidateQueries({ queryKey: ["desktop-auth-status"] });
  };

  if (auth.isPending) return <main className="min-h-screen bg-background p-6 text-foreground"><div className="mx-auto w-full max-w-xl"><Skeleton className="h-72 w-full" /></div></main>;
  if (auth.error || auth.data?.credential_ready === false) return <main className="min-h-screen bg-background p-6 text-foreground"><div className="mx-auto w-full max-w-xl"><Card><CardHeader><CardTitle>{t("error.connection")}</CardTitle><CardDescription>{t("error.generic")}</CardDescription></CardHeader><CardContent><Button onClick={() => void auth.refetch()}>{t("common.retry")}</Button></CardContent></Card></div></main>;
  if (!auth.data?.initialized || continueOnboarding) return <Onboarding onAccountCreated={() => setContinueOnboarding(true)} onComplete={() => { setContinueOnboarding(false); resetAccountData(); }} />;

  if (!auth.data.authenticated) return <LoginScreen status={auth.data} onAuthenticated={() => { setActiveView("sessions"); resetAccountData(); }} />;

  return (
    <AccountEpochContext.Provider value={{ epoch, resetAccountData }}>
      <DictionaryLearningNotice />
      <SessionQueryRefresh />
      <WorkspaceShell view={activeView} onViewChange={setActiveView} onOpenSettings={() => { setSettingsGroup("privacy"); setActiveView("settings"); }} views={{
        sessions: <><PermissionReminder onOpenSettings={() => { setSettingsGroup("privacy"); setActiveView("settings"); }} /><SessionWorkspace key={epoch} /></>,
        dictionary: <DictionaryWorkspace key={epoch} />,
        settings: <SettingsWorkspace epoch={epoch} initialGroup={settingsGroup} />,
        help: <HelpWorkspace />,
      }} />
    </AccountEpochContext.Provider>
  );
}
