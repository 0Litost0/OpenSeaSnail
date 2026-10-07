import { describe, expect, it } from "vitest";
import { resolveSystemLocale, translate } from "./I18nProvider";
import { enUS, zhCN } from "./dictionaries";

describe("i18n", () => {
  it("maps Chinese language variants and defaults other locales to English", () => {
    expect(resolveSystemLocale("zh-Hans-CN")).toBe("zh-CN");
    expect(resolveSystemLocale("zh-TW")).toBe("zh-CN");
    expect(resolveSystemLocale("fr-FR")).toBe("en-US");
  });

  it("reads both dictionaries and replaces variables", () => {
    expect(translate("zh-CN", "nav.sessions")).toBe("会话");
    expect(translate("en-US", "nav.sessions")).toBe("Sessions");
    expect(translate("en-US", "sessions.speaker", { speaker: "A" })).toBe("Speaker A");
    expect(translate("zh-CN", "sessions.speaker", { speaker: "甲" })).toBe("说话人 甲");
  });

  it("falls back to a stable English message for an unavailable runtime key", () => {
    expect(translate("en-US", "missing.key" as never)).toBe(enUS["error.generic"]);
  });

  it("keeps a non-empty zh-CN value for every en-US key", () => {
    for (const key of Object.keys(enUS) as (keyof typeof enUS)[]) {
      expect(typeof zhCN[key]).toBe("string");
      expect(zhCN[key].trim().length).toBeGreaterThan(0);
    }
  });
});
