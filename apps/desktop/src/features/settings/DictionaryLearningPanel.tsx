import { useEffect, useState } from "react";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { useI18n } from "@/i18n/I18nProvider";
import { getDictionaryAutoLearnEnabled, setDictionaryAutoLearnEnabled } from "@/api/transport";

export function DictionaryLearningPanel() {
  const { t } = useI18n();
  const [enabled, setEnabled] = useState(true);
  useEffect(() => { void getDictionaryAutoLearnEnabled().then(setEnabled).catch(() => undefined); }, []);
  return <Card><CardHeader><CardTitle>{t("dictionary.learning.title")}</CardTitle><CardDescription>{t("dictionary.learning.description")}</CardDescription></CardHeader><CardContent><label className="flex items-center gap-3"><Checkbox checked={enabled} onCheckedChange={(value) => { const next = value === true; setEnabled(next); void setDictionaryAutoLearnEnabled(next).catch(() => setEnabled(!next)); }} /><span>{t("dictionary.learning.enabled")}</span></label></CardContent></Card>;
}
