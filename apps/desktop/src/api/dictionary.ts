import type { paths } from "./openapi";
import { unwrap } from "./errors";
import { request } from "./transport";

type ListOperation = paths["/dictionary"]["get"];
export type DictionaryListOptions = NonNullable<ListOperation["parameters"]["query"]>;
export type DictionaryPage = ListOperation["responses"][200]["content"]["application/json"];
export type DictionaryEntry = DictionaryPage["items"][number];
export type DictionaryMutationResult = paths["/dictionary/entries"]["post"]["responses"][200]["content"]["application/json"];
export type DictionaryImportPreview = paths["/dictionary/imports/preview"]["post"]["responses"][200]["content"]["application/json"];
export type DictionaryImportResult = paths["/dictionary/imports"]["post"]["responses"][200]["content"]["application/json"];

export function listDictionary(options: DictionaryListOptions = {}): Promise<DictionaryPage> {
  return unwrap(request<DictionaryPage, "/dictionary">({
    method: "GET",
    path: "/dictionary",
    query: {
      query: options.query,
      cursor: options.cursor,
      limit: options.limit?.toString(),
    },
  }));
}

export function addDictionaryTerms(terms: string[]): Promise<DictionaryMutationResult> {
  return unwrap(request<DictionaryMutationResult, "/dictionary/entries">({
    method: "POST",
    path: "/dictionary/entries",
    body: { terms },
  }));
}

export function editDictionaryEntry(id: string, term: string): Promise<DictionaryEntry> {
  return unwrap(request<DictionaryEntry, "/dictionary/entries/{id}">({
    method: "PUT",
    path: `/dictionary/entries/${encodeURIComponent(id)}` as "/dictionary/entries/{id}",
    body: { term },
  }));
}

export async function deleteDictionaryEntry(id: string): Promise<void> {
  await unwrap(request<undefined, "/dictionary/entries/{id}">({
    method: "DELETE",
    path: `/dictionary/entries/${encodeURIComponent(id)}` as "/dictionary/entries/{id}",
  }));
}

export async function clearDictionary(): Promise<void> {
  await unwrap(request<undefined, "/dictionary/entries">({
    method: "DELETE",
    path: "/dictionary/entries",
  }));
}

export function previewDictionaryImport(csv: string): Promise<DictionaryImportPreview> {
  return unwrap(request<DictionaryImportPreview, "/dictionary/imports/preview">({
    method: "POST",
    path: "/dictionary/imports/preview",
    body: { csv },
  }));
}

export function importDictionary(csv: string): Promise<DictionaryImportResult> {
  return unwrap(request<DictionaryImportResult, "/dictionary/imports">({
    method: "POST",
    path: "/dictionary/imports",
    body: { csv },
  }));
}
