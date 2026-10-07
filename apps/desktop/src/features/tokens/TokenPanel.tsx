import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { KeyRoundIcon, TriangleAlertIcon } from "lucide-react";
import { type FormEvent, useState } from "react";
import { createToken, listTokens, revokeToken } from "@/api/tokens";
import { localizedError } from "@/api/errors";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle, AlertDialogTrigger } from "@/components/ui/alert-dialog";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Field, FieldGroup, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { useI18n } from "@/i18n/I18nProvider";

const scopes = ["sessions:read", "sessions:write", "tokens:manage"];
export function TokenPanel() {
  const { t } = useI18n();
  const client = useQueryClient(); const [name, setName] = useState(""); const [selectedScopes, setSelectedScopes] = useState<string[]>(["sessions:read"]); const [secret, setSecret] = useState<string>(); const [revokeId, setRevokeId] = useState<string>();
  const tokens = useQuery({ queryKey: ["tokens"], queryFn: listTokens });
  const create = useMutation({ mutationFn: createToken, onSuccess: (created) => { setSecret(created.secret); void client.invalidateQueries({ queryKey: ["tokens"] }); } });
  const revoke = useMutation({ mutationFn: revokeToken, onSuccess: () => { setRevokeId(undefined); void client.invalidateQueries({ queryKey: ["tokens"] }); } });
  const error = tokens.error ?? create.error ?? revoke.error;
  const submit = (event: FormEvent) => { event.preventDefault(); create.mutate({ name, scopes: selectedScopes }); };
  return <Card><CardHeader><CardTitle>{t("token.title")}</CardTitle><CardDescription>{t("token.description")}</CardDescription></CardHeader><CardContent className="flex flex-col gap-5">{error && <Alert variant="destructive"><TriangleAlertIcon /><AlertTitle>{t("account.operation_failed")}</AlertTitle><AlertDescription>{localizedError(t, error)}</AlertDescription></Alert>}{secret && <Alert><KeyRoundIcon /><AlertTitle>{t("token.save_now")}</AlertTitle><AlertDescription><p className="break-all">{secret}</p><p>{t("token.once")}</p><Button variant="outline" size="sm" onClick={() => setSecret(undefined)}>{t("token.saved")}</Button></AlertDescription></Alert>}<form onSubmit={submit}><FieldGroup><Field><FieldLabel htmlFor="token-name">{t("token.name")}</FieldLabel><Input id="token-name" value={name} onChange={(event) => setName(event.target.value)} required /></Field><Field><FieldLabel>{t("token.permissions")}</FieldLabel><ToggleGroup type="multiple" value={selectedScopes} onValueChange={setSelectedScopes} variant="outline"><ToggleGroupItem value={scopes[0]}>{t("token.read")}</ToggleGroupItem><ToggleGroupItem value={scopes[1]}>{t("token.write")}</ToggleGroupItem><ToggleGroupItem value={scopes[2]}>{t("token.manage")}</ToggleGroupItem></ToggleGroup></Field><Button disabled={create.isPending || selectedScopes.length === 0}>{create.isPending ? t("token.creating") : t("token.create")}</Button></FieldGroup></form><Table><TableHeader><TableRow><TableHead>{t("token.name")}</TableHead><TableHead>{t("token.prefix")}</TableHead><TableHead>{t("token.permissions")}</TableHead><TableHead>{t("token.actions")}</TableHead></TableRow></TableHeader><TableBody>{tokens.data?.map((token) => <TableRow key={token.id}><TableCell>{token.name}</TableCell><TableCell>{token.prefix}</TableCell><TableCell>{token.scopes.join(", ")}</TableCell><TableCell><AlertDialog><AlertDialogTrigger asChild><Button variant="destructive" size="sm" onClick={() => setRevokeId(token.id)}>{t("token.revoke")}</Button></AlertDialogTrigger><AlertDialogContent><AlertDialogHeader><AlertDialogTitle>{t("token.revoke_title")}</AlertDialogTitle><AlertDialogDescription>{t("token.revoke_description")}</AlertDialogDescription></AlertDialogHeader><AlertDialogFooter><AlertDialogCancel>{t("common.cancel")}</AlertDialogCancel><AlertDialogAction variant="destructive" disabled={revoke.isPending} onClick={() => revokeId && revoke.mutate(revokeId)}>{t("token.confirm_revoke")}</AlertDialogAction></AlertDialogFooter></AlertDialogContent></AlertDialog></TableCell></TableRow>)}</TableBody></Table></CardContent></Card>;
}
