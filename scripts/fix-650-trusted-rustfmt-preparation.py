#!/usr/bin/env python3
from pathlib import Path

# Add exact-file Rust formatting preparation to the authoritative verification layer.
authority = Path('crates/medusa-agent/src/verification_authority.rs')
text = authority.read_text(encoding='utf-8')
insert_anchor = '''pub fn authoritative_verification_for_components(
'''
preparation = '''pub fn prepare_components_for_verification(
    repo: &Path,
    components: &[ChangedComponent],
) -> MedusaResult<()> {
    let mut rust_paths = components
        .iter()
        .filter(|component| component.kind != ChangeKind::Deleted)
        .map(|component| component.path.as_str())
        .filter(|path| Path::new(path).extension().and_then(|value| value.to_str()) == Some("rs"))
        .filter(|path| repo.join(path).is_file())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    rust_paths.sort();
    rust_paths.dedup();
    if rust_paths.is_empty() {
        return Ok(());
    }
    let edition = rust_edition(repo);
    let mut args = vec!["--edition".to_owned(), edition.to_owned()];
    args.extend(rust_paths.iter().cloned());
    let result = execute_verification_command(repo, "rustfmt", &args)?;
    if !result.passed {
        let stdout = bounded_failure_text(&result.stdout);
        let stderr = bounded_failure_text(&result.stderr);
        return Err(MedusaError::new(
            ErrorCode::VerificationFailed,
            ErrorCategory::Validation,
            format!(
                "trusted rustfmt preparation failed for {}: stdout={stdout}; stderr={stderr}",
                rust_paths.join(",")
            ),
        ));
    }
    Ok(())
}

pub(crate) fn prepare_paths_for_verification(
    repo: &Path,
    paths: &[String],
) -> MedusaResult<()> {
    let components = paths
        .iter()
        .map(|path| ChangedComponent::new(ChangeKind::Modified, path.clone()))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(evidence_error)?;
    prepare_components_for_verification(repo, &components)
}

fn rust_edition(repo: &Path) -> &'static str {
    let manifest = fs::read_to_string(repo.join("Cargo.toml")).unwrap_or_default();
    for edition in ["2024", "2021", "2018"] {
        if manifest.contains(&format!("edition = \\\"{edition}\\\""))
            || manifest.contains(&format!("edition=\\\"{edition}\\\""))
        {
            return edition;
        }
    }
    "2021"
}

fn bounded_failure_text(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    truncate_utf8(text.trim(), 2048)
}

'''
if text.count(insert_anchor) != 1:
    raise SystemExit(f'authority insertion anchor count={text.count(insert_anchor)}')
text = text.replace(insert_anchor, preparation + insert_anchor, 1)

test_anchor = '''    #[test]
    fn command_preview_is_bounded_and_redacts_secret_like_lines() {
'''
test = '''    #[test]
    fn trusted_rustfmt_preparation_normalizes_only_changed_rust_files() {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(directory.path().join("src")).expect("src");
        fs::write(
            directory.path().join("Cargo.toml"),
            "[package]\\nname = \\\"fixture\\\"\\nversion = \\\"0.1.0\\\"\\nedition = \\\"2024\\\"\\n",
        )
        .expect("manifest");
        fs::write(
            directory.path().join("src/lib.rs"),
            "pub fn value()->u32{1}\\n\\n",
        )
        .expect("source");
        fs::write(directory.path().join("untouched.txt"), "unchanged\\n")
            .expect("untouched");
        let component = ChangedComponent::new(ChangeKind::Modified, "src/lib.rs")
            .expect("component");
        prepare_components_for_verification(directory.path(), &[component])
            .expect("preparation");
        assert_eq!(
            fs::read_to_string(directory.path().join("src/lib.rs")).expect("source"),
            "pub fn value() -> u32 {\\n    1\\n}\\n"
        );
        assert_eq!(
            fs::read_to_string(directory.path().join("untouched.txt")).expect("untouched"),
            "unchanged\\n"
        );
    }

''' + test_anchor
if text.count(test_anchor) != 1:
    raise SystemExit(f'authority test anchor count={text.count(test_anchor)}')
authority.write_text(text.replace(test_anchor, test, 1), encoding='utf-8')

# Make rustfmt executable portable on Windows.
verification = Path('crates/medusa-agent/src/verification.rs')
text = verification.read_text(encoding='utf-8')
old = '''        "cargo" => "cargo.exe",
        "bash" => "bash.exe",
'''
new = '''        "cargo" => "cargo.exe",
        "rustfmt" => "rustfmt.exe",
        "bash" => "bash.exe",
'''
if text.count(old) != 1:
    raise SystemExit(f'platform program anchor count={text.count(old)}')
verification.write_text(text.replace(old, new, 1), encoding='utf-8')

# Export public preparation for runtime and keep direct-path adapter crate-private.
lib = Path('crates/medusa-agent/src/lib.rs')
text = lib.read_text(encoding='utf-8')
old = '''pub use verification_authority::{
    AuthoritativeVerificationResult, authoritative_verification_for_components,
    authoritative_verification_for_components_at,
};
'''
new = '''pub use verification_authority::{
    AuthoritativeVerificationResult, authoritative_verification_for_components,
    authoritative_verification_for_components_at, prepare_components_for_verification,
};
'''
if text.count(old) != 1:
    raise SystemExit(f'lib export anchor count={text.count(old)}')
lib.write_text(text.replace(old, new, 1), encoding='utf-8')

# Direct mutation uses the same trusted exact-file preparation.
engine = Path('crates/medusa-agent/src/engine.rs')
text = engine.read_text(encoding='utf-8')
old_import = '    verification_authority::authoritative_verification_for_paths,\n'
new_import = '''    verification_authority::{
        authoritative_verification_for_paths, prepare_paths_for_verification,
    },
'''
if text.count(old_import) != 1:
    raise SystemExit(f'engine import anchor count={text.count(old_import)}')
text = text.replace(old_import, new_import, 1)
call = '                let verification =\n                    match authoritative_verification_for_paths(repo, &changed_paths) {\n'
replacement = '''                prepare_paths_for_verification(repo, &changed_paths)?;
                let verification =
                    match authoritative_verification_for_paths(repo, &changed_paths) {
'''
if text.count(call) != 1:
    raise SystemExit(f'direct verification call anchor count={text.count(call)}')
engine.write_text(text.replace(call, replacement, 1), encoding='utf-8')

# Isolated mutation prepares the exact changed files, then rechecks scope before verification.
coordinator = Path('crates/medusa-runtime/src/mutating_worker_coordinator.rs')
text = coordinator.read_text(encoding='utf-8')
old_import = '''    AgentEngine, AgentExecutionPolicy, AgentUpdate, StepOutcome, TeamMemberContext, TeamRole,
    TeamRuntime, WorkerExecutionController, authoritative_verification_for_components_at,
};
'''
new_import = '''    AgentEngine, AgentExecutionPolicy, AgentUpdate, StepOutcome, TeamMemberContext, TeamRole,
    TeamRuntime, WorkerExecutionController, authoritative_verification_for_components_at,
    prepare_components_for_verification,
};
'''
if text.count(old_import) != 1:
    raise SystemExit(f'coordinator import anchor count={text.count(old_import)}')
text = text.replace(old_import, new_import, 1)
anchor = '''        let worktree_identity = format!(
            "worktree:{}:{}",
            base_head,
            changed_scope_fingerprint(&changed_components)
        );
'''
block = '''        if let Err(error) =
            prepare_components_for_verification(&worker.worktree, &changed_components)
        {
            let retryable = attempt < MAX_ATTEMPTS;
            let recorded = record_attempt_failure(
                &mut controller,
                team,
                events,
                control,
                manager,
                state_path,
                &assignment,
                &worker,
                running,
                Some(&run),
                changed_paths,
                Vec::new(),
                format!("trusted preparation failed: {error}"),
                retryable,
                false,
            )?;
            last_error = Some(recorded.clone());
            if retryable {
                continue;
            }
            return Err(recorded);
        }
        let prepared_components = match manager.changed_components_since(&worker, &base_head) {
            Ok(components) => components,
            Err(error) => {
                let retryable = attempt < MAX_ATTEMPTS;
                let recorded = record_attempt_failure(
                    &mut controller,
                    team,
                    events,
                    control,
                    manager,
                    state_path,
                    &assignment,
                    &worker,
                    running,
                    Some(&run),
                    changed_paths,
                    Vec::new(),
                    format!("failed to inspect prepared changes: {error}"),
                    retryable,
                    false,
                )?;
                last_error = Some(recorded.clone());
                if retryable {
                    continue;
                }
                return Err(recorded);
            }
        };
        if changed_scope_fingerprint(&prepared_components)
            != changed_scope_fingerprint(&changed_components)
        {
            let error = format!(
                "trusted preparation changed repository scope: before={changed_components:?}; after={prepared_components:?}"
            );
            let retryable = attempt < MAX_ATTEMPTS;
            let recorded = record_attempt_failure(
                &mut controller,
                team,
                events,
                control,
                manager,
                state_path,
                &assignment,
                &worker,
                running,
                Some(&run),
                component_paths(&prepared_components),
                Vec::new(),
                error,
                retryable,
                false,
            )?;
            last_error = Some(recorded.clone());
            if retryable {
                continue;
            }
            return Err(recorded);
        }
        let changed_components = prepared_components;
        let changed_paths = component_paths(&changed_components);
        let worktree_identity = format!(
            "worktree:{}:{}",
            base_head,
            changed_scope_fingerprint(&changed_components)
        );
'''
if text.count(anchor) != 1:
    raise SystemExit(f'coordinator preparation anchor count={text.count(anchor)}')
coordinator.write_text(text.replace(anchor, block, 1), encoding='utf-8')

# Permanent semantic gate requires trusted exact-file preparation.
checker = Path('scripts/check-evidence-authority.py')
text = checker.read_text(encoding='utf-8')
anchor = '    "command outputs become receipts": "CommandReceipt::new" in read("crates/medusa-agent/src/verification_authority.rs"),\n'
addition = anchor + '    "trusted exact-file formatting precedes verification": "prepare_components_for_verification" in read("crates/medusa-runtime/src/mutating_worker_coordinator.rs"),\n'
if text.count(anchor) != 1:
    raise SystemExit(f'semantic checker anchor count={text.count(anchor)}')
checker.write_text(text.replace(anchor, addition, 1), encoding='utf-8')
