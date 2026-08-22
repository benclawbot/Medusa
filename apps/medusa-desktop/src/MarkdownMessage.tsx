import React from "react";

type MarkdownBlock =
  | { kind: "paragraph"; lines: string[] }
  | { kind: "heading"; level: number; text: string }
  | { kind: "code"; language?: string; text: string }
  | { kind: "list"; ordered: boolean; items: string[] }
  | { kind: "quote"; lines: string[] }
  | { kind: "table"; headings: string[]; rows: string[][] }
  | { kind: "rule" };

function splitTableRow(line: string): string[] {
  const value = line.trim().replace(/^\|/, "").replace(/\|$/, "");
  return value.split("|").map((cell) => cell.trim());
}

function isTableDivider(line: string): boolean {
  const cells = splitTableRow(line);
  return cells.length > 0 && cells.every((cell) => /^:?-{3,}:?$/.test(cell));
}

function isBlockStart(lines: string[], index: number): boolean {
  const line = lines[index]?.trim() ?? "";
  if (!line) return true;
  if (/^```/.test(line) || /^#{1,6}\s+/.test(line) || /^([-*_])(?:\s*\1){2,}$/.test(line)) return true;
  if (/^\s*(?:[-+*]|\d+[.)])\s+/.test(line) || /^>/.test(line)) return true;
  return Boolean(lines[index + 1] && line.includes("|") && isTableDivider(lines[index + 1]));
}

function parseMarkdown(source: string): MarkdownBlock[] {
  const lines = source.replace(/\r\n?/g, "\n").split("\n");
  const blocks: MarkdownBlock[] = [];
  let index = 0;

  while (index < lines.length) {
    const line = lines[index] ?? "";
    const trimmed = line.trim();
    if (!trimmed) {
      index += 1;
      continue;
    }

    const fence = trimmed.match(/^```\s*([^`]*)$/);
    if (fence) {
      const code: string[] = [];
      index += 1;
      while (index < lines.length && !/^\s*```\s*$/.test(lines[index] ?? "")) {
        code.push(lines[index] ?? "");
        index += 1;
      }
      if (index < lines.length) index += 1;
      blocks.push({ kind: "code", language: fence[1]?.trim() || undefined, text: code.join("\n") });
      continue;
    }

    const heading = trimmed.match(/^(#{1,6})\s+(.+?)\s*#*$/);
    if (heading) {
      blocks.push({ kind: "heading", level: heading[1].length, text: heading[2] });
      index += 1;
      continue;
    }

    if (/^([-*_])(?:\s*\1){2,}$/.test(trimmed)) {
      blocks.push({ kind: "rule" });
      index += 1;
      continue;
    }

    if (line.includes("|") && lines[index + 1] && isTableDivider(lines[index + 1])) {
      const headings = splitTableRow(line);
      index += 2;
      const rows: string[][] = [];
      while (index < lines.length && lines[index].trim() && lines[index].includes("|")) {
        rows.push(splitTableRow(lines[index]));
        index += 1;
      }
      blocks.push({ kind: "table", headings, rows });
      continue;
    }

    const listItem = trimmed.match(/^(?:([-+*])|(\d+[.)]))\s+(.+)$/);
    if (listItem) {
      const ordered = Boolean(listItem[2]);
      const items: string[] = [];
      while (index < lines.length) {
        const current = (lines[index] ?? "").trim().match(/^(?:([-+*])|(\d+[.)]))\s+(.+)$/);
        if (!current || Boolean(current[2]) !== ordered) break;
        items.push(current[3]);
        index += 1;
      }
      blocks.push({ kind: "list", ordered, items });
      continue;
    }

    if (trimmed.startsWith(">")) {
      const quote: string[] = [];
      while (index < lines.length && (lines[index] ?? "").trim().startsWith(">")) {
        quote.push((lines[index] ?? "").trim().replace(/^>\s?/, ""));
        index += 1;
      }
      blocks.push({ kind: "quote", lines: quote });
      continue;
    }

    const paragraph: string[] = [line];
    index += 1;
    while (index < lines.length && lines[index]?.trim() && !isBlockStart(lines, index)) {
      paragraph.push(lines[index] ?? "");
      index += 1;
    }
    blocks.push({ kind: "paragraph", lines: paragraph });
  }

  return blocks;
}

function safeHref(value: string): string | undefined {
  const href = value.trim();
  return /^(?:https?:|mailto:)/i.test(href) ? href : undefined;
}

function renderInline(value: string, keyPrefix: string): React.ReactNode[] {
  const tokenPattern = /(`[^`\n]+`|\[[^\]\n]+\]\((?:[^()]|\([^()]*\))+\)|https?:\/\/[^\s<]+|\*\*[^*\n]+\*\*|__[^_\n]+__|~~[^~\n]+~~|\*[^*\n]+\*|_[^_\n]+_)/g;
  const children: React.ReactNode[] = [];
  let cursor = 0;
  let tokenIndex = 0;

  const pushText = (text: string) => {
    text.split("\n").forEach((part, index) => {
      if (index > 0) children.push(<br key={`${keyPrefix}-br-${tokenIndex++}`} />);
      if (part) children.push(part);
    });
  };

  for (const match of value.matchAll(tokenPattern)) {
    const start = match.index ?? 0;
    const token = match[0];
    pushText(value.slice(cursor, start));
    const key = `${keyPrefix}-${tokenIndex++}`;
    if (token.startsWith("`") && token.endsWith("`")) {
      children.push(<code key={key}>{token.slice(1, -1)}</code>);
    } else if (token.startsWith("[") && token.includes("](")) {
      const closing = token.lastIndexOf("](");
      const label = token.slice(1, closing);
      const href = safeHref(token.slice(closing + 2, -1));
      children.push(href ? <a key={key} href={href} target="_blank" rel="noreferrer" title="Ctrl+click to open" onClick={(event) => { if (!event.ctrlKey) event.preventDefault(); }}>{renderInline(label, key)}</a> : label);
    } else if (/^https?:\/\//i.test(token)) {
      const href = token.replace(/[.,;:!?\)\]\}]+$/, "");
      children.push(<a key={key} href={href} target="_blank" rel="noreferrer" title="Ctrl+click to open" onClick={(event) => { if (!event.ctrlKey) event.preventDefault(); }}>{href}</a>);
      if (href.length < token.length) children.push(token.slice(href.length));
    } else if ((token.startsWith("**") && token.endsWith("**")) || (token.startsWith("__") && token.endsWith("__"))) {
      children.push(<strong key={key}>{renderInline(token.slice(2, -2), key)}</strong>);
    } else if (token.startsWith("~~") && token.endsWith("~~")) {
      children.push(<del key={key}>{renderInline(token.slice(2, -2), key)}</del>);
    } else if (token.startsWith("*") || token.startsWith("_")) {
      children.push(<em key={key}>{renderInline(token.slice(1, -1), key)}</em>);
    }
    cursor = start + token.length;
  }
  pushText(value.slice(cursor));
  return children;
}

export function MarkdownMessage({ text }: { text: string }) {
  const blocks = parseMarkdown(typeof text === "string" ? text : String(text ?? ""));
  return (
    <div className="markdown-message">
      {blocks.map((block, index) => {
        const key = `markdown-${index}`;
        switch (block.kind) {
          case "heading": {
            const Heading = `h${Math.min(block.level, 6)}` as keyof JSX.IntrinsicElements;
            return <Heading key={key}>{renderInline(block.text, key)}</Heading>;
          }
          case "paragraph":
            return <p key={key}>{renderInline(block.lines.join("\n"), key)}</p>;
          case "code":
            return <pre key={key} className="markdown-code"><code>{block.text}</code>{block.language && <span className="markdown-code-language">{block.language}</span>}</pre>;
          case "list": {
            const List = block.ordered ? "ol" : "ul";
            return <List key={key}>{block.items.map((item, itemIndex) => <li key={`${key}-${itemIndex}`}>{renderInline(item, `${key}-${itemIndex}`)}</li>)}</List>;
          }
          case "quote":
            return <blockquote key={key}>{renderInline(block.lines.join("\n"), key)}</blockquote>;
          case "table":
            return <div className="markdown-table-wrap" key={key}><table><thead><tr>{block.headings.map((heading, cell) => <th key={`${key}-h-${cell}`}>{renderInline(heading, `${key}-h-${cell}`)}</th>)}</tr></thead><tbody>{block.rows.map((row, rowIndex) => <tr key={`${key}-r-${rowIndex}`}>{block.headings.map((_, cell) => <td key={`${key}-r-${rowIndex}-${cell}`}>{renderInline(row[cell] ?? "", `${key}-${rowIndex}-${cell}`)}</td>)}</tr>)}</tbody></table></div>;
          case "rule":
            return <hr key={key} />;
        }
      })}
    </div>
  );
}
