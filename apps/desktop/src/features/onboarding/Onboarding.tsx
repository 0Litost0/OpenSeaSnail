import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CheckIcon, KeyRoundIcon, MicIcon, ShieldCheckIcon, TriangleAlertIcon } from "lucide-react";
import { type FormEvent, type ReactNode, useEffect, useState } from "react";
import { type AuthStatus, setupAccount } from "@/api/auth";
import { localizedError } from "@/api/errors";
import { clipboardContextStatus, openAccessibilitySettings, permissionStatus, recordingShortcut, requestMicrophonePermission, setClipboardContextEnabled } from "@/api/transport";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from "@/components/ui/card";
import { Field, FieldDescription, FieldError, FieldGroup, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Checkbox } from "@/components/ui/checkbox";
import { displayShortcut } from "@/features/settings/shortcutKeys";
import { Separator } from "@/components/ui/separator";
import { useI18n } from "@/i18n/I18nProvider";

type OnboardingStep = "welcome" | "account" | "permissions" | "tutorial";

function StepBadge({ active, complete, children }: { active: boolean; complete: boolean; children: ReactNode }) {
  return <Badge variant={active ? "default" : "secondary"}>{complete ? <CheckIcon data-icon="inline-start" /> : null}{children}</Badge>;
}

export function Onboarding({ onAccountCreated, onComplete }: { onAccountCreated: () => void; onComplete: () => void }) {
  const { t, locale, setLocale } = useI18n();
  const [localeError, setLocaleError] = useState(false);
  const client = useQueryClient();
  const [step, setStep] = useState<OnboardingStep>("welcome");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [passwordConfirmation, setPasswordConfirmation] = useState("");
  const [accountCreated, setAccountCreated] = useState(false);
  const passwordsMatch = password === passwordConfirmation;
  const setup = useMutation({
    mutationFn: setupAccount,
    onSuccess: () => {
      setAccountCreated(true);
      setPassword("");
      setPasswordConfirmation("");
      onAccountCreated();
      client.setQueryData<AuthStatus>(["auth-status"], { initialized: true });
      void client.invalidateQueries({ queryKey: ["accounts"] });
    },
  });
  const submitAccount = (event: FormEvent) => {
    event.preventDefault();
    if (!passwordsMatch) return;
    // Creating the account writes Keychain entries and the native proxy then
    // reads them. Show the privacy explanation before either operation starts.
    setup.reset();
    setStep("permissions");
  };

  if (step === "welcome") {
    return <OnboardingFrame step={step}>
      <CardHeader>
        <CardTitle>{t("onboarding.welcome.title")}</CardTitle>
        <CardDescription>{t("onboarding.welcome.description")}</CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        <Alert><ShieldCheckIcon /><AlertTitle>{t("onboarding.local.title")}</AlertTitle><AlertDescription>{t("onboarding.local.description")}</AlertDescription></Alert>
        <Alert><KeyRoundIcon /><AlertTitle>{t("onboarding.account.title")}</AlertTitle><AlertDescription>{t("onboarding.account.description")}</AlertDescription></Alert>
      </CardContent>
      <CardFooter className="flex-wrap justify-between gap-3"><Button onClick={() => setStep("account")}>{t("onboarding.start")}</Button><Button variant="ghost" onClick={() => { setLocaleError(false); void setLocale(locale === "zh-CN" ? "en-US" : "zh-CN").catch(() => setLocaleError(true)); }}>{locale === "zh-CN" ? "English" : "简体中文"}</Button>{localeError ? <p role="alert">{t("error.generic")}</p> : null}</CardFooter>
    </OnboardingFrame>;
  }

  if (step === "account") {
    return <OnboardingFrame step={step}>
      <CardHeader>
        <CardTitle>{t("onboarding.account.create.title")}</CardTitle>
        <CardDescription>{t("onboarding.account.create.description")}</CardDescription>
      </CardHeader>
      <CardContent>
        <form onSubmit={submitAccount}>
          <FieldGroup>
            <Field><FieldLabel htmlFor="onboarding-username">{t("onboarding.username")}</FieldLabel><Input id="onboarding-username" value={username} onChange={(event) => setUsername(event.target.value)} required autoFocus /></Field>
            <Field><FieldLabel htmlFor="onboarding-password">{t("onboarding.password")}</FieldLabel><Input id="onboarding-password" type="password" value={password} onChange={(event) => setPassword(event.target.value)} required /><FieldDescription>{t("onboarding.password.hint")}</FieldDescription></Field>
            <Field data-invalid={!passwordsMatch && passwordConfirmation.length > 0}><FieldLabel htmlFor="onboarding-password-confirmation">{t("onboarding.password.confirm")}</FieldLabel><Input id="onboarding-password-confirmation" type="password" value={passwordConfirmation} onChange={(event) => setPasswordConfirmation(event.target.value)} aria-invalid={!passwordsMatch && passwordConfirmation.length > 0} required />{!passwordsMatch && passwordConfirmation.length > 0 ? <FieldError>{t("onboarding.password.mismatch")}</FieldError> : null}</Field>
            <Button disabled={!passwordsMatch}>{t("onboarding.account.continue")}</Button>
          </FieldGroup>
        </form>
      </CardContent>
      <CardFooter><Button variant="ghost" onClick={() => setStep("welcome")} disabled={setup.isPending}>{t("onboarding.back")}</Button></CardFooter>
    </OnboardingFrame>;
  }

  if (step === "permissions") return <PermissionStep
    accountCreated={accountCreated}
    creatingAccount={setup.isPending}
    accountError={setup.error}
    onCreateAccount={() => setup.mutate({ username, password })}
    onBack={() => setStep("account")}
    onComplete={() => setStep("tutorial")}
  />;
  return <TutorialStep onComplete={onComplete} onBack={() => setStep("permissions")} />;
}

function OnboardingFrame({ step, children }: { step: OnboardingStep; children: ReactNode }) {
  const { t } = useI18n();
  const accountComplete = step === "permissions" || step === "tutorial";
  return <main className="h-full overflow-y-auto bg-background p-6 text-foreground"><div className="mx-auto flex w-full max-w-xl flex-col gap-6">
    <header className="flex flex-col gap-3"><p className="text-sm text-muted-foreground">SeaSnail</p><h1 className="font-heading text-2xl font-semibold tracking-tight">{t("onboarding.title")}</h1><div className="flex flex-wrap gap-2" aria-label={t("onboarding.title")}><StepBadge active={step === "welcome"} complete={step !== "welcome"}>{t("onboarding.step.welcome")}</StepBadge><StepBadge active={step === "account"} complete={accountComplete}>{t("onboarding.step.account")}</StepBadge><StepBadge active={step === "permissions"} complete={step === "tutorial"}>{t("onboarding.step.permissions")}</StepBadge><StepBadge active={step === "tutorial"} complete={false}>{t("onboarding.step.tutorial")}</StepBadge></div></header>
    <Card>{children}</Card>
  </div></main>;
}

function PermissionStep({ accountCreated, creatingAccount, accountError, onCreateAccount, onBack, onComplete }: {
  accountCreated: boolean;
  creatingAccount: boolean;
  accountError: unknown;
  onCreateAccount: () => void;
  onBack: () => void;
  onComplete: () => void;
}) {
  const { t } = useI18n();
  const permissions = useQuery({ queryKey: ["permissions-status"], queryFn: permissionStatus });
  const microphone = useMutation({ mutationFn: requestMicrophonePermission, onSuccess: () => void permissions.refetch() });
  const accessibility = useMutation({ mutationFn: openAccessibilitySettings, onSuccess: () => void permissions.refetch() });
  useEffect(() => {
    const refresh = () => { void permissions.refetch(); };
    window.addEventListener("focus", refresh);
    return () => window.removeEventListener("focus", refresh);
  }, [permissions.refetch]);
  const status = permissions.data;
  const microphoneGranted = status?.microphone.granted === true;
  const accessibilityGranted = status?.accessibility_granted === true;
  return <OnboardingFrame step="permissions">
    <CardHeader><CardTitle>{t("onboarding.permissions.title")}</CardTitle><CardDescription>{t("onboarding.permissions.description")}</CardDescription></CardHeader>
    <CardContent className="flex flex-col gap-4">
      <Alert>
        <KeyRoundIcon />
        <AlertTitle>{t("onboarding.keychain.title")}</AlertTitle>
        <AlertDescription>
          <p>{t("onboarding.keychain.description")}</p>
          <p>{t("onboarding.keychain.path")}</p>
          <Button onClick={onCreateAccount} disabled={creatingAccount || accountCreated}>{creatingAccount ? t("onboarding.account.creating") : accountCreated ? t("onboarding.keychain.ready") : t("onboarding.keychain.create")}</Button>
        </AlertDescription>
      </Alert>
      {accountError ? <Alert variant="destructive"><TriangleAlertIcon /><AlertTitle>{t("onboarding.account.error")}</AlertTitle><AlertDescription>{localizedError(t, accountError)}</AlertDescription></Alert> : null}
      <Separator />
      <PermissionItem title={t("onboarding.microphone")} granted={microphoneGranted} description={t("onboarding.microphone.required")} action={<Button variant="outline" onClick={() => microphoneGranted ? void permissions.refetch() : microphone.mutate()} disabled={microphone.isPending || permissions.isPending}>{microphone.isPending ? t("onboarding.microphone.requesting") : microphoneGranted ? t("onboarding.microphone.recheck") : t("onboarding.microphone.authorize")}</Button>} />
      <p className="text-sm text-muted-foreground">{t("onboarding.microphone.path")}</p>
      <Separator />
      <PermissionItem title={t("onboarding.accessibility")} granted={accessibilityGranted} description={t("onboarding.accessibility.required")} action={<Button variant="outline" onClick={() => accessibility.mutate()} disabled={accessibility.isPending || permissions.isPending}>{accessibility.isPending ? t("onboarding.accessibility.opening") : t("onboarding.accessibility.open")}</Button>} />
      <p className="text-sm text-muted-foreground">{t("onboarding.accessibility.path")}</p>
      {status?.microphone.status === "denied" ? <Alert><TriangleAlertIcon /><AlertTitle>{t("onboarding.microphone.denied.title")}</AlertTitle><AlertDescription>{t("onboarding.microphone.denied.description")}</AlertDescription></Alert> : null}
      {permissions.error || microphone.error || accessibility.error ? <Alert variant="destructive"><AlertTitle>{t("onboarding.permissions.error")}</AlertTitle><AlertDescription>{localizedError(t, microphone.error ?? accessibility.error ?? permissions.error)}</AlertDescription></Alert> : null}
      <Button variant="outline" onClick={() => void permissions.refetch()} disabled={permissions.isFetching}>{t("onboarding.microphone.recheck")}</Button>
    </CardContent>
    <CardFooter className="flex-wrap justify-between gap-3">
      {!accountCreated && <Button variant="ghost" onClick={onBack} disabled={creatingAccount}>{t("onboarding.back")}</Button>}
      <Button variant="ghost" onClick={onComplete} disabled={!accountCreated || creatingAccount}>{t("onboarding.skip")}</Button>
      <Button onClick={onComplete} disabled={!accountCreated || creatingAccount}>{t("onboarding.permissions.continue")}</Button>
    </CardFooter>
  </OnboardingFrame>;
}

function PermissionItem({ title, description, granted, action }: { title: string; description: string; granted: boolean; action: ReactNode }) {
  const { t } = useI18n();
  return <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between"><div className="flex flex-col gap-1"><div className="flex items-center gap-2"><p className="font-medium">{title}</p><Badge variant={granted ? "default" : "secondary"}>{granted ? t("onboarding.authorized") : t("onboarding.not_authorized")}</Badge></div><p className="text-sm text-muted-foreground">{description}</p></div>{action}</div>;
}

function TutorialStep({ onComplete, onBack }: { onComplete: () => void; onBack: () => void }) {
  const { t } = useI18n();
  const client = useQueryClient();
  const shortcut = useQuery({ queryKey: ["recording-shortcut"], queryFn: recordingShortcut });
  const context = useQuery({ queryKey: ["clipboard-context-status"], queryFn: clipboardContextStatus });
  const update = useMutation({
    mutationFn: setClipboardContextEnabled,
    onSuccess: (data) => { client.setQueryData(["clipboard-context-status"], data); },
  });
  return <OnboardingFrame step="tutorial">
    <CardHeader><CardTitle>{t("onboarding.tutorial.title")}</CardTitle><CardDescription>{t("onboarding.tutorial.description")}</CardDescription></CardHeader>
    <CardContent className="flex flex-col gap-5">
      <Alert><MicIcon /><AlertTitle>{shortcut.data ? t("onboarding.shortcut.current", { shortcut: displayShortcut(shortcut.data.recording_shortcut) }) : t("onboarding.shortcut.title")}</AlertTitle><AlertDescription>{t("onboarding.shortcut.description")}</AlertDescription></Alert>
      {shortcut.error || shortcut.data?.registered === false ? <Alert variant="destructive"><AlertTitle>{t("onboarding.shortcut.unavailable")}</AlertTitle><AlertDescription>{t("onboarding.shortcut.fix")}</AlertDescription></Alert> : null}
      <section aria-labelledby="context-demo-title" className="flex flex-col gap-3">
        <h2 id="context-demo-title" className="font-medium">{t("onboarding.context.title")}</h2>
        <p className="text-sm text-muted-foreground">{t("onboarding.context.description")}</p>
        <ol className="grid gap-3 sm:grid-cols-3" aria-label={t("onboarding.context.demo")}>
          {(["speak", "copy", "result"] as const).map((part, index) => <li key={part} className="onboarding-demo-step flex flex-col gap-2 rounded-lg border p-3" style={{ animationDelay: `${index * 180}ms` }}>
            <Badge variant="secondary">{index + 1}</Badge><p className="text-sm font-medium">{t(`onboarding.context.${part}`)}</p><p className="break-words text-sm text-muted-foreground">{t(`onboarding.context.${part}.example`)}</p>
          </li>)}
        </ol>
        <p className="text-sm text-muted-foreground">{t("onboarding.context.privacy")}</p>
      </section>
      <Field orientation="horizontal" data-disabled={context.isPending || update.isPending || !context.data || undefined}>
        <Checkbox id="onboarding-context" checked={context.data?.enabled ?? false} disabled={context.isPending || update.isPending || !context.data} onCheckedChange={(checked) => update.mutate(checked === true)} />
        <div className="flex flex-col gap-1"><FieldLabel htmlFor="onboarding-context">{t("onboarding.context.enable")}</FieldLabel><FieldDescription>{t("onboarding.context.setting")}</FieldDescription></div>
      </Field>
      {context.error || update.error ? <Alert variant="destructive"><AlertTitle>{t("clipboard.update_failed")}</AlertTitle><AlertDescription>{localizedError(t, update.error ?? context.error)}<Button variant="outline" onClick={() => void context.refetch()}>{t("common.retry")}</Button></AlertDescription></Alert> : null}
      <Alert><AlertTitle>{t("onboarding.try.title")}</AlertTitle><AlertDescription>{t("onboarding.try.description")}</AlertDescription></Alert>
    </CardContent>
    <CardFooter className="flex-wrap justify-between gap-3"><Button variant="ghost" onClick={onBack} disabled={update.isPending}>{t("onboarding.back")}</Button><Button onClick={onComplete} disabled={update.isPending}>{t("onboarding.finish")}</Button></CardFooter>
  </OnboardingFrame>;
}
