from pathlib import Path

path = Path("crates/medusa-runtime/src/error.rs")
source = path.read_text()
old = """            RuntimeCommand::Submit(draft) => {
                let _ = events.send(RuntimeEvent::Started);
                let event = match run_prompt(&mut state, draft, &events, &cancel, &submission) {
"""
new = """            RuntimeCommand::Submit { draft, accepted } => {
                let _ = events.send(RuntimeEvent::Started);
                let event = match run_prompt(
                    &mut state,
                    draft,
                    &events,
                    &cancel,
                    &submission,
                    Some(accepted),
                ) {
"""
if new not in source:
    count = source.count(old)
    if count != 1:
        raise SystemExit(f"expected one alternate worker target, found {count}")
    path.write_text(source.replace(old, new, 1))
