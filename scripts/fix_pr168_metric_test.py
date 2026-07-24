from pathlib import Path

path = Path("crates/medusa-tui/src/lib.rs")
text = path.read_text()
old = "session 0s · total 2.5k · input 700 · output 1.5k · cache-read 200 · cache-write 100 · cost — · estimated · 600.0 tok/s"
new = "session 0s · total 2.5k · input 700 · output 1.5k · cache-read 200 · cache-write 100 · cost — · estimated · 1.2k tok/s"
if text.count(old) != 1:
    raise SystemExit(f"expected exactly one stale metric assertion, found {text.count(old)}")
path.write_text(text.replace(old, new, 1))
