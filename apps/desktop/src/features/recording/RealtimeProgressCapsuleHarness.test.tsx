import { render, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { RealtimeProgressCapsuleHarness } from "./RealtimeProgressCapsuleHarness";

const tauri = vi.hoisted(() => ({ resize: vi.fn(async () => undefined) }));

vi.mock("@/api/transport", () => ({ resizeRealtimeProgressCapsule: tauri.resize }));
vi.mock("./RealtimeProgressCapsule", () => ({
  RealtimeProgressCapsule: ({ onResize }: { onResize: (width: number, height: number, generation: number, revision: number) => void }) => {
    onResize(320, 40, 4, 9);
    return <section data-testid="capsule" />;
  },
}));

describe("RealtimeProgressCapsuleHarness", () => {
  it("applies measured logical dimensions to the isolated capsule window", async () => {
    render(<RealtimeProgressCapsuleHarness />);
    await waitFor(() => expect(tauri.resize).toHaveBeenCalledOnce());
    expect(tauri.resize).toHaveBeenCalledWith(320, 40, 4, 9);
  });

  it("centers content within a temporary wider native window", () => {
    const { getByRole } = render(<RealtimeProgressCapsuleHarness />);
    expect(getByRole("main")).toHaveClass("w-full", "justify-center");
  });
});
