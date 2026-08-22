import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { MarkdownMessage } from "./MarkdownMessage";

afterEach(() => cleanup());

describe("MarkdownMessage", () => {
  it("renders prose, tables, links, and fenced code", () => {
    render(
      <MarkdownMessage
        text={'## Findings\n\n**Safe** and [source](https://example.com).\n\n| Property | Mechanism |\n| --- | --- |\n| Escape | Containment |\n\n```rust\nfn main() {}\n```'}
      />,
    );

    expect(screen.getByRole("heading", { name: "Findings" })).toBeInTheDocument();
    expect(screen.getByText("Safe")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "source" })).toHaveAttribute("href", "https://example.com");
    expect(screen.getByRole("table")).toBeInTheDocument();
    expect(screen.getByText("fn main() {}")).toBeInTheDocument();
    expect(screen.getByText("rust")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Copy code" })).toBeInTheDocument();
  });

  it("does not turn unsafe markdown links into executable links", () => {
    render(<MarkdownMessage text="[unsafe](javascript:alert(1))" />);

    expect(screen.queryByRole("link")).not.toBeInTheDocument();
    expect(screen.getByText("unsafe")).toBeInTheDocument();
  });
});
