import { describe, expect, it, vi } from "vitest";

const { request } = vi.hoisted(() => ({ request: vi.fn() }));
vi.mock("./transport", () => ({ request }));

import { getSession, listSessions, searchSessions } from "./sessions";

describe("sessions API", () => {
  it("provides an MSW OpenAPI-shaped sessions mock", async () => {
    const response = await fetch("http://127.0.0.1:43123/api/v1/sessions");
    await expect(response.json()).resolves.toEqual({ items: [], next_cursor: null });
  });

  it("serializes cursor and source for the restricted IPC transport", async () => {
    request.mockResolvedValueOnce({ status: 200, body: { items: [], next_cursor: "next" } });
    await expect(listSessions({ cursor: "cursor", limit: 20, source: "realtime" })).resolves.toEqual({ items: [], next_cursor: "next" });
    expect(request).toHaveBeenCalledWith(expect.objectContaining({ path: "/sessions", query: { cursor: "cursor", limit: "20", source: "realtime" } }));
  });

  it("uses the OpenAPI search operation and rejects non-success responses", async () => {
    request.mockResolvedValueOnce({ status: 200, body: { items: [], next_cursor: null } });
    await searchSessions({ q: "海", cursor: "next", limit: 20, source: "imported" });
    expect(request).toHaveBeenLastCalledWith(expect.objectContaining({ query: { q: "海", cursor: "next", limit: "20", source: "imported" } }));
    // daemon 返回稳定 error.code 时优先透传码，UI 按 error.<code> 本地化。
    request.mockResolvedValueOnce({ status: 423, body: { error: { code: "locked", message: "account not unlocked" } } });
    await expect(searchSessions({ q: "海" })).rejects.toThrow("locked");
    // 无可解析码时按 HTTP 状态回退稳定码（423 → locked），不暴露 HTTP 文案。
    request.mockResolvedValueOnce({ status: 423, body: {} });
    await expect(searchSessions({ q: "海" })).rejects.toThrow("locked");
  });

  it("encodes dynamic session ids before requesting details", async () => {
    request.mockResolvedValueOnce({ status: 200, body: { id: "a/b", status: "transcribing", transcript: null } });
    await getSession("a/b");
    expect(request).toHaveBeenCalledWith(expect.objectContaining({ path: "/sessions/a%2Fb" }));
  });
});
