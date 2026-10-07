import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { ApiError } from "@/api/errors";
import { AccountEpochContext } from "@/features/accounts/account-context";
import { I18nProvider } from "@/i18n/I18nProvider";

const api = vi.hoisted(() => ({
  listDictionary: vi.fn(),
  addDictionaryTerms: vi.fn(),
  editDictionaryEntry: vi.fn(),
  deleteDictionaryEntry: vi.fn(),
  clearDictionary: vi.fn(),
  previewDictionaryImport: vi.fn(),
  importDictionary: vi.fn(),
  exportDictionaryToFile: vi.fn(),
}));
vi.mock("@/api/dictionary", () => ({
  listDictionary: api.listDictionary,
  addDictionaryTerms: api.addDictionaryTerms,
  editDictionaryEntry: api.editDictionaryEntry,
  deleteDictionaryEntry: api.deleteDictionaryEntry,
  clearDictionary: api.clearDictionary,
  previewDictionaryImport: api.previewDictionaryImport,
  importDictionary: api.importDictionary,
}));
vi.mock("@/api/transport", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/api/transport")>()),
  exportDictionaryToFile: api.exportDictionaryToFile,
}));

import { DictionaryWorkspace } from "./DictionaryWorkspace";

const entry = {
  id: "00000000-0000-4000-8000-000000000001",
  term: "SeaSnail",
  source: "learned" as const,
  created_at: "2026-09-19T00:00:00Z",
  updated_at: "2026-09-19T00:00:00Z",
};

function renderWorkspace(epoch = 0) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  return render(
    <QueryClientProvider client={client}>
      <I18nProvider localeOverride="en-US">
        <AccountEpochContext.Provider value={{ epoch, resetAccountData: vi.fn() }}>
          <DictionaryWorkspace />
        </AccountEpochContext.Provider>
      </I18nProvider>
    </QueryClientProvider>,
  );
}

describe("DictionaryWorkspace", () => {
  it("lists learned terms and adds a manual term", async () => {
    api.listDictionary.mockResolvedValue({ items: [entry], next_cursor: null });
    api.addDictionaryTerms.mockResolvedValue({ added: [{ ...entry, source: "manual", term: "Codex" }], promoted: [], skipped_count: 0 });
    renderWorkspace();

    expect(await screen.findByText("SeaSnail")).toBeInTheDocument();
    expect(screen.getByText("Learned")).toBeInTheDocument();
    fireEvent.change(screen.getByPlaceholderText("e.g. SeaSnail"), { target: { value: "Codex" } });
    fireEvent.click(screen.getByRole("button", { name: "Add" }));
    await waitFor(() => expect(api.addDictionaryTerms.mock.calls[0]?.[0]).toEqual(["Codex"]));
    expect(await screen.findByRole("status")).toHaveTextContent("Added 1, promoted 0, skipped 0.");
  });

  it("shows the stable conflict message while editing", async () => {
    api.listDictionary.mockResolvedValue({ items: [entry], next_cursor: null });
    api.editDictionaryEntry.mockRejectedValue(new ApiError("dictionary_conflict"));
    renderWorkspace();

    await screen.findByText("SeaSnail");
    fireEvent.click(screen.getByRole("button", { name: "Edit term" }));
    fireEvent.change(screen.getByLabelText("Edited term"), { target: { value: "OpenAI" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Another entry already uses that term.");
  });

  it("previews a CSV before importing and states that final counts may change", async () => {
    api.listDictionary.mockResolvedValue({ items: [], next_cursor: null });
    api.previewDictionaryImport.mockResolvedValue({ parsed_count: 3, valid_count: 2, added_count: 1, promoted_count: 1, skipped_count: 1 });
    renderWorkspace();

    const file = new File(["term\nSeaSnail\n"], "terms.csv", { type: "text/csv" });
    Object.defineProperty(file, "text", { value: () => Promise.resolve("term\nSeaSnail\n") });
    fireEvent.change(screen.getByLabelText("Import CSV"), { target: { files: [file] } });
    expect(await screen.findByText("Import preview")).toBeInTheDocument();
    expect(screen.getByText("The dictionary may change after preview. Final counts come from the import response.")).toBeInTheDocument();
    expect(api.previewDictionaryImport).toHaveBeenCalledWith("term\nSeaSnail\n");
  });

  it("previews pasted CSV text through the same import flow", async () => {
    api.listDictionary.mockResolvedValue({ items: [], next_cursor: null });
    api.previewDictionaryImport.mockResolvedValue({ parsed_count: 2, valid_count: 2, added_count: 2, promoted_count: 0, skipped_count: 0 });
    api.importDictionary.mockResolvedValue({ parsed_count: 2, added: [], promoted: [], skipped_count: 0 });
    renderWorkspace();

    fireEvent.click(screen.getByRole("button", { name: "Paste CSV" }));
    fireEvent.change(screen.getByLabelText("CSV text"), { target: { value: "term\nSeaSnail\nSenseVoice\n" } });
    fireEvent.click(screen.getByRole("button", { name: "Preview import" }));
    await waitFor(() => expect(api.previewDictionaryImport).toHaveBeenCalledWith("term\nSeaSnail\nSenseVoice\n"));
    expect(await screen.findByText("Import preview")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Import terms" }));
    await waitFor(() => expect(api.importDictionary.mock.calls[0]?.[0]).toBe("term\nSeaSnail\nSenseVoice\n"));
    expect(await screen.findByRole("status")).toHaveTextContent("Imported: added 0, promoted 0, skipped 0.");
  });

  it("reports a failed paste preview and keeps the pasted text", async () => {
    api.listDictionary.mockResolvedValue({ items: [], next_cursor: null });
    api.previewDictionaryImport.mockRejectedValue(new ApiError("dictionary_csv_invalid"));
    renderWorkspace();

    fireEvent.click(screen.getByRole("button", { name: "Paste CSV" }));
    const input = screen.getByLabelText("CSV text");
    fireEvent.change(input, { target: { value: "not-a-csv" } });
    fireEvent.click(screen.getByRole("button", { name: "Preview import" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("CSV");
    expect(input).toHaveValue("not-a-csv");
  });
});
