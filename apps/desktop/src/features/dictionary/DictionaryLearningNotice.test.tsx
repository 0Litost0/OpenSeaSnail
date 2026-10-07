import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { DictionaryLearningNotice, type LearningNotification } from "./DictionaryLearningNotice";

const tauri = vi.hoisted(() => ({
  invoke: vi.fn(),
  listeners: new Map<string, (event: { payload: unknown }) => void>(),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: tauri.invoke }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, callback: (event: { payload: unknown }) => void) => {
    tauri.listeners.set(name, callback);
    return () => tauri.listeners.delete(name);
  }),
}));
vi.mock("@/i18n/I18nProvider", () => ({
  useI18n: () => ({
    t: (key: string, variables?: Record<string, number>) =>
      key === "dictionary.learning.notice_title"
        ? `Learned ${variables?.count} terms`
        : key,
  }),
}));

const notification: LearningNotification = {
  generation: 7,
  added_terms: ["SeaSnail", "OpenWhispr"],
};

describe("DictionaryLearningNotice", () => {
  beforeEach(() => {
    tauri.invoke.mockReset();
    tauri.listeners.clear();
    tauri.invoke.mockImplementation(async (command: string) =>
      command === "learning_notification_status" ? notification : 2,
    );
  });

  it("shows learned terms and invokes generation-bound undo", async () => {
    render(<DictionaryLearningNotice capsule />);
    expect(await screen.findByText("Learned 2 terms")).toBeInTheDocument();
    expect(screen.getByText("SeaSnail · OpenWhispr")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "dictionary.learning.undo" }));
    await waitFor(() => expect(tauri.invoke).toHaveBeenCalledWith(
      "undo_latest_dictionary_learning",
      { generation: 7 },
    ));
  });

  it("removes only the matching notification generation", async () => {
    render(<DictionaryLearningNotice />);
    expect(await screen.findByText("Learned 2 terms")).toBeInTheDocument();
    tauri.listeners.get("dictionary-learning-dismissed")?.({ payload: 6 });
    expect(screen.getByText("Learned 2 terms")).toBeInTheDocument();
    tauri.listeners.get("dictionary-learning-dismissed")?.({ payload: 7 });
    await waitFor(() => expect(screen.queryByText("Learned 2 terms")).not.toBeInTheDocument());
  });

  it("keeps a newer notification when an older undo finishes", async () => {
    let resolveUndo: ((value: number) => void) | undefined;
    tauri.invoke.mockImplementation((command: string) => {
      if (command === "learning_notification_status") return Promise.resolve(notification);
      return new Promise<number>((resolve) => { resolveUndo = resolve; });
    });
    render(<DictionaryLearningNotice capsule />);
    expect(await screen.findByText("Learned 2 terms")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "dictionary.learning.undo" }));
    await waitFor(() => expect(resolveUndo).toBeDefined());
    tauri.listeners.get("dictionary-learning-completed")?.({
      payload: { generation: 8, added_terms: ["SenseVoice"] },
    });
    expect(await screen.findByText("Learned 1 terms")).toBeInTheDocument();
    resolveUndo?.(2);
    await waitFor(() => expect(screen.getByText("SenseVoice")).toBeInTheDocument());
  });

  it("does not let a delayed status response replace a newer event", async () => {
    let resolveStatus: ((value: LearningNotification) => void) | undefined;
    tauri.invoke.mockImplementation((command: string) =>
      command === "learning_notification_status"
        ? new Promise<LearningNotification>((resolve) => { resolveStatus = resolve; })
        : Promise.resolve(0),
    );
    render(<DictionaryLearningNotice />);
    await waitFor(() => expect(resolveStatus).toBeDefined());
    tauri.listeners.get("dictionary-learning-completed")?.({
      payload: { generation: 8, added_terms: ["SenseVoice"] },
    });
    expect(await screen.findByText("SenseVoice")).toBeInTheDocument();
    resolveStatus?.(notification);
    await waitFor(() => expect(screen.getByText("SenseVoice")).toBeInTheDocument());
    expect(screen.queryByText("SeaSnail · OpenWhispr")).not.toBeInTheDocument();
  });
  it("does not resurrect the previous account from delayed status or notification events", async () => {
    let resolveStatus!: (value: LearningNotification) => void;
    tauri.invoke.mockImplementation(() => new Promise<LearningNotification>((resolve) => { resolveStatus = resolve; }));
    render(<DictionaryLearningNotice capsule />);
    await waitFor(() => expect(tauri.listeners.has("dictionary-learning-dismissed")).toBe(true));
    await act(async () => {
      tauri.listeners.get("dictionary-learning-dismissed")?.({ payload: 7 });
      tauri.listeners.get("dictionary-learning-completed")?.({ payload: notification });
      resolveStatus(notification);
    });
    expect(screen.queryByText("SeaSnail · OpenWhispr")).not.toBeInTheDocument();
    act(() => tauri.listeners.get("dictionary-learning-completed")?.({ payload: { generation: 8, added_terms: ["BobTerm"] } }));
    expect(await screen.findByText("BobTerm")).toBeInTheDocument();
  });

});
