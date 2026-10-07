import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { I18nProvider } from "@/i18n/I18nProvider";
import type { ContextItem, WorkspaceDetail } from "@/api/transport";

const api = vi.hoisted(() => ({ detail: vi.fn(), thumbnail: vi.fn() }));
vi.mock("@/api/transport", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/api/transport")>()),
  getSessionWorkspaceDetail: api.detail,
  getContextThumbnail: api.thumbnail,
}));
import { SessionPreview } from "./SessionPreview";

const text: ContextItem = { kind: "context_text", sequence: 1, captured_at_ms: 0, placement: "exact", text: "Clipboard instruction" };
const image: ContextItem = { kind: "context_image", sequence: 2, captured_at_ms: 100, placement: "exact", resources: [{ index: 0, path: "/private/image.png", display_name: "image.png", mime_type: "image/png", available: true }] };
function detail(contexts: ContextItem[], separate = false): WorkspaceDetail {
  return { full_text: "Transcript", final_text: "Transcript", text_source: "raw", cleanup_status: "disabled", cleanup_error_code: null, context_layout: separate ? "separate" : "inline", context_degraded: false,
    display_items: [{ kind: "transcript", text: "Transcript", speaker: "" }, ...(separate ? [] : contexts)], separate_contexts: separate ? contexts : [] };
}
function show(status: "completed" | "transcribing" = "completed") {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  return render(<QueryClientProvider client={client}><I18nProvider localeOverride="en-US"><SessionPreview sessionId="session-1" status={status} preview="Transcript" /></I18nProvider></QueryClientProvider>);
}
beforeEach(() => {
  api.detail.mockReset(); api.thumbnail.mockReset();
  vi.stubGlobal("IntersectionObserver", undefined);
  api.thumbnail.mockResolvedValue({ mime_type: "image/png", base64: "fixture" });
});

afterEach(() => { vi.unstubAllGlobals(); });

describe("session list context preview", () => {
  it("shows actual clipboard text and the image resource path without requesting thumbnails", async () => {
    api.detail.mockResolvedValue(detail([text, image]));
    show();
    expect(screen.getByText("Transcript")).toBeInTheDocument();
    expect(await screen.findByText("“Clipboard instruction”")).toBeInTheDocument();
    expect(screen.getByText("/private/image.png")).toBeInTheDocument();
    expect(screen.queryByRole("img")).not.toBeInTheDocument();
    expect(api.thumbnail).not.toHaveBeenCalled();
  });

  it("shows rich text safely, links, and file paths from separate contexts", async () => {
    api.detail.mockResolvedValue(detail([
      { kind: "context_rich_text", sequence: 1, captured_at_ms: 0, placement: "separate", plain_text: "Rich content", sanitized_html: "<strong>Rich content</strong>" },
      { kind: "context_link", sequence: 2, captured_at_ms: 0, placement: "separate", url: "https://example.com/context" },
      { kind: "context_file", sequence: 3, captured_at_ms: 0, placement: "separate", resources: [{ index: 0, path: "/private/report.pdf", display_name: "report.pdf", mime_type: "application/pdf", available: true }] },
    ], true));
    const view = show();
    expect(await screen.findByText("“Rich content”")).toBeInTheDocument();
    expect(screen.getByText("“https://example.com/context”")).toBeInTheDocument();
    expect(screen.getByText("/private/report.pdf")).toBeInTheDocument();
    expect(view.container.querySelector("strong")).toBeNull();
  });

  it("preserves complete paths and newline separation for multiple resources", async () => {
    const paths = ["/private/image.png", "/Users/example/图片/截图 2.png"];
    api.detail.mockResolvedValue(detail([{ ...image, resources: paths.map((path, index) => ({ ...image.resources[0], path, index })) }]));
    show();
    expect(await screen.findByTitle(paths.join("\n"), { normalizer: (value) => value })).toHaveProperty("textContent", paths.join("\n"));
    expect(api.thumbnail).not.toHaveBeenCalled();
  });

  it("does not fetch offscreen or unfinished rows", async () => {
    let intersect!: IntersectionObserverCallback;
    vi.stubGlobal("IntersectionObserver", class {
      constructor(callback: IntersectionObserverCallback) { intersect = callback; }
      observe() {} disconnect() {}
    });
    api.detail.mockResolvedValue(detail([text]));
    const view = show();
    expect(api.detail).not.toHaveBeenCalled();
    await act(async () => { intersect([{ isIntersecting: true }] as IntersectionObserverEntry[], {} as IntersectionObserver); });
    expect(await screen.findByText("“Clipboard instruction”")).toBeInTheDocument();
    view.unmount(); api.detail.mockClear();
    vi.stubGlobal("IntersectionObserver", undefined);
    show("transcribing");
    await waitFor(() => expect(screen.getByText("Transcript")).toBeInTheDocument());
    expect(api.detail).not.toHaveBeenCalled();
  });

  it("keeps unavailable resource paths visible without requesting resources", async () => {
    const unavailable: ContextItem = { ...image, resources: image.resources.map((resource) => ({ ...resource, available: false })) };
    api.detail.mockResolvedValue(detail([unavailable, text, { ...text, sequence: 3, text: "Third context" }, { ...text, sequence: 4, text: "Fourth context" }]));
    show();
    expect(await screen.findByText("/private/image.png")).toBeInTheDocument();
    expect(screen.getByText("+1 more context items")).toBeInTheDocument();
    expect(screen.queryByText("“Fourth context”")).not.toBeInTheDocument();
    expect(api.thumbnail).not.toHaveBeenCalled();
  });

  it("keeps body visible and explicitly reports unavailable context", async () => {
    api.detail.mockRejectedValue(new Error("connection"));
    show();
    expect(await screen.findByText("Clipboard context is temporarily unavailable.")).toBeInTheDocument();
    expect(screen.getByText("Transcript")).toBeInTheDocument();
  });
});
