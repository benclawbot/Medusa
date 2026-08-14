from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text()
    if old not in text:
        raise SystemExit(f"missing anchor in {path}: {old[:120]!r}")
    p.write_text(text.replace(old, new, 1))

# Test-only source inclusion should not turn newly shipped transaction APIs into dead-code errors.
replace_once(
    "crates/medusa-agent/tests/identity_transaction_safety.rs",
    '#[path = "../src/transaction.rs"]\nmod transaction;\n',
    '#[allow(dead_code)]\n#[path = "../src/transaction.rs"]\nmod transaction;\n',
)

# Successful selective revert invalidates durable session verification evidence. Repository/review
# fingerprints are content-derived, so the inverse mutation itself forces refreshed review snapshots
# to carry the new repository/file fingerprints.
tx = Path("crates/medusa-agent/src/transaction.rs")
text = tx.read_text()
old = '''    apply_selective_revert(repo, mutation_id, &context)\n}\n\npub fn preview_selective_revert'''
new = '''    let outcome = apply_selective_revert(repo, mutation_id, &context)?;\n    let mut session = crate::session::load(repo, session_id)?;\n    session.evidence.clear();\n    session.updated_at = time::OffsetDateTime::now_utc();\n    crate::session::persist(&session)?;\n    Ok(outcome)\n}\n\npub fn preview_selective_revert'''
if old not in text:
    raise SystemExit("missing session revert lifecycle anchor")
tx.write_text(text.replace(old, new, 1))

# Preserve the public exhaustive FrontendControlResult API. Encode new result payloads through the
# already-shipped CommandAccepted envelope rather than adding enum variants (a semver-major break).
front = Path("crates/medusa-daemon/src/frontend_control.rs")
text = front.read_text()
variants = '''    SelectiveRevertPreview {\n        session_id: String,\n        mutation_id: String,\n        path: String,\n        start_byte: usize,\n        remove_len: usize,\n        restore_len: usize,\n    },\n    SelectiveRevertApplied {\n        session_id: String,\n        mutation_id: String,\n        inverse_mutation_ids: Vec<String>,\n    },\n'''
if variants not in text:
    raise SystemExit("missing selective revert result variants")
text = text.replace(variants, "", 1)
old_preview = '''                Ok(FrontendControlResult::SelectiveRevertPreview {\n                    session_id,\n                    mutation_id: preview.mutation_id,\n                    path: preview.path,\n                    start_byte: preview.start_byte,\n                    remove_len: preview.remove_len,\n                    restore_len: preview.restore_len,\n                })'''
new_preview = '''                let command = serde_json::to_string(&serde_json::json!({\n                    "type": "selective_revert_preview",\n                    "mutation_id": preview.mutation_id,\n                    "path": preview.path,\n                    "start_byte": preview.start_byte,\n                    "remove_len": preview.remove_len,\n                    "restore_len": preview.restore_len,\n                }))\n                .map_err(|error| FrontendControlError::InvalidCommand(error.to_string()))?;\n                Ok(FrontendControlResult::CommandAccepted { session_id, command })'''
if old_preview not in text:
    raise SystemExit("missing preview result anchor")
text = text.replace(old_preview, new_preview, 1)
old_apply = '''                Ok(FrontendControlResult::SelectiveRevertApplied {\n                    session_id,\n                    mutation_id: mutation_id.clone(),\n                    inverse_mutation_ids: outcome.mutation_ids,\n                })'''
new_apply = '''                let command = serde_json::to_string(&serde_json::json!({\n                    "type": "selective_revert_applied",\n                    "mutation_id": mutation_id,\n                    "inverse_mutation_ids": outcome.mutation_ids,\n                    "verification_invalidated": true,\n                    "review_refresh_required": true,\n                }))\n                .map_err(|error| FrontendControlError::InvalidCommand(error.to_string()))?;\n                Ok(FrontendControlResult::CommandAccepted { session_id, command })'''
if old_apply not in text:
    raise SystemExit("missing apply result anchor")
front.write_text(text.replace(old_apply, new_apply, 1))

# Telegram already handles CommandAccepted, so remove the temporary enum arms.
service = Path("crates/medusa-daemon/src/telegram/service.rs")
text = service.read_text()
text = text.replace(
    '''            | FrontendControlResult::Status { session_id, .. }\n            | FrontendControlResult::SelectiveRevertPreview { session_id, .. }\n            | FrontendControlResult::SelectiveRevertApplied { session_id, .. } => {\n                Some(session_id.clone())\n            }''',
    '''            | FrontendControlResult::Status { session_id, .. } => Some(session_id.clone())''',
    1,
)
service.write_text(text)

# Strengthen black-box proof: successful revert clears previously durable verification evidence.
test = Path("crates/medusa-agent/tests/production_mutation_revert.rs")
text = test.read_text()
anchor = '''    assert_eq!(resumed.id, session.id);\n    let preview ='''
replacement = '''    assert_eq!(resumed.id, session.id);\n    // Seed durable verification evidence to prove a successful revert invalidates it.\n    let session_path = directory\n        .path()\n        .join(".medusa/sessions")\n        .join(format!("{}.json", session.id));\n    let mut persisted: serde_json::Value =\n        serde_json::from_slice(&fs::read(&session_path).expect("session snapshot")).expect("json");\n    persisted["evidence"] = json!(["verification-before-revert"]);\n    fs::write(&session_path, serde_json::to_vec_pretty(&persisted).unwrap()).expect("seed evidence");\n    let preview ='''
if anchor not in text:
    raise SystemExit("missing restart test anchor")
text = text.replace(anchor, replacement, 1)
anchor2 = '''    assert_eq!(outcome.mutation_ids.len(), 1);\n}'''
replacement2 = '''    assert_eq!(outcome.mutation_ids.len(), 1);\n    let refreshed = restarted\n        .load_session(directory.path(), session.id.as_str())\n        .expect("session after revert");\n    assert!(refreshed.evidence.is_empty());\n}'''
if anchor2 not in text:
    raise SystemExit("missing revert assertion anchor")
test.write_text(text.replace(anchor2, replacement2, 1))
