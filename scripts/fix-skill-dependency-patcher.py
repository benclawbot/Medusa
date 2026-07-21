from pathlib import Path

path = Path("crates/medusa-runtime/src/skill_dependencies.rs")
text = path.read_text()
text = text.replace("reverse_dependents(&graph,", "reverse_dependents_in_graph(&graph,")
text = text.replace("fn reverse_dependents(\n    graph:", "fn reverse_dependents_in_graph(\n    graph:")
path.write_text(text)
