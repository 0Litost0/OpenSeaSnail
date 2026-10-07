import { useMutation } from "@tanstack/react-query";
import { type FormEvent, useMemo, useRef, useState } from "react";
import { exportToFile } from "@/api/transport";
import { localizedError } from "@/api/errors";
import { useSessions } from "@/features/sessions/queries";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { AlertDialog, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle, AlertDialogTrigger } from "@/components/ui/alert-dialog";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { useI18n } from "@/i18n/I18nProvider";

export function ExportPanel() {
  const { locale, t } = useI18n();
  const sessions = useSessions();
  const [password, setPassword] = useState("");
  const [confirmOpen, setConfirmOpen] = useState(false);
  const passwordRef = useRef<HTMLInputElement>(null);
  const [selected, setSelected] = useState<string[]>([]);
  const [notice, setNotice] = useState<string>();
  const items = useMemo(() => sessions.data?.pages.flatMap((page) => page.items) ?? [], [sessions.data]);
  const exportZip = useMutation({
    mutationFn: (request: { password: string; session_ids: string[] }) => exportToFile(request),
    onSuccess: (saved) => setNotice(saved ? t("export.saved") : t("export.cancelled")),
  });
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!password || exportZip.isPending) return;
    const request = { password, session_ids: [...selected] };
    // 关闭 WebView 模态后再等待原生保存窗口；密码不留在组件状态中。
    setConfirmOpen(false);
    setPassword("");
    exportZip.mutate(request);
  };
  const toggle = (id: string, checked: boolean) => setSelected((current) => checked ? [...current, id] : current.filter((item) => item !== id));
  return <Card>
    <CardHeader><CardTitle>{t("export.title")}</CardTitle><CardDescription>{t("export.description")}</CardDescription></CardHeader>
    <CardContent className="flex flex-col gap-5">
      {notice && <Alert><AlertTitle>{t("export.result")}</AlertTitle><AlertDescription>{notice}</AlertDescription></Alert>}
      {exportZip.error && <Alert variant="destructive"><AlertTitle>{t("export.result")}</AlertTitle><AlertDescription>{localizedError(t, exportZip.error)}</AlertDescription></Alert>}
      <FieldGroup>
        <Field>
          <FieldLabel>{t("export.sessions")}</FieldLabel><FieldDescription>{t("export.sessions.hint")}</FieldDescription>
          <div className="flex flex-col gap-2">{items.map((session) => <label key={session.id} className="flex items-center gap-2 text-sm"><Checkbox disabled={exportZip.isPending} checked={selected.includes(session.id)} onCheckedChange={(checked) => toggle(session.id, checked === true)} />{new Intl.DateTimeFormat(locale, { dateStyle: "short", timeStyle: "short" }).format(new Date(session.created_at))} · {session.preview}</label>)}</div>
          {sessions.hasNextPage && <Button type="button" variant="outline" disabled={sessions.isFetchingNextPage} onClick={() => sessions.fetchNextPage()}>{t("export.load_more")}</Button>}
        </Field>
        <AlertDialog open={confirmOpen} onOpenChange={(open) => { setConfirmOpen(open); setPassword(""); if (open) { setNotice(undefined); exportZip.reset(); } }}>
          <AlertDialogTrigger asChild><Button disabled={exportZip.isPending}>{exportZip.isPending ? t("export.preparing") : t("export.submit")}</Button></AlertDialogTrigger>
          <AlertDialogContent onOpenAutoFocus={(event) => { event.preventDefault(); passwordRef.current?.focus(); }}>
            <AlertDialogHeader><AlertDialogTitle>{t("export.confirm_title")}</AlertDialogTitle><AlertDialogDescription>{t("export.confirm_description")}</AlertDialogDescription></AlertDialogHeader>
            <form onSubmit={submit} className="flex flex-col gap-4">
              <FieldGroup><Field><FieldLabel htmlFor="export-password">{t("export.password")}</FieldLabel><Input ref={passwordRef} id="export-password" type="password" autoComplete="current-password" value={password} onChange={(event) => setPassword(event.target.value)} required /></Field></FieldGroup>
              <AlertDialogFooter><AlertDialogCancel type="button">{t("common.cancel")}</AlertDialogCancel><Button type="submit" disabled={!password || exportZip.isPending}>{t("export.confirm")}</Button></AlertDialogFooter>
            </form>
          </AlertDialogContent>
        </AlertDialog>
      </FieldGroup>
    </CardContent>
  </Card>;
}
