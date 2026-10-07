import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { useI18n } from "@/i18n/I18nProvider";

export function HelpWorkspace() {
  const { t } = useI18n();
  return (
    <section className="@container flex min-h-0 flex-col gap-4" aria-labelledby="help-title">
      <div>
        <h2 id="help-title" className="font-heading text-xl font-semibold">{t("help.title")}</h2>
        <p className="mt-1 text-sm text-muted-foreground">{t("help.description")}</p>
      </div>
      <div className="grid gap-4 @md:grid-cols-2">
        <Card><CardHeader><CardTitle>{t("help.shortcut.title")}</CardTitle><CardDescription>{t("help.shortcut.description")}</CardDescription></CardHeader><CardContent className="text-sm">{t("help.shortcut.body")}</CardContent></Card>
        <Card><CardHeader><CardTitle>{t("help.permissions.title")}</CardTitle><CardDescription>{t("help.permissions.description")}</CardDescription></CardHeader><CardContent className="text-sm">{t("help.permissions.body")}</CardContent></Card>
        <Card><CardHeader><CardTitle>{t("help.context.title")}</CardTitle><CardDescription>{t("help.context.description")}</CardDescription></CardHeader><CardContent className="text-sm">{t("help.context.body")}</CardContent></Card>
        <Card><CardHeader><CardTitle>{t("help.troubleshooting.title")}</CardTitle><CardDescription>{t("help.troubleshooting.description")}</CardDescription></CardHeader><CardContent className="text-sm">{t("help.troubleshooting.body")}</CardContent></Card>
      </div>
      <p role="status" className="sr-only">{t("help.accessible_status")}</p>
    </section>
  );
}
