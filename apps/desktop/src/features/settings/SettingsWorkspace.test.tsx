import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { SettingsWorkspace } from "./SettingsWorkspace";

const layout = vi.hoisted(() => ({ width: 720 }));
vi.mock("@/hooks/use-element-width", () => ({ useElementWidth: () => ({ ref: vi.fn(), width: layout.width }), resolveWideLayout: (width: number | null, _wasWide: boolean, enter: number) => width !== null && width >= enter }));
vi.mock("@/i18n/I18nProvider", () => ({ useI18n: () => ({ locale: "en-US", setLocale: vi.fn(), t: (key: string) => key }) }));
vi.mock("@/features/accounts/AccountPanel", () => ({ AccountPanel: () => <input aria-label="Account draft" defaultValue="" /> }));
vi.mock("@/features/export/ExportPanel", () => ({ ExportPanel: () => <div>export</div> }));
vi.mock("@/features/tokens/TokenPanel", () => ({ TokenPanel: () => <div>token</div> }));
vi.mock("./ClipboardContextPanel", () => ({ ClipboardContextPanel: () => <div>clipboard</div> }));
vi.mock("./CleanupProviderPanel", () => ({ CleanupProviderPanel: () => <div>cleanup-provider</div> }));
vi.mock("./PromptStudioPanel", () => ({ PromptStudioPanel: () => <div>prompt-studio</div> }));
vi.mock("./ModelManagementPanel", () => ({ ModelManagementPanel: () => <div>model</div> }));
vi.mock("./PermissionsPanel", () => ({ PermissionsPanel: () => <div>permissions</div> }));
vi.mock("./RecordingBehaviorPanel", () => ({ RecordingBehaviorPanel: () => <div>behavior</div> }));
vi.mock("./ShortcutPanel", () => ({ ShortcutPanel: () => <div>shortcut</div> }));

describe("SettingsWorkspace container layout", () => {
  it("resets content scroll on category changes without discarding form drafts", () => {
    layout.width = 960;
    const { container } = render(<SettingsWorkspace epoch={1} initialGroup="account" />);
    fireEvent.change(screen.getByLabelText("Account draft"), { target: { value: "unsaved draft" } });
    const viewport = container.querySelector<HTMLElement>('[data-slot="scroll-area-viewport"]')!;
    viewport.scrollTop = 400;
    fireEvent.click(screen.getByRole("button", { name: "settings.general" }));
    expect(viewport.scrollTop).toBe(0);
    fireEvent.click(screen.getByRole("button", { name: "settings.account" }));
    expect(screen.getByLabelText("Account draft")).toHaveValue("unsaved draft");
  });

  beforeEach(() => { layout.width = 680; });

  it("uses the Select at a 680px container and keeps the desktop navigation hidden", () => {
    const { container } = render(<SettingsWorkspace epoch={1} />);
    expect(container.querySelector("nav")).toHaveAttribute("hidden");
    expect(container.querySelector("[data-slot=select-trigger]")).toBeInTheDocument();
  });

  it("uses the settings navigation at 960px and hides the Select", () => {
    layout.width = 960;
    const { container } = render(<SettingsWorkspace epoch={1} />);
    expect(container.querySelector("nav")).not.toHaveAttribute("hidden");
    expect(container.querySelector("[data-slot=select-trigger]")?.parentElement).toHaveAttribute("hidden");
  });
});
