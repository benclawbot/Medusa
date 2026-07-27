#!/usr/bin/env bash
set -euo pipefail

npm install --no-save playwright @xterm/xterm
npx playwright install chromium

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
        TranscriptEntry::User(PromptDraft { text: "Fix the flaky runtime test and verify the result.".to_owned(), attachments: Vec::new(), revision: 0 }),
        TranscriptEntry::Assistant("I found a stale assertion in the runtime activity test. The production path already forwards tool output details, so I’m aligning the test and running the focused suite.".to_owned()),
        TranscriptEntry::Activity(TranscriptActivity { id: Some("read".to_owned()), kind: TranscriptActivityKind::Tool, title: "Read crates/medusa-runtime/src/tests.rs".to_owned(), details: vec!["Located the stale completed activity expectation".to_owned()] }),
        TranscriptEntry::Activity(TranscriptActivity { id: Some("patch".to_owned()), kind: TranscriptActivityKind::Tool, title: "Patch completed activity expectation".to_owned(), details: vec!["Updated the assertion to preserve tool output details".to_owned()] }),
        TranscriptEntry::Activity(TranscriptActivity { id: Some("verify".to_owned()), kind: TranscriptActivityKind::Verification, title: "cargo test -p medusa-runtime".to_owned(), details: vec!["running targeted verification".to_owned()] }),
    ];
    app.begin_run();
    let identity = UiIdentity::for_repo(&directory);
    let frame = render_frame(&identity, &app, 140, 42);
    let mut stdout = std::io::stdout();
    crate::render::support::draw_frame(&mut stdout, 140, &frame, None)?;
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

cat > /tmp/capture-tui.mjs <<'JS'
import { chromium } from 'playwright';
import fs from 'node:fs';
import path from 'node:path';
const ansi = fs.readFileSync('/tmp/medusa-tui.ansi', 'utf8');
const css = fs.readFileSync(path.resolve('node_modules/@xterm/xterm/css/xterm.css'), 'utf8');
const module = path.resolve('node_modules/@xterm/xterm/lib/xterm.mjs');
const html = `<!doctype html><html><head><meta charset="utf-8"><style>${css}\nhtml,body{margin:0;background:#080c12}.shell{padding:28px;display:inline-block}.terminal{display:inline-block;border:1px solid #344256;border-radius:12px;overflow:hidden;box-shadow:0 18px 50px #0008}</style></head><body><div class="shell"><div id="terminal" class="terminal"></div></div><script type="module">import { Terminal } from 'file://${module}'; window.captureReady=false; const term=new Terminal({cols:140,rows:42,convertEol:true,fontFamily:'SFMono-Regular,Consolas,Liberation Mono,monospace',fontSize:14,lineHeight:1.1,theme:{background:'#080c12',foreground:'#dbe2ec',cursor:'#dbe2ec',black:'#080c12',red:'#ff6b6b',green:'#70dc9b',yellow:'#ffca5c',blue:'#6fc2ff',magenta:'#d49aff',cyan:'#72d7ff',white:'#e6edf7',brightBlack:'#66758a'}}); term.open(document.getElementById('terminal')); term.write(${JSON.stringify(ansi)},()=>{window.captureReady=true;});</script></body></html>`;
fs.writeFileSync('/tmp/medusa-tui.html', html);
const browser = await chromium.launch({ headless: true });
const page = await browser.newPage({ viewport: { width: 1800, height: 1000 }, deviceScaleFactor: 1 });
await page.goto('file:///tmp/medusa-tui.html');
await page.waitForFunction(() => window.captureReady === true);
await page.waitForFunction(() => document.querySelectorAll('.xterm-rows > div').length >= 42);
const box = await page.locator('.shell').boundingBox();
if (!box || box.height < 500) throw new Error(`TUI capture too short: ${box?.width}x${box?.height}`);
await page.locator('.shell').screenshot({ path: 'docs/assets/medusa-tui.png' });
await browser.close();
JS
node /tmp/capture-tui.mjs

python3 - <<'PY'
from pathlib import Path
p = Path('docs/assets/medusa-tui.png').read_bytes()
assert p[:8] == b'\x89PNG\r\n\x1a\n'
w = int.from_bytes(p[16:20], 'big')
h = int.from_bytes(p[20:24], 'big')
assert w >= 1200 and h >= 500, (w, h)
print(f'TUI PNG: {w}x{h}')
PY

git checkout -- crates/medusa-tui/src/lib.rs
rm -f crates/medusa-tui/examples/readme_screenshot.rs .github/fix-tui-screenshot.sh
git checkout origin/main -- .github/workflows/architecture-policy.yml

git config user.name github-actions[bot]
git config user.email 41898282+github-actions[bot]@users.noreply.github.com
git add docs/assets/medusa-tui.png .github crates/medusa-tui
git commit -m "docs: recapture full-height real TUI screenshot"
git push origin HEAD:fix/real-product-screenshots
