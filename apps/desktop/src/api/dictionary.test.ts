import { describe, expect, it, vi } from "vitest";

const { request } = vi.hoisted(() => ({ request: vi.fn() }));
vi.mock("./transport", () => ({ request }));

import { addDictionaryTerms, editDictionaryEntry, listDictionary, previewDictionaryImport } from "./dictionary";

describe("dictionary API", () => {
  it("provides an MSW OpenAPI-shaped dictionary mock", async () => {
    const response = await fetch("http://127.0.0.1:43123/api/v1/dictionary");
    await expect(response.json()).resolves.toEqual({ items: [], next_cursor: null });
  });

  it("serializes list filters and dynamic ids through the restricted transport", async () => {
    request.mockResolvedValueOnce({ status: 200, body: { items: [], next_cursor: null } });
    await listDictionary({ query: "Sea", cursor: "next", limit: 50 });
    expect(request).toHaveBeenLastCalledWith(expect.objectContaining({
      method: "GET",
      path: "/dictionary",
      query: { query: "Sea", cursor: "next", limit: "50" },
    }));

    request.mockResolvedValueOnce({ status: 200, body: { id: "a/b", term: "SeaSnail" } });
    await editDictionaryEntry("a/b", "SeaSnail");
    expect(request).toHaveBeenLastCalledWith(expect.objectContaining({ path: "/dictionary/entries/a%2Fb" }));
  });

  it("uses the generated request shapes and stable errors", async () => {
    request.mockResolvedValueOnce({ status: 200, body: { added: [], promoted: [], skipped_count: 1 } });
    await addDictionaryTerms(["SeaSnail"]);
    expect(request).toHaveBeenLastCalledWith(expect.objectContaining({ body: { terms: ["SeaSnail"] } }));

    request.mockResolvedValueOnce({ status: 422, body: { error: { code: "dictionary_csv_invalid", message: "hidden" } } });
    await expect(previewDictionaryImport("bad")).rejects.toThrow("dictionary_csv_invalid");
  });
});
