import { createContext, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import { getLocalePreference, onLocaleChanged, setLocalePreference } from "@/api/transport";
import { enUS, zhCN, type TranslationKey } from "./dictionaries";

export type AppLocale = "zh-CN" | "en-US";

export function resolveSystemLocale(locale: string | undefined): AppLocale {
  return locale?.toLowerCase().startsWith("zh") ? "zh-CN" : "en-US";
}

export function translate(locale: AppLocale, key: TranslationKey, variables?: Record<string, string | number>): string {
  const dictionary = locale === "zh-CN" ? zhCN : enUS;
  const localized = dictionary[key];
  if (!localized && import.meta.env.DEV) console.warn(`Missing translation: ${key}`);
  let value: string = localized ?? enUS[key] ?? enUS["error.generic"];
  for (const [name, replacement] of Object.entries(variables ?? {})) {
    value = value.replaceAll(`{${name}}`, String(replacement));
  }
  return value;
}

type I18nContextValue = {
  locale: AppLocale;
  setLocale: (locale: AppLocale) => Promise<void>;
  t: (key: TranslationKey, variables?: Record<string, string | number>) => string;
};

const I18nContext = createContext<I18nContextValue | null>(null);

export function I18nProvider({ children, localeOverride }: { children: ReactNode; localeOverride?: AppLocale }) {
  const [locale, updateLocale] = useState<AppLocale>(() => localeOverride ?? resolveSystemLocale(navigator.language));
  useEffect(() => {
    if (localeOverride) {
      updateLocale(localeOverride);
      return;
    }
    let active = true;
    let unlisten: (() => void) | undefined;
    let eventObserved = false;
    void (async () => {
      try {
        unlisten = await onLocaleChanged((next) => {
          eventObserved = true;
          if (active && (next === "zh-CN" || next === "en-US")) updateLocale(next);
        });
        if (!active) { unlisten(); return; }
        const stored = await getLocalePreference();
        if (active && !eventObserved && (stored === "zh-CN" || stored === "en-US")) updateLocale(stored);
      } catch {
        // 系统 locale 仍是安全回退；下次挂载重新订阅。
      }
    })();
    return () => { active = false; unlisten?.(); };
  }, [localeOverride]);
  useEffect(() => {
    document.documentElement.lang = locale;
  }, [locale]);
  const value = useMemo<I18nContextValue>(() => ({
    locale,
    setLocale: async (next) => {
      if (localeOverride) return;
      await setLocalePreference(next);
      document.documentElement.lang = next;
      updateLocale(next);
    },
    t: (key, variables) => translate(locale, key, variables),
  }), [locale, localeOverride]);
  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

export function useI18n() {
  const value = useContext(I18nContext);
  if (!value) throw new Error("I18nProvider is missing");
  return value;
}
