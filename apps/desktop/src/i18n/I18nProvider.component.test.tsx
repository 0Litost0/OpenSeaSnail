import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { I18nProvider, useI18n } from "./I18nProvider";

const preferences = vi.hoisted(() => ({
  get: vi.fn<() => Promise<"zh-CN" | "en-US" | null>>(),
  set: vi.fn<(locale: "zh-CN" | "en-US") => Promise<"zh-CN" | "en-US">>(),
  changed: undefined as ((locale: "zh-CN" | "en-US") => void) | undefined,
}));

vi.mock("@/api/transport", () => ({
  getLocalePreference: preferences.get,
  onLocaleChanged: vi.fn(async (handler: (locale: "zh-CN" | "en-US") => void) => { preferences.changed = handler; return () => { preferences.changed = undefined; }; }),
  setLocalePreference: preferences.set,
}));

function LanguageProbe() {
  const { locale, setLocale, t } = useI18n();
  return <button onClick={() => void setLocale(locale === "en-US" ? "zh-CN" : "en-US")}>{t("nav.sessions")}</button>;
}

function renderProvider() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={client}><I18nProvider><LanguageProbe /></I18nProvider></QueryClientProvider>);
}

describe("I18nProvider component", () => {
  beforeEach(() => {
    preferences.get.mockResolvedValue("en-US");
    preferences.set.mockImplementation(async (locale) => locale);
    preferences.changed = undefined;
    Object.defineProperty(navigator, "language", { configurable: true, value: "en-US" });
  });

  it("loads the persisted locale and updates language without losing the mounted view", async () => {
    renderProvider();
    const button = await screen.findByRole("button", { name: "Sessions" });
    fireEvent.click(button);
    await waitFor(() => expect(screen.getAllByRole("button", { name: "会话" }).length).toBeGreaterThan(0));
    expect(preferences.set).toHaveBeenCalledWith("zh-CN");
    expect(document.documentElement.lang).toBe("zh-CN");
  });

  it("uses the persisted locale after a new provider mount", async () => {
    preferences.get.mockResolvedValue("zh-CN");
    renderProvider();
    await waitFor(() => expect(screen.getByRole("button", { name: "会话" })).toBeInTheDocument());
  });

  it("updates every mounted provider when the native locale event arrives", async () => {
    renderProvider();
    await screen.findByRole("button", { name: "Sessions" });
    preferences.changed?.("zh-CN");
    await waitFor(() => expect(screen.getAllByRole("button", { name: "会话" }).length).toBeGreaterThan(0));
  });

  it("does not let a stale preference query overwrite a newer native event", async () => {
    let resolvePreference: ((locale: "en-US") => void) | undefined;
    preferences.get.mockImplementation(() => new Promise((resolve) => { resolvePreference = resolve; }));
    const view = renderProvider();

    await waitFor(() => expect(preferences.changed).toBeTypeOf("function"));
    preferences.changed?.("zh-CN");
    await waitFor(() => expect(view.container.querySelector("button")).toHaveTextContent("会话"));
    resolvePreference?.("en-US");

    await waitFor(() => expect(view.container.querySelector("button")).toHaveTextContent("会话"));
  });
});
