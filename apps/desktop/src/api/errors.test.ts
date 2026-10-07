import { describe, expect, it, vi } from "vitest";

const { request } = vi.hoisted(() => ({ request: vi.fn() }));
vi.mock("./transport", () => ({ request }));

import { ApiError, errorCode, localizedError, unwrap } from "./errors";
import { listSessions } from "./sessions";

const t = ((key: string) => key) as never;

describe("errorCode", () => {
  it("normalizes ApiError, strings, errors and unknowns to a code string", () => {
    expect(errorCode(new ApiError("locked"))).toBe("locked");
    expect(errorCode("resource_unavailable")).toBe("resource_unavailable");
    expect(errorCode(new Error("gone"))).toBe("gone");
    expect(errorCode(undefined)).toBe("generic");
  });
});

describe("unwrap", () => {
  it("extracts the daemon error.code on failure", async () => {
    request.mockResolvedValueOnce({ status: 423, body: { error: { code: "locked", message: "x" } } });
    await expect(listSessions()).rejects.toMatchObject({ code: "locked" });
  });

  it("falls back to an HTTP-status code when no daemon code is present", async () => {
    request.mockResolvedValueOnce({ status: 409, body: {} });
    await expect(listSessions()).rejects.toMatchObject({ code: "conflict" });
  });

  it("falls back to generic for unmapped HTTP statuses", async () => {
    request.mockResolvedValueOnce({ status: 418, body: {} });
    await expect(listSessions()).rejects.toMatchObject({ code: "generic" });
  });

  it("wraps transport rejections (invoke failures) as an ApiError", async () => {
    request.mockRejectedValueOnce("connection");
    await expect(listSessions()).rejects.toMatchObject({ code: "connection" });
  });
});

describe("localizedError", () => {
  it("maps known daemon, native and recording codes to error.<code>", () => {
    expect(localizedError(t, "wrong_password")).toBe("error.wrong_password");
    expect(localizedError(t, "resource_changed")).toBe("error.resource_changed");
    expect(localizedError(t, "recording_no_audio")).toBe("error.recording_no_audio");
    expect(localizedError(t, "recording_microphone_unavailable")).toBe("error.recording_microphone_unavailable");
    expect(localizedError(t, "recording_failed")).toBe("error.recording_failed");
    expect(localizedError(t, "clipboard_image_write_failed")).toBe("error.clipboard_image_write_failed");
    expect(localizedError(t, "shortcut_invalid")).toBe("error.shortcut_invalid");
  });

  it("aliases connection codes to error.connection", () => {
    expect(localizedError(t, "connection")).toBe("error.connection");
    expect(localizedError(t, "daemon_request_failed")).toBe("error.connection");
  });

  it("falls back to generic for any unknown string and never leaks raw text", () => {
    expect(localizedError(t, "实时转译提交失败: HTTP 500")).toBe("error.generic");
    expect(localizedError(t, new Error("some daemon message with 麦克风权限"))).toBe("error.generic");
  });
});
