from pathlib import Path


def replace(path: str, old: str, new: str) -> None:
    file = Path(path)
    text = file.read_text()
    if old not in text:
        raise SystemExit(f"missing anchor in {path}: {old[:100]!r}")
    file.write_text(text.replace(old, new, 1))


lib = Path("crates/medusa-runtime/src/lib.rs")
text = lib.read_text()
start = text.index("fn load_skill(")
end = text.index("fn run_prompt(", start)
text = text[:start] + text[end:]
text = text.replace("let skill = load_skill(&state.repo, &selector)?;", "let skill = load_selected_skill(&state.repo, &selector)?;")
lib.write_text(text)

support = Path("crates/medusa-runtime/src/support.rs")
text = support.read_text()
anchor = '''    let Some((scope, path)) = matches.pop() else {
        return Err(RuntimeError::InvalidCommand(format!(
            "skill {name} disappeared while resolving its path"
        )));
    };
    let bytes = fs::read(&path)?;
'''
replacement = '''    let Some((scope, path)) = matches.pop() else {
        return Err(RuntimeError::InvalidCommand(format!(
            "skill {name} disappeared while resolving its path"
        )));
    };
    let approved_root = repo.join(".medusa/skills");
    if scope == "project" && approved_root.is_dir() {
        let canonical_root = fs::canonicalize(&approved_root)?;
        if path.starts_with(&canonical_root) {
            let resolved = crate::skill_dependencies::resolve_project_skill(
                &approved_root,
                name,
                MAX_SKILL_CONTEXT_BYTES,
            )
            .map_err(RuntimeError::InvalidCommand)?;
            return Ok(SelectedSkill {
                name: name.to_owned(),
                scope: scope.to_owned(),
                content: resolved.content,
            });
        }
    }
    let bytes = fs::read(&path)?;
'''
if anchor not in text:
    raise SystemExit("support loader anchor not found")
support.write_text(text.replace(anchor, replacement, 1))
