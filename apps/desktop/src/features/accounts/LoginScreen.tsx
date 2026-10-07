import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import { desktopAccountAction, type DesktopAuthStatus } from "@/api/transport";
import { localizedError } from "@/api/errors";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Field, FieldGroup, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { useI18n } from "@/i18n/I18nProvider";

export function LoginScreen({ status, onAuthenticated }: { status: DesktopAuthStatus; onAuthenticated: () => void }) {
  const { t } = useI18n();
  const client = useQueryClient();
  const [creating, setCreating] = useState(false);
  const [id, setId] = useState(status.accounts[0]?.id ?? "");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [confirmation, setConfirmation] = useState("");
  const mutation = useMutation({
    mutationFn: () => desktopAccountAction(creating ? "create" : "login", creating ? { username: username.trim(), password } : { id, password }),
    onSuccess: (value) => { setPassword(""); setConfirmation(""); client.setQueryData(["desktop-auth-status"], value); onAuthenticated(); },
    onError: () => { setPassword(""); setConfirmation(""); void client.invalidateQueries({ queryKey: ["desktop-auth-status"] }); },
  });
  const submit = (event: FormEvent) => { event.preventDefault(); if (!password || (creating ? !username.trim() || password !== confirmation : !id) || mutation.isPending) return; mutation.mutate(); };
  return <main className="flex min-h-screen items-center justify-center bg-background p-6 text-foreground"><Card className="w-full max-w-md"><CardHeader><CardTitle>{creating ? t("account.register") : t("account.login.title")}</CardTitle><CardDescription>{t("account.login.description")}</CardDescription></CardHeader><CardContent><form onSubmit={submit}><FieldGroup>
    {creating ? <Field><FieldLabel htmlFor="login-username">{t("account.username")}</FieldLabel><Input id="login-username" value={username} onChange={(e) => setUsername(e.target.value)} maxLength={128} disabled={mutation.isPending} autoComplete="username" required /></Field> : <Field><FieldLabel>{t("account.select")}</FieldLabel><Select value={id} onValueChange={(value) => { setId(value); setPassword(""); mutation.reset(); }} disabled={mutation.isPending}><SelectTrigger aria-label={t("account.select")}><SelectValue placeholder={t("account.select.placeholder")} /></SelectTrigger><SelectContent>{status.accounts.map((account) => <SelectItem key={account.id} value={account.id}>{account.username}</SelectItem>)}</SelectContent></Select></Field>}
    <Field><FieldLabel htmlFor="login-password">{t("account.password")}</FieldLabel><Input id="login-password" type="password" autoComplete={creating ? "new-password" : "current-password"} value={password} onChange={(e) => setPassword(e.target.value)} disabled={mutation.isPending} maxLength={creating ? 1024 : undefined} required /></Field>
    {creating && <Field><FieldLabel htmlFor="login-confirmation">{t("onboarding.password.confirm")}</FieldLabel><Input id="login-confirmation" type="password" autoComplete="new-password" value={confirmation} onChange={(e) => setConfirmation(e.target.value)} disabled={mutation.isPending} required />{confirmation && confirmation !== password && <p role="alert">{t("onboarding.password.mismatch")}</p>}</Field>}
    {mutation.error && <p role="alert" className="text-destructive">{localizedError(t, mutation.error)}</p>}
    <Button disabled={mutation.isPending || !password || (creating ? !username.trim() || password !== confirmation : !id)}>{mutation.isPending ? t("account.checking") : creating ? t("account.create") : t("account.login")}</Button>
    <Button type="button" variant="outline" disabled={mutation.isPending} onClick={() => { setCreating(!creating); setPassword(""); setConfirmation(""); mutation.reset(); }}>{creating ? t("account.back_login") : t("account.register")}</Button>
  </FieldGroup></form></CardContent></Card></main>;
}
