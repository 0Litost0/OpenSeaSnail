import { useMutation, useQuery } from "@tanstack/react-query";
import { MicIcon, ShieldCheckIcon, TriangleAlertIcon } from "lucide-react";
import { type ReactNode } from "react";
import { openAccessibilitySettings, permissionStatus, requestMicrophonePermission } from "@/api/transport";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from "@/components/ui/card";
import { Separator } from "@/components/ui/separator";
import { useI18n } from "@/i18n/I18nProvider";

export function PermissionsPanel() {
  const { t } = useI18n();
  const permissions = useQuery({ queryKey: ["permissions-status"], queryFn: permissionStatus });
  const microphone = useMutation({ mutationFn: requestMicrophonePermission, onSuccess: () => void permissions.refetch() });
  const accessibility = useMutation({ mutationFn: openAccessibilitySettings, onSuccess: () => void permissions.refetch() });
  const data = permissions.data;
  return <Card><CardHeader><CardTitle>{t("permission.title")}</CardTitle><CardDescription>{t("permission.description")}</CardDescription></CardHeader><CardContent className="flex flex-col gap-4">
    <PermissionRow icon={<MicIcon />} title={t("permission.microphone")} granted={data?.microphone.granted === true} description={t("permission.microphone.desc")} action={<Button variant="outline" onClick={() => microphone.mutate()} disabled={microphone.isPending}>{microphone.isPending ? t("onboarding.microphone.requesting") : t("permission.request")}</Button>} />
    <Separator />
    <PermissionRow icon={<ShieldCheckIcon />} title={t("permission.accessibility")} granted={data?.accessibility_granted === true} description={t("permission.accessibility.desc")} action={<Button variant="outline" onClick={() => accessibility.mutate()} disabled={accessibility.isPending}>{accessibility.isPending ? t("onboarding.accessibility.opening") : t("permission.open_settings")}</Button>} />
    {data?.microphone.status === "denied" ? <Alert><TriangleAlertIcon /><AlertTitle>{t("permission.denied.title")}</AlertTitle><AlertDescription>{t("permission.denied.description")}</AlertDescription></Alert> : null}
  </CardContent><CardFooter><Button variant="ghost" onClick={() => void permissions.refetch()}>{t("permission.refresh")}</Button></CardFooter></Card>;
}

function PermissionRow({ icon, title, description, granted, action }: { icon: ReactNode; title: string; description: string; granted: boolean; action: ReactNode }) {
  const { t } = useI18n();
  return <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between"><div className="flex items-start gap-2">{icon}<div className="flex flex-col gap-1"><div className="flex items-center gap-2"><p className="font-medium">{title}</p><Badge variant={granted ? "default" : "secondary"}>{granted ? t("onboarding.authorized") : t("onboarding.not_authorized")}</Badge></div><p className="text-sm text-muted-foreground">{description}</p></div></div>{action}</div>;
}
