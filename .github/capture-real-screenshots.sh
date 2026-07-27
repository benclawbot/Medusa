#!/usr/bin/env bash
set -euo pipefail

npm install --no-save playwright @xterm/xterm
npx playwright install --with-deps chromium

cat >> crates/medusa-tui/src/lib.rs <<'RUST'

#[doc(hidden)]
pub fn write_readme_screenshot_frame() -> std::io::Result<()> {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let directory = std::env::temp_dir().join("medusa-readme-tui");
    std::fs::create_dir_all(&directory)?;
    let mut app = AppState::new(
        directory.clone(),
        "readme-screenshot",
        "",
        Arc::new(UnsupportedClipboard),
    )?;
    let _ = app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)));
    app.composer = crate::input::ComposerState::new("");
    app.status = "running targeted verification".to_owned();
    app.model_label = Some("MiniMax-M3".to_owned());
    app.effort_label = Some("auto".to_owned());
    app.active_turn = 4;
    app.input_tokens = 12_480;
    app.output_tokens = 1_936;
    app.total_tokens = 14_416;
    app.cache_read_input_tokens = 8_204;
    app.model_elapsed_millis = 18_200;
    app.plan = Some(app::TranscriptPlan {
        steps: vec![
            app::TranscriptPlanStep { title: "Inspect the failing assertion".to_owned(), state: app::TranscriptPlanStepState::Completed },
            app::TranscriptPlanStep { title: "Apply the focused correction".to_owned(), state: app::TranscriptPlanStepState::Completed },
            app::TranscriptPlanStep { title: "Run targeted verification".to_owned(), state: app::TranscriptPlanStepState::Active },
            app::TranscriptPlanStep { title: "Report evidence".to_owned(), state: app::TranscriptPlanStepState::Pending },
        ],
    });
    app.transcript = vec![
        TranscriptEntry::User(PromptDraft { text: "Fix the flaky runtime test and verify the result.".to_owned(), attachments: Vec::new() }),
        TranscriptEntry::Assistant("I found a stale assertion in the runtime activity test. The production path already forwards tool output details, so I’m aligning the test and running the focused suite.".to_owned()),
        TranscriptEntry::Activity(TranscriptActivity { id: Some("read".to_owned()), kind: TranscriptActivityKind::Tool, title: "Read crates/medusa-runtime/src/tests.rs".to_owned(), details: vec!["Located the stale completed activity expectation".to_owned()] }),
        TranscriptEntry::Activity(TranscriptActivity { id: Some("patch".to_owned()), kind: TranscriptActivityKind::Tool, title: "Patch completed activity expectation".to_owned(), details: vec!["Updated the assertion to preserve tool output details".to_owned()] }),
        TranscriptEntry::Activity(TranscriptActivity { id: Some("verify".to_owned()), kind: TranscriptActivityKind::Verification, title: "cargo test -p medusa-runtime".to_owned(), details: vec!["running targeted verification".to_owned()] }),
    ];
    app.begin_run();
    let identity = UiIdentity::for_repo(&directory);
    let frame = render_frame(&identity, &app, 140, 42);
    let mut stdout = std::io::stdout();
    draw_frame(&mut stdout, 140, &frame, None)?;
    stdout.flush()
}
RUST
mkdir -p crates/medusa-tui/examples
cat > crates/medusa-tui/examples/readme_screenshot.rs <<'RUST'
fn main() -> std::io::Result<()> {
    medusa_tui::write_readme_screenshot_frame()
}
RUST
cargo run -q -p medusa-tui --example readme_screenshot > /tmp/medusa-tui.ansi

cd apps/medusa-desktop
npm ci
python3 - <<'PY'
from pathlib import Path
app = Path('src/App.tsx')
app.write_text(app.read_text().replace('from "./runtime";', 'from "./runtime.screenshot";'))
PY
cat > src/runtime.screenshot.ts <<'TS'
export type Effort = "low" | "medium" | "high" | "auto";
export type SubmitDisposition = "started" | "queued";
export interface FileAttachment { kind: "file"; path: string; }
export interface ImageAttachment { kind: "image"; name: string; dataUrl: string; }
export interface TextAttachment { kind: "text"; name: string; text: string; }
export type DesktopAttachment = FileAttachment | ImageAttachment | TextAttachment;
export interface CommandSuggestion { name: string; usage: string; description: string; }
export interface PlanStep { title: string; status: "pending" | "inProgress" | "completed" | "failed"; }
export interface QuestionOption { label: string; description: string; }
export interface QuestionPrompt { header: string; question: string; options: QuestionOption[]; multiSelect: boolean; }
export interface RuntimeActivity { id?: string; kind: "assistant" | "done" | "error" | "tool" | "verification"; title: string; details: string[]; }
export type RuntimeEvent =
  | { type: "started" }
  | { type: "assistantText"; text: string }
  | { type: "activity"; activity: RuntimeActivity }
  | { type: "plan"; steps: PlanStep[] }
  | { type: "question"; prompts: QuestionPrompt[] }
  | { type: "usage"; inputTokens: number; outputTokens: number; cacheReadInputTokens: number; cacheCreationInputTokens: number; modelElapsedMillis: number }
  | { type: "progress"; turn: number }
  | { type: "settings"; model: string; effort: string; planMode: boolean; credentialConfigured: boolean }
  | { type: "notice"; title: string; details: string[] }
  | { type: "newSession" }
  | { type: "compacted"; message: string }
  | { type: "completed"; sessionId: string }
  | { type: "turnFinished" }
  | { type: "cancelled" }
  | { type: "failed"; message: string };
let delivered = false;
export async function startRuntime() { return { runtimeId: "readme-runtime", repo: "/workspace/medusa" }; }
export async function closeRuntime() {}
export async function submitRuntime(): Promise<SubmitDisposition> { return "started"; }
export async function runRuntimeCommand() {}
export async function commandSuggestions(): Promise<CommandSuggestion[]> { return []; }
export async function cancelRuntime() {}
export async function configureRuntime() {}
export async function pollRuntime(): Promise<RuntimeEvent[]> {
  if (delivered) return [];
  delivered = true;
  return [
    { type: "settings", model: "MiniMax-M3", effort: "effort:auto", planMode: true, credentialConfigured: true },
    { type: "started" },
    { type: "progress", turn: 4 },
    { type: "assistantText", text: "I found the stale runtime assertion. I’m applying the focused correction and running the targeted suite." },
    { type: "plan", steps: [
      { title: "Inspect the failing assertion", status: "completed" },
      { title: "Apply the focused correction", status: "completed" },
      { title: "Run targeted verification", status: "inProgress" },
      { title: "Report evidence", status: "pending" },
    ] },
    { type: "activity", activity: { id: "read", kind: "tool", title: "Read crates/medusa-runtime/src/tests.rs", details: ["Located the stale completed activity expectation"] } },
    { type: "activity", activity: { id: "patch", kind: "tool", title: "Updated completed activity expectation", details: ["Preserved forwarded tool output details"] } },
    { type: "activity", activity: { id: "verify", kind: "verification", title: "cargo test -p medusa-runtime", details: ["Targeted verification running"] } },
    { type: "usage", inputTokens: 12480, outputTokens: 1936, cacheReadInputTokens: 8204, cacheCreationInputTokens: 0, modelElapsedMillis: 18200 },
  ];
}
TS
npm run build
npm run dev -- --host 127.0.0.1 --port 4173 > /tmp/medusa-vite.log 2>&1 &
VITE_PID=$!
cd ../..
for i in $(seq 1 60); do curl -fsS http://127.0.0.1:4173 >/dev/null && break; sleep 1; done

cat > capture-real-screenshots.mjs <<'JS'
import { chromium } from 'playwright';
import fs from 'node:fs';
import path from 'node:path';
const browser = await chromium.launch({ headless: true });
const page = await browser.newPage({ viewport: { width: 1500, height: 980 }, deviceScaleFactor: 1 });
await page.goto('http://127.0.0.1:4173', { waitUntil: 'networkidle' });
await page.waitForTimeout(1800);
await page.screenshot({ path: 'docs/assets/medusa-desktop.png', fullPage: true });
const ansi = fs.readFileSync('/tmp/medusa-tui.ansi', 'utf8');
const css = path.resolve('node_modules/@xterm/xterm/css/xterm.css');
const module = path.resolve('node_modules/@xterm/xterm/lib/xterm.mjs');
const html = `<!doctype html><html><head><meta charset="utf-8"><link rel="stylesheet" href="file://${css}"><style>html,body{margin:0;background:#080c12}.shell{padding:28px}.terminal{display:inline-block;border:1px solid #344256;border-radius:12px;overflow:hidden;box-shadow:0 18px 50px #0008}</style></head><body><div class="shell"><div id="terminal" class="terminal"></div></div><script type="module">import { Terminal } from 'file://${module}'; const term=new Terminal({cols:140,rows:42,convertEol:true,fontFamily:'SFMono-Regular,Consolas,Liberation Mono,monospace',fontSize:14,lineHeight:1.1,theme:{background:'#080c12',foreground:'#dbe2ec',cursor:'#dbe2ec',black:'#080c12',red:'#ff6b6b',green:'#70dc9b',yellow:'#ffca5c',blue:'#6fc2ff',magenta:'#d49aff',cyan:'#72d7ff',white:'#e6edf7',brightBlack:'#66758a'}}); term.open(document.getElementById('terminal')); term.write(${JSON.stringify(ansi)});</script></body></html>`;
fs.writeFileSync('/tmp/medusa-tui.html', html);
const tui = await browser.newPage({ viewport: { width: 1800, height: 900 }, deviceScaleFactor: 1 });
await tui.goto('file:///tmp/medusa-tui.html');
await tui.waitForTimeout(800);
await tui.locator('.shell').screenshot({ path: 'docs/assets/medusa-tui.png' });
await browser.close();
JS
node capture-real-screenshots.mjs
kill "$VITE_PID" || true

rm docs/assets/medusa-tui.svg docs/assets/medusa-desktop.svg
python3 - <<'PY'
from pathlib import Path
p = Path('README.md')
s = p.read_text().replace('docs/assets/medusa-tui.svg', 'docs/assets/medusa-tui.png').replace('docs/assets/medusa-desktop.svg', 'docs/assets/medusa-desktop.png')
p.write_text(s)
PY

git checkout -- crates/medusa-tui/src/lib.rs apps/medusa-desktop/src/App.tsx
rm -f crates/medusa-tui/examples/readme_screenshot.rs apps/medusa-desktop/src/runtime.screenshot.ts capture-real-screenshots.mjs
rm -f .github/workflows/capture-real-screenshots.yml .github/capture-real-screenshots.sh .github/capture-real-screenshots.trigger

git config user.name github-actions[bot]
git config user.email 41898282+github-actions[bot]@users.noreply.github.com
git add README.md docs/assets .github crates/medusa-tui apps/medusa-desktop
git commit -m "docs: replace illustrations with actual product screenshots"
git push origin HEAD:fix/real-product-screenshots
