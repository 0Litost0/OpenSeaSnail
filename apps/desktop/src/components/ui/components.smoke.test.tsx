import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { Empty, EmptyDescription, EmptyHeader, EmptyTitle } from "./empty";
import { Spinner } from "./spinner";

describe("基础 UI 组件", () => {
  it("renders empty state and spinner in jsdom", () => {
    render(
      <Empty>
        <EmptyHeader>
          <EmptyTitle>暂无内容</EmptyTitle>
          <EmptyDescription>请稍后再试</EmptyDescription>
        </EmptyHeader>
        <Spinner aria-label="加载中" />
      </Empty>,
    );
    expect(screen.getByText("暂无内容")).toBeInTheDocument();
    expect(screen.getByLabelText("加载中")).toBeInTheDocument();
  });
});
