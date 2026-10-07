import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { HelpWorkspace } from "./HelpWorkspace";

vi.mock("@/i18n/I18nProvider", () => ({ useI18n: () => ({ t: (key: string) => key }) }));

describe("HelpWorkspace", () => {
  it("covers the operational help topics and accessible status", () => {
    render(<HelpWorkspace />);
    expect(screen.getByText("help.title")).toBeInTheDocument();
    expect(screen.getByText("help.shortcut.title")).toBeInTheDocument();
    expect(screen.getByText("help.permissions.title")).toBeInTheDocument();
    expect(screen.getByText("help.context.title")).toBeInTheDocument();
    expect(screen.getByText("help.troubleshooting.title")).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("help.accessible_status");
  });
});
