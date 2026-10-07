import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { createElement } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ContextItem, QueryError, SessionWorkspace, serializeTimelineForCopy, shouldOpenContextLink } from "./SessionWorkspace";
import { ApiError, localizedError } from "@/api/errors";
import type { CleanupDetail, ContextItem as ContextItemData, ContextResource, TimelineItem, WorkspaceDetail } from "@/api/transport";

vi.mock("@/i18n/I18nProvider", () => ({
  useI18n: () => ({ locale: "en-US", t: (key: string) => key }),
}));
vi.mock("@/features/accounts/account-context", () => ({
  useAccountEpoch: () => ({ epoch: 1 }),
}));

const transport = vi.hoisted(() => ({
  openContextLink: vi.fn().mockResolvedValue(true),
  openContextResource: vi.fn().mockResolvedValue(true),
  getContextThumbnail: vi.fn().mockResolvedValue(undefined),
  copyText: vi.fn().mockResolvedValue(undefined),
}));
vi.mock("@/api/transport", () => ({ ...transport }));
const layout = vi.hoisted(() => ({ wide: true, ref: vi.fn() }));
vi.mock("@/hooks/use-element-width", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/hooks/use-element-width")>()),
  useElementWidth: () => ({ ref: layout.ref, width: layout.wide ? 960 : 720 }),
}));

// 可变查询状态：每个用例按需注入列表/状态/详情，验证渲染而非数据获取。
const query = {
  rows: [] as Array<{ id: string; preview: string; status: "transcribing" | "completed" | "failed"; source: "realtime" | "imported"; created_at: string }>,
  session: undefined as { status: string } | undefined,
  detail: undefined as WorkspaceDetail | undefined,
  cleanupDetail: undefined as CleanupDetail | undefined,
  cleanupEnabled: false,
  cleanupId: undefined as string | undefined,
  listPending: false,
};

vi.mock("./SessionPreview", () => ({ SessionPreview: ({ preview }: { preview: string }) => createElement("span", null, preview) }));

vi.mock("./queries", () => {
  const base = { isPending: false, isError: false, hasNextPage: false, isFetchingNextPage: false, refetch: vi.fn(), fetchNextPage: vi.fn() };
  return {
    contextThumbnailQueryKey: (...args: unknown[]) => ["thumbnail", ...args],
    useSessions: () => ({ ...base, isPending: query.listPending, data: { pages: [{ items: query.rows, next_cursor: null }] } }),
    useSessionSearch: () => ({ ...base, isPending: query.listPending, data: { pages: [{ items: query.rows, next_cursor: null }] } }),
    useSession: () => ({ ...base, data: query.session }),
    useSessionWorkspaceDetail: () => ({ ...base, data: query.detail }),
    useSessionCleanupDetail: (id?: string, enabled = true) => {
      query.cleanupId = id;
      query.cleanupEnabled = enabled;
      return { ...base, data: query.cleanupDetail };
    },
  };
});

// jsdom 无 matchMedia；按用例切换宽窄布局以覆盖双栏 Card 与 Sheet 两条路径。
let wideMode = true;
function setWide(value: boolean) { wideMode = value; layout.wide = value; }
function matchMedia(): MediaQueryList {
  return { matches: wideMode, media: "", onchange: null, addEventListener: () => {}, removeEventListener: () => {}, addListener: () => {}, removeListener: () => {}, dispatchEvent: () => false } as MediaQueryList;
}

function withClient(node: ReturnType<typeof createElement>) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  return createElement(QueryClientProvider, { client }, node);
}
function row(id: string, preview = "preview"): typeof query.rows[number] {
  return { id, preview, status: "completed", source: "realtime", created_at: "2026-08-01T00:00:00Z" };
}
function transcript(text: string, speaker = ""): TimelineItem { return { kind: "transcript", text, speaker }; }
function res(name: string, available = true): ContextResource {
  return { index: 0, path: `/tmp/${name}`, display_name: name, mime_type: name.endsWith(".png") ? "image/png" : "application/pdf", available };
}
function ctxText(sequence: number, text: string): ContextItemData { return { kind: "context_text", sequence, captured_at_ms: 0, placement: "exact", text }; }
function ctxRich(sequence: number, plain_text: string, sanitized_html: string): ContextItemData { return { kind: "context_rich_text", sequence, captured_at_ms: 0, placement: "exact", plain_text, sanitized_html }; }
function ctxLink(sequence: number, url: string): ContextItemData { return { kind: "context_link", sequence, captured_at_ms: 0, placement: "exact", url }; }
function ctxFile(sequence: number, resources: ContextResource[]): ContextItemData { return { kind: "context_file", sequence, captured_at_ms: 0, placement: "exact", resources }; }
function ctxImage(sequence: number, resources: ContextResource[]): ContextItemData { return { kind: "context_image", sequence, captured_at_ms: 0, placement: "exact", resources }; }
function detail(display_items: TimelineItem[], fullText = ""): WorkspaceDetail {
  return { full_text: fullText, final_text: fullText, text_source: "raw", cleanup_status: "not_requested", cleanup_error_code: null, context_layout: "none", display_items, separate_contexts: [], context_degraded: false };
}

beforeEach(() => {
  query.rows = [];
  query.session = undefined;
  query.detail = undefined;
  query.cleanupDetail = undefined;
  query.cleanupEnabled = false;
  query.cleanupId = undefined;
  query.listPending = false;
  setWide(true);
  window.matchMedia = matchMedia as unknown as typeof window.matchMedia;
  transport.openContextLink.mockClear();
  transport.openContextResource.mockClear();
  transport.getContextThumbnail.mockReset().mockResolvedValue(undefined);
  transport.copyText.mockClear();
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("context link activation", () => {
  it("requires Command for pointer clicks but supports keyboard activation", () => {
    expect(shouldOpenContextLink(false, 1)).toBe(false);
    expect(shouldOpenContextLink(true, 1)).toBe(true);
    expect(shouldOpenContextLink(false, 0)).toBe(true);
  });
});

describe("workspace error localization", () => {
  const t = ((key: string) => key) as never;
  it("maps stable codes and falls back to generic without leaking raw text", () => {
    expect(localizedError(t, "resource_changed")).toBe("error.resource_changed");
    expect(localizedError(t, new Error("thumbnail_budget_exceeded"))).toBe("error.thumbnail_budget_exceeded");
    expect(localizedError(t, "recording_microphone_unauthorized")).toBe("error.recording_microphone_unauthorized");
    expect(localizedError(t, "resource_changed: replaced")).toBe("error.generic");
    expect(localizedError(t, "unexpected internal detail")).toBe("error.generic");
  });
});

describe("workspace empty and error states", () => {
  it("renders an empty list and selection prompt without a selected session", () => {
    render(withClient(createElement(SessionWorkspace)));
    expect(screen.getByText("sessions.empty")).toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "sessions.search" })).toBeInTheDocument();
  });

  it("renders the list loading state", () => {
    query.listPending = true;
    const { container } = render(withClient(createElement(SessionWorkspace)));
    expect(container.querySelector('[data-slot="spinner"]')).not.toBeNull();
  });

  it("shows time, source, status, and preview in the session list", () => {
    query.rows = [row("s1", "preview text")];
    render(withClient(createElement(SessionWorkspace)));
    expect(screen.getByText("sessions.time", { exact: true })).toBeInTheDocument();
    expect(screen.getAllByText("sessions.source.realtime", { exact: true })).toHaveLength(2);
    expect(screen.getByText("sessions.status.completed", { exact: true })).toBeInTheDocument();
    expect(screen.getByText("preview text", { exact: true })).toBeInTheDocument();
  });

  it("renders a retry action for query errors", () => {
    const retry = vi.fn();
    render(createElement(QueryError, { retry }));
    fireEvent.click(screen.getByRole("button", { name: "common.retry" }));
    expect(retry).toHaveBeenCalledTimes(1);
  });

  it("expands and collapses long context text", async () => {
    const item = ctxText(1, "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7");
    render(createElement(ContextItem, { sessionId: "s", item }));
    fireEvent.click(screen.getByRole("button", { name: "sessions.context.expand" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "sessions.context.collapse" })).toBeInTheDocument());
  });
});

describe("read-only transcript rendering", () => {
  it("renders display_items in transcript order with context interleaved", async () => {
    query.rows = [row("s1", "前文")];
    query.session = { status: "completed" };
    query.detail = detail([transcript("正文A"), ctxText(3, "产品方案"), transcript("正文B")], "正文A正文B");
    render(withClient(createElement(SessionWorkspace)));
    fireEvent.click(screen.getByRole("button", { name: "前文" }));
    await screen.findByText("正文A");
    const body = document.body.textContent ?? "";
    expect(body).toContain("正文B");
    expect(body).toContain("产品方案");
    expect(body.indexOf("正文A")).toBeLessThan(body.indexOf("产品方案"));
    expect(body.indexOf("产品方案")).toBeLessThan(body.indexOf("正文B"));
  });

  it("copies the rendered timeline without visual markers or UI labels", async () => {
    query.rows = [row("s1")];
    query.session = { status: "completed" };
    query.detail = detail([transcript("hello"), ctxText(1, "context"), ctxLink(2, "https://example.com"), ctxFile(3, [res("plan.pdf")]), transcript("end")], "helloend");
    render(withClient(createElement(SessionWorkspace)));
    fireEvent.click(screen.getByRole("button", { name: "preview" }));
    const copy = await screen.findByRole("button", { name: "sessions.copy" });
    fireEvent.click(copy);
    await waitFor(() => expect(transport.copyText).toHaveBeenCalledWith("hello\ncontext\nhttps://example.com\n/tmp/plan.pdf\nend"));
    expect(transport.copyText.mock.calls[0][0]).not.toContain("sessions.context");
    expect(screen.queryByRole("textbox", { name: /edit|save|编辑|保存/i })).toBeNull();
  });

  it("shows a localized error when copying fails", async () => {
    transport.copyText.mockRejectedValueOnce(new Error("clipboard_write_failed"));
    query.rows = [row("s1")];
    query.session = { status: "completed" };
    query.detail = detail([transcript("hello")], "hello");
    render(withClient(createElement(SessionWorkspace)));
    fireEvent.click(screen.getByRole("button", { name: "preview" }));
    fireEvent.click(await screen.findByRole("button", { name: "sessions.copy" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("error.clipboard_write_failed");
  });

  it("shows degraded notice when context is unavailable", async () => {
    query.rows = [row("s1")];
    query.session = { status: "completed" };
    query.detail = { ...detail([transcript("x")], "x"), context_degraded: true };
    render(withClient(createElement(SessionWorkspace)));
    fireEvent.click(screen.getByRole("button", { name: "preview" }));
    await screen.findByText("x");
    expect(screen.getByText("sessions.context.degraded", { exact: true })).toBeInTheDocument();
  });

  it("renders the narrow Sheet with an accessible title", async () => {
    setWide(false);
    query.rows = [row("s1", "前文")];
    query.session = { status: "completed" };
    query.detail = detail([transcript("正文A")]);
    render(withClient(createElement(SessionWorkspace)));
    fireEvent.click(screen.getByRole("button", { name: "前文" }));
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent("sessions.detail");
    expect(dialog).toHaveTextContent("正文A");
    expect(screen.getByRole("button", { name: "common.close" })).toBeInTheDocument();
  });

  it("keeps separate list and detail scroll areas in wide mode", async () => {
    query.rows = [row("s1")];
    query.session = { status: "completed" };
    query.detail = detail([transcript("body")], "body");
    const { container } = render(withClient(createElement(SessionWorkspace)));
    fireEvent.click(screen.getByRole("button", { name: "preview" }));
    await screen.findByText("body");
    expect(container.querySelectorAll('[data-slot="scroll-area"]')).toHaveLength(2);
  });

  // 重启后 presentation cache 必然为空，cleanup 会话的 context 以 separate 布局返回；
  // 详情页必须渲染 separate_contexts，否则重启后剪贴板上下文从页面消失。
  it("renders separate contexts restored after a restart", async () => {
    query.rows = [row("s1")];
    query.session = { status: "completed" };
    query.detail = {
      ...detail([{ kind: "final_text" as const, text: "cleaned" }], "cleaned"),
      text_source: "cleanup",
      cleanup_status: "succeeded",
      context_layout: "separate",
      separate_contexts: [ctxText(2, "secret clipboard")],
    };
    render(withClient(createElement(SessionWorkspace)));
    fireEvent.click(screen.getByRole("button", { name: "preview" }));
    const contextText = await screen.findByText("secret clipboard");
    expect(screen.getByText("sessions.context.label")).toBeInTheDocument();
    // 正文在上、上下文区块在下。
    const bodyText = screen.getByText("cleaned");
    expect(
      bodyText.compareDocumentPosition(contextText) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  });

  it("includes separate contexts in copy serialization", async () => {
    query.rows = [row("s1")];
    query.session = { status: "completed" };
    query.detail = {
      ...detail([{ kind: "final_text" as const, text: "cleaned" }], "cleaned"),
      text_source: "cleanup",
      cleanup_status: "succeeded",
      context_layout: "separate",
      separate_contexts: [ctxText(2, "secret clipboard")],
    };
    render(withClient(createElement(SessionWorkspace)));
    fireEvent.click(screen.getByRole("button", { name: "preview" }));
    await screen.findByText("secret clipboard");
    fireEvent.click(screen.getByRole("button", { name: "sessions.copy" }));
    await waitFor(() => expect(transport.copyText).toHaveBeenCalledWith("cleaned\nsecret clipboard"));
  });

  it("shows original, cleaned, corrections, and captured provider response on demand", async () => {
    query.rows = [row("s1")];
    query.session = { status: "completed" };
    query.detail = {
      ...detail([transcript("cleaned")], "raw transcript"),
      final_text: "cleaned",
      text_source: "cleanup",
      cleanup_status: "succeeded",
    };
    query.cleanupDetail = {
      original_text: "raw transcript",
      cleaned_text: "cleaned",
      corrections: [{ original_text: "Sea Snail", corrected_text: "SeaSnail", kind: "proper_noun" }],
      cleanup_elapsed_ms: 200,
      diagnostics: {
        local_transcription_elapsed_ms: 320,
        trace_id: "00000000-0000-4000-8000-000000000001",
        request_started_at_ms: 1_786_000_000_000,
        response_started_at_ms: 1_786_000_000_100,
        response_completed_at_ms: 1_786_000_000_200,
        http_status: 200,
        response_content_type: "application/json",
        provider_request_id: "request-1",
        raw_response: "{\"choices\":[]}",
        raw_response_base64: "",
        response_sha256: "ab",
        response_body_bytes: 14,
        capture_status: "complete",
      },
    };
    render(withClient(createElement(SessionWorkspace)));
    fireEvent.click(screen.getByRole("button", { name: "preview" }));
    expect(query.cleanupEnabled).toBe(false);
    fireEvent.click(await screen.findByRole("button", { name: "sessions.processing.show" }));
    expect(query.cleanupEnabled).toBe(true);
    const cleaned = await screen.findByLabelText("sessions.processing.cleaned");
    const original = screen.getByLabelText("sessions.processing.original");
    const correction = await screen.findByText("SeaSnail", { exact: true });
    const timeline = screen.getAllByText("sessions.processing.timeline")[0];
    const rawResponse = screen.getByLabelText("sessions.processing.raw_response");
    expect(cleaned).toHaveValue("cleaned");
    expect(original).toHaveValue("raw transcript");
    expect(rawResponse).toHaveValue('{"choices":[]}');
    expect(cleaned.compareDocumentPosition(original) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(original.compareDocumentPosition(correction) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(correction.compareDocumentPosition(timeline) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(timeline.compareDocumentPosition(rawResponse) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(screen.getByLabelText("sessions.processing.provider_request_id")).toHaveValue("request-1");
    expect(screen.getByLabelText("sessions.processing.status")).toHaveValue("200 · sessions.processing.status_complete");
    expect(screen.getByText("320 ms")).toBeInTheDocument();
    expect(screen.queryByText("100 ms")).not.toBeInTheDocument();
    expect(screen.getAllByText("200 ms")).toHaveLength(2);
    expect(screen.getByRole("listitem", { name: "sessions.processing.phase_transcription: 320 ms" })).toBeInTheDocument();
    expect(screen.getByRole("listitem", { name: "sessions.processing.phase_request: 200 ms" })).toBeInTheDocument();
    expect(screen.getByRole("columnheader", { name: "cleanup.prompt.original" })).toBeInTheDocument();
    expect(screen.getByRole("columnheader", { name: "cleanup.prompt.corrected" })).toBeInTheDocument();
  });

  it("does not infer request timing when diagnostics are unavailable", async () => {
    query.rows = [row("s1")];
    query.session = { status: "completed" };
    query.detail = detail([transcript("cleaned")], "raw transcript");
    query.cleanupDetail = {
      original_text: "raw transcript",
      cleaned_text: "cleaned",
      corrections: [],
      cleanup_elapsed_ms: 200,
      diagnostics: null,
    };
    render(withClient(createElement(SessionWorkspace)));
    fireEvent.click(screen.getByRole("button", { name: "preview" }));
    fireEvent.click(await screen.findByRole("button", { name: "sessions.processing.show" }));
    expect(await screen.findByRole("listitem", { name: "sessions.processing.phase_transcription: sessions.processing.unavailable" })).toBeInTheDocument();
    expect(screen.getByRole("listitem", { name: "sessions.processing.phase_request: sessions.processing.unavailable" })).toBeInTheDocument();
    expect(screen.getByText("200 ms")).toBeInTheDocument();
  });

  it("keeps request timing unavailable until the response body is complete", async () => {
    query.rows = [row("s1")];
    query.session = { status: "completed" };
    query.detail = detail([transcript("cleaned")], "raw transcript");
    query.cleanupDetail = {
      original_text: "raw transcript",
      cleaned_text: "cleaned",
      corrections: [],
      cleanup_elapsed_ms: 200,
      diagnostics: {
        local_transcription_elapsed_ms: 320,
        trace_id: "00000000-0000-4000-8000-000000000001",
        request_started_at_ms: 1_786_000_000_000,
        response_started_at_ms: null,
        response_completed_at_ms: null,
        http_status: null,
        response_content_type: null,
        provider_request_id: null,
        raw_response: null,
        raw_response_base64: "",
        response_sha256: "",
        response_body_bytes: 0,
        capture_status: "not_received",
      },
    };
    render(withClient(createElement(SessionWorkspace)));
    fireEvent.click(screen.getByRole("button", { name: "preview" }));
    fireEvent.click(await screen.findByRole("button", { name: "sessions.processing.show" }));
    expect(await screen.findByRole("listitem", { name: "sessions.processing.phase_transcription: 320 ms" })).toBeInTheDocument();
    expect(screen.getByRole("listitem", { name: "sessions.processing.phase_request: sessions.processing.unavailable" })).toBeInTheDocument();
    expect(screen.getByLabelText("sessions.processing.raw_response")).toHaveValue("sessions.processing.capture_not_received");
  });

  it("requires processing details to be opened again after switching sessions", async () => {
    query.rows = [row("s1", "first"), row("s2", "second")];
    query.session = { status: "completed" };
    query.detail = detail([transcript("body")], "body");
    render(withClient(createElement(SessionWorkspace)));

    fireEvent.click(screen.getByRole("button", { name: "first" }));
    fireEvent.click(await screen.findByRole("button", { name: "sessions.processing.show" }));
    expect(query.cleanupId).toBe("s1");
    expect(query.cleanupEnabled).toBe(true);

    fireEvent.click(screen.getByRole("button", { name: "second" }));
    await waitFor(() => {
      expect(query.cleanupId).toBe("s2");
      expect(query.cleanupEnabled).toBe(false);
    });
  });

  it("uses the container width matrix: 960px stays wide and 720px uses Sheet", () => {
    setWide(true);
    const wide = render(withClient(createElement(SessionWorkspace)));
    expect(wide.container.querySelectorAll('[data-slot="scroll-area"]')).toHaveLength(2);
    wide.unmount();

    setWide(false);
    const narrow = render(withClient(createElement(SessionWorkspace)));
    expect(narrow.container.querySelectorAll('[data-slot="scroll-area"]')).toHaveLength(1);
  });
});

describe("context item variants", () => {
  it("renders a file path without a persistent type label", () => {
    const item = ctxFile(1, [res("plan.pdf")]);
    render(createElement(ContextItem, { sessionId: "s", item }));
    expect(screen.getByText("/tmp/plan.pdf", { exact: true })).toBeInTheDocument();
    expect(screen.queryByText("sessions.context.file", { exact: true })).toBeNull();
    expect(screen.queryByText("application/pdf", { exact: true })).toBeNull();
  });

  it("renders an image without a persistent type label", () => {
    const item = ctxImage(1, [res("shot.png")]);
    render(withClient(createElement(ContextItem, { sessionId: "s", item })));
    expect(screen.queryByText("sessions.context.image", { exact: true })).toBeNull();
  });

  it("loads the detail thumbnail lazily and keeps resource opening", async () => {
    const item = ctxImage(1, [res("shot.png")]);
    transport.getContextThumbnail.mockResolvedValue({ mime_type: "image/png", base64: "cG5n", width: 10, height: 10 });
    let intersect!: IntersectionObserverCallback;
    vi.stubGlobal("IntersectionObserver", class {
      constructor(callback: IntersectionObserverCallback) { intersect = callback; }
      observe() {} disconnect() {}
    });
    render(withClient(createElement(ContextItem, { sessionId: "s", item })));
    expect(transport.getContextThumbnail).not.toHaveBeenCalled();
    act(() => intersect([{ isIntersecting: true } as IntersectionObserverEntry], {} as IntersectionObserver));
    const image = await screen.findByRole("img", { name: "shot.png" });
    expect(image).toHaveAttribute("src", "data:image/png;base64,cG5n");
    expect(screen.queryByText("/tmp/shot.png")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "/tmp/shot.png" }));
    expect(transport.openContextResource).toHaveBeenCalledWith("s", 1, 0);
    vi.unstubAllGlobals();
  });

  it("recovers a busy first thumbnail while displaying both context images", async () => {
    let firstAttempts = 0;
    transport.getContextThumbnail.mockImplementation(async (_id, sequence) => {
      if (sequence === 1 && firstAttempts++ === 0) throw new ApiError("thumbnail_busy");
      return { mime_type: "image/png", base64: "cG5n", width: 10, height: 10 };
    });
    render(withClient(createElement("div", null,
      createElement(ContextItem, { sessionId: "s", item: ctxImage(1, [res("first.png")]) }),
      createElement(ContextItem, { sessionId: "s", item: ctxImage(2, [res("second.png")]) }),
    )));
    expect(await screen.findByRole("img", { name: "second.png" })).toBeInTheDocument();
    expect(await screen.findByRole("img", { name: "first.png" })).toBeInTheDocument();
    expect(firstAttempts).toBe(2);
    expect(screen.queryByText("error.thumbnail_busy")).not.toBeInTheDocument();
  });

  it("stops after two retries when thumbnail generation stays busy", async () => {
    transport.getContextThumbnail.mockRejectedValue("thumbnail_busy");
    render(withClient(createElement(ContextItem, { sessionId: "s", item: ctxImage(1, [res("shot.png")]) })));
    expect(await screen.findByText("error.thumbnail_busy", {}, { timeout: 2000 })).toBeInTheDocument();
    expect(transport.getContextThumbnail).toHaveBeenCalledTimes(3);
  });

  it("does not retry an oversized image and still opens its original", async () => {
    transport.getContextThumbnail.mockRejectedValue(new ApiError("thumbnail_budget_exceeded"));
    render(withClient(createElement(ContextItem, { sessionId: "s", item: ctxImage(1, [res("large.png")]) })));
    expect(await screen.findByText("error.thumbnail_budget_exceeded")).toBeInTheDocument();
    expect(transport.getContextThumbnail).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByRole("button", { name: "/tmp/large.png" }));
    expect(transport.openContextResource).toHaveBeenCalledWith("s", 1, 0);
  });

  it("renders a link that opens only on Command-click, not plain click", () => {
    const item = ctxLink(1, "https://example.com");
    render(createElement(ContextItem, { sessionId: "s", item }));
    const link = screen.getByText("https://example.com", { exact: true });
    expect(screen.queryByText("sessions.context.link", { exact: true })).toBeNull();
    fireEvent.click(link, { metaKey: false, detail: 1 });
    expect(transport.openContextLink).not.toHaveBeenCalled();
    fireEvent.click(link, { metaKey: true, detail: 1 });
    expect(transport.openContextLink).toHaveBeenCalledWith("s", 1);
  });

  it("opens a file resource on plain click", () => {
    const item = ctxFile(1, [res("plan.pdf")]);
    render(createElement(ContextItem, { sessionId: "s", item }));
    fireEvent.click(screen.getByRole("button", { name: "/tmp/plan.pdf" }));
    expect(transport.openContextResource).toHaveBeenCalledWith("s", 1, 0);
  });

  it("renders rich text in a prose container", () => {
    const item = ctxRich(1, "bold", "<strong>bold</strong>");
    const { container } = render(createElement(ContextItem, { sessionId: "s", item }));
    expect(screen.queryByText("sessions.context.rich", { exact: true })).toBeNull();
    expect(container.querySelector(".prose strong")).not.toBeNull();
  });

  it("marks unavailable resources without hiding the card", () => {
    const item = ctxFile(1, [res("gone.pdf", false)]);
    render(createElement(ContextItem, { sessionId: "s", item }));
    expect(screen.getByText("/tmp/gone.pdf", { exact: true })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "/tmp/gone.pdf" })).toBeDisabled();
  });
});

describe("timeline copy serialization", () => {
  it("preserves transcript runs and separates context blocks", () => {
    expect(serializeTimelineForCopy([transcript("a"), ctxRich(1, "b", "<strong>b</strong>"), ctxImage(2, [res("shot.png")]), transcript("c")]))
      .toBe("a\nb\n/tmp/shot.png\nc");
  });
});
