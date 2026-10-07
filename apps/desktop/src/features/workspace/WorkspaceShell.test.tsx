import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { shouldUseIconSidebar, WorkspaceShell } from "./WorkspaceShell";

vi.mock("@/i18n/I18nProvider", () => ({
  useI18n: () => ({ t: (key: string) => ({ "nav.label": "Workspace navigation", "nav.sessions": "Sessions", "nav.dictionary": "Dictionary", "nav.settings": "Settings", "nav.help": "Help" }[key] ?? key) }),
}));
vi.mock("@/hooks/use-element-width", () => ({ useElementWidth: () => ({ ref: vi.fn(), width: 1_100 }) }));
vi.mock("./RuntimeStatusBar", () => ({ RuntimeStatusBar: () => <footer data-testid="runtime-status-bar">runtime</footer> }));
vi.mock("./WorkspaceTaskStatusAnnouncer", () => ({ WorkspaceTaskStatusAnnouncer: () => <p role="status">task status</p> }));

describe("WorkspaceShell", () => {
  it("switches between session, settings and help views", () => {
    render(<WorkspaceShell views={{ sessions: <p>sessions view</p>, dictionary: <p>dictionary view</p>, settings: <p>settings view</p>, help: <p>help view</p> }} />);
    expect(screen.getByText("sessions view")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    expect(screen.getByText("settings view")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Help" }));
    expect(screen.getByText("help view")).toBeInTheDocument();
  });

  it("keeps the navigation and footer in the sidebar while exposing a single main region", () => {
    const { container } = render(<WorkspaceShell views={{ sessions: <p>sessions</p>, dictionary: <p>dictionary</p>, settings: <p>settings</p>, help: <p>help</p> }} />);
    expect(container.querySelector('[data-slot="sidebar-content"]')).toBeInTheDocument();
    expect(container.querySelector('[data-slot="sidebar-footer"]')).toBeInTheDocument();
    expect(container.querySelectorAll("main")).toHaveLength(1);
    expect(container.querySelector('[data-slot="sidebar-inset"]')).toHaveTextContent("sessions");
  });

  it("allows the owner-controlled sidebar to be toggled at a wide width", () => {
    const { container } = render(<WorkspaceShell views={{ sessions: <p>sessions</p>, dictionary: <p>dictionary</p>, settings: <p>settings</p>, help: <p>help</p> }} />);
    const sidebar = container.querySelector('[data-slot="sidebar"]');
    expect(sidebar).toHaveAttribute("data-state", "expanded");
    fireEvent.click(container.querySelector('[data-slot="sidebar-trigger"]')!);
    expect(sidebar).toHaveAttribute("data-state", "collapsed");
  });

  it("uses the icon sidebar at the 960px shell width", () => {
    expect(shouldUseIconSidebar(960)).toBe(true);
    expect(shouldUseIconSidebar(1_040)).toBe(false);
  });
});
