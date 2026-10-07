import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { Progress } from "./progress";

describe("Progress", () => {
  it("marks an indeterminate segment and limits animation to motion-safe mode", () => {
    const { container } = render(<Progress />);
    const indicator = container.querySelector('[data-slot="progress-indicator"]');
    expect(indicator).toHaveAttribute("data-indeterminate", "true");
    expect(indicator).toHaveClass("motion-safe:data-[indeterminate=true]:animate-pulse", "motion-reduce:transition-none");
    expect(indicator).toHaveStyle({ transform: "translateX(-45%)" });
  });

  it("positions a determinate segment from its value", () => {
    const { container } = render(<Progress value={25} />);
    expect(container.querySelector('[data-slot="progress-indicator"]')).toHaveStyle({ transform: "translateX(-75%)" });
    expect(container.querySelector('[data-slot="progress-indicator"]')).not.toHaveAttribute("data-indeterminate");
  });
});
