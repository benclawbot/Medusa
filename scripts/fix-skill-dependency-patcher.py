from pathlib import Path

path = Path("crates/medusa-runtime/src/skill_dependencies.rs")
text = path.read_text()
text = text.replace("reverse_dependents(&graph,", "reverse_dependents_in_graph(&graph,")
text = text.replace("fn reverse_dependents(\n    graph:", "fn reverse_dependents_in_graph(\n    graph:")
anchor = '''        let file_type = entry
            .file_type()
            .map_err(|error| format!("inspect {}: {error}", entry.path().display()))?;
        if !file_type.is_dir() {
            continue;
        }
'''
replacement = '''        let file_type = entry
            .file_type()
            .map_err(|error| format!("inspect {}: {error}", entry.path().display()))?;
        if file_type.is_symlink() {
            return Err(format!(
                "approved skill entry is a symlink and is not allowed: {}",
                entry.path().display()
            ));
        }
        if !file_type.is_dir() {
            continue;
        }
'''
if anchor not in text:
    raise SystemExit("skill entry type anchor not found")
path.write_text(text.replace(anchor, replacement, 1))

test = Path("crates/medusa-runtime/tests/skill_dependency_graph.rs")
test_text = test.read_text()
if not test_text.startswith("#![allow(dead_code)]"):
    test.write_text("#![allow(dead_code)]\n\n" + test_text)
