import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const sourcePath = (file: string) => resolve(process.cwd(), "src", file);
const mobileCss = readFileSync(sourcePath("mobile-navigation.css"), "utf8");
const mainSource = readFileSync(sourcePath("main.tsx"), "utf8");

describe("narrow-window desktop acceptance", () => {
  it("keeps the command rail reachable without overlapping the workspace", () => {
    expect(mobileCss).toContain("@media (max-width: 760px)");
    expect(mobileCss).toContain("grid-template-columns: minmax(0, 1fr)");
    expect(mobileCss).toContain("grid-template-rows: auto minmax(0, 1fr)");
    expect(mobileCss).toMatch(
      /\.sidebar\s*\{[\s\S]*?display:\s*flex;[\s\S]*?flex-direction:\s*row;[\s\S]*?overflow-x:\s*auto;/,
    );
    expect(mobileCss).toContain(".workspace.medusa-workspace");
    expect(mobileCss).toContain("grid-row: 2");
  });

  it("preserves accessible names when controls become icon-only", () => {
    expect(mobileCss).toContain(".sidebar .rail-label");
    expect(mobileCss).toContain(".rail-collapsed .sidebar .rail-label");
    expect(mobileCss).toContain("display: block !important");
    expect(mobileCss).toContain("position: absolute !important");
    expect(mobileCss).toContain("clip: rect(0, 0, 0, 0) !important");
    expect(mobileCss).not.toMatch(/\.sidebar\s*\{[^}]*display:\s*none;/);
  });

  it("loads the canonical dark system after the responsive acceptance layer", () => {
    const mobileLayer = mainSource.indexOf('import "./mobile-navigation.css";');
    const darkTheme = mainSource.indexOf('import "./medusa-codex-dark.css";');
    expect(mobileLayer).toBeGreaterThanOrEqual(0);
    expect(darkTheme).toBeGreaterThan(mobileLayer);
    expect(mainSource).not.toContain('import "./neutral-light.css";');
    expect(mainSource).not.toContain('import "./neumorphism.css";');
    expect(mainSource).not.toContain('import "./neumorphism-a11y.css";');
  });
});
