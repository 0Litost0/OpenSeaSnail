import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { deleteAccount } from "@/api/auth";
import { desktopAuthStatus, desktopAccountAction } from "@/api/transport";
import { localizedError } from "@/api/errors";
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle, AlertDialogTrigger } from "@/components/ui/alert-dialog";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { useAccountEpoch } from "./account-context";
import { useI18n } from "@/i18n/I18nProvider";

export function AccountPanel() {
  const { t } = useI18n();
  const client = useQueryClient();
  const { resetAccountData } = useAccountEpoch();
  const status = useQuery({ queryKey: ["desktop-auth-status"], queryFn: desktopAuthStatus });
  const [selectedId, setSelectedId] = useState("");
  const logout = useMutation({ mutationFn: () => desktopAccountAction("logout"), onError: () => { void client.invalidateQueries({ queryKey: ["desktop-auth-status"] }); }, onSuccess: (value) => { client.setQueryData(["desktop-auth-status"], value); resetAccountData(); } });
  const remove = useMutation({ mutationFn: deleteAccount, onSuccess: () => { setSelectedId(""); void client.invalidateQueries({ queryKey: ["desktop-auth-status"] }); } });
  const others = status.data?.accounts.filter((account) => !account.is_active) ?? [];
  const error = logout.error ?? remove.error ?? status.error;
  return <Card><CardHeader><CardTitle>{t("account.current")}</CardTitle><CardDescription>{t("account.logout.description")}</CardDescription></CardHeader><CardContent className="flex flex-col gap-5">
    <p>{status.data?.accounts.find((account) => account.is_active)?.username ?? t("account.checking")}</p>
    {error && <p role="alert" className="text-destructive">{localizedError(t, error)}</p>}
    <Button variant="outline" disabled={logout.isPending || remove.isPending || !status.data?.authenticated} onClick={() => logout.mutate()}>{t("account.logout")}</Button>
    {others.length > 0 && <><p>{t("account.manage")}</p><Select value={selectedId} onValueChange={setSelectedId} disabled={logout.isPending || remove.isPending}><SelectTrigger aria-label={t("account.select")}><SelectValue placeholder={t("account.select.placeholder")} /></SelectTrigger><SelectContent>{others.map((account) => <SelectItem key={account.id} value={account.id}>{account.username}</SelectItem>)}</SelectContent></Select><AlertDialog><AlertDialogTrigger asChild><Button variant="destructive" disabled={!selectedId}>{t("account.delete_selected")}</Button></AlertDialogTrigger><AlertDialogContent><AlertDialogHeader><AlertDialogTitle>{t("account.delete_title")}</AlertDialogTitle><AlertDialogDescription>{t("account.delete_description")}</AlertDialogDescription></AlertDialogHeader><AlertDialogFooter><AlertDialogCancel>{t("common.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" disabled={remove.isPending} onClick={() => selectedId && remove.mutate(selectedId)}>{remove.isPending ? t("account.creating") : t("common.confirm_delete")}</AlertDialogAction></AlertDialogFooter></AlertDialogContent></AlertDialog></>}
  </CardContent></Card>;
}
