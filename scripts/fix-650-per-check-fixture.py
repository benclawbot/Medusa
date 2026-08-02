from pathlib import Path

path = Path('crates/medusa-runtime/src/mutation_transaction.rs')
text = path.read_text()
old = '''        let worktree_verification = authoritative_verification_for_components(
            &worker.worktree,
            "repo",
            &format!("worktree:{}", changed_scope_fingerprint(&changed_components)),
            &changed_components,
        )
'''
new = '''        let worktree_verification = authoritative_verification_for_components_at(
            &worker.worktree,
            &root.join("evidence/test-worktree"),
            "repo",
            &format!("worktree:{}", changed_scope_fingerprint(&changed_components)),
            &changed_components,
        )
'''
if text.count(old) != 1:
    raise SystemExit(f'expected one worktree verification fixture, found {text.count(old)}')
path.write_text(text.replace(old, new, 1))
