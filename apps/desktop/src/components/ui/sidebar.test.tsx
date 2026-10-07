import { fireEvent, render } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { Sidebar, SidebarProvider, useSidebar } from "./sidebar";

function ToggleProbe() {
  const { state, toggleSidebar } = useSidebar();
  return <button data-state={state} onClick={toggleSidebar}>toggle</button>;
}

describe("SidebarProvider", () => {
  it("is controlled by its container owner without cookie persistence", () => {
    const onOpenChange = vi.fn();
    const before = document.cookie;
    const view = render(<SidebarProvider open onOpenChange={onOpenChange}><ToggleProbe /></SidebarProvider>);
    fireEvent.click(view.getByRole("button", { name: "toggle" }));
    expect(onOpenChange).toHaveBeenCalledWith(false);
    expect(document.cookie).toBe(before);
  });

  it("renders the same sidebar branch without viewport media queries", () => {
    const view = render(<SidebarProvider><Sidebar><span>content</span></Sidebar></SidebarProvider>);
    const sidebar = view.container.querySelector('[data-slot="sidebar"]');
    expect(sidebar).toHaveClass("block");
    expect(sidebar).not.toHaveClass("hidden");
  });
});
