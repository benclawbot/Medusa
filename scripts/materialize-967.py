from pathlib import Path

path = Path("crates/medusa-agent/src/verification_authority.rs")
text = path.read_text()
old = """    let mut paths = repository_state_paths(repo, components, evidence_relative.as_deref())?;
    paths.sort();
"""
new = """    let mut paths = repository_state_paths(repo, components, evidence_relative.as_deref())?;
    // `.medusa` is Medusa-owned execution state. Session journals, request manifests, and
    // other runtime files can change concurrently with authoritative verification and must not
    // invalidate the product-state snapshot. Product/source paths outside this reserved runtime
    // namespace remain fully fingerprinted and still fail closed on genuine drift.
    paths.retain(|relative| {
        let normalized = relative.to_string_lossy().replace('\\\\', "/");
        normalized != ".medusa" && !normalized.starts_with(".medusa/")
    });
    paths.sort();
"""
if old not in text:
    raise SystemExit("verification path collection anchor not found")
text = text.replace(old, new, 1)

marker = """    #[test]
    fn parallel_repository_fingerprint_is_order_stable_and_content_sensitive() {
"""
test = r'''    #[test]
    fn medusa_runtime_state_does_not_destabilize_repository_fingerprint() {
        let directory = tempfile::tempdir().expect("repository");
        fs::write(directory.path().join("input.txt"), "stable\n").expect("input");
        let component =
            ChangedComponent::new(ChangeKind::Modified, "input.txt").expect("component");
        let evidence_root = directory.path().join(".medusa/evidence");
        fs::create_dir_all(directory.path().join(".medusa/sessions")).expect("runtime state");
        fs::write(
            directory.path().join(".medusa/sessions/session.json"),
            "{\"revision\":1}\n",
        )
        .expect("session state");

        let before = complete_repository_state_fingerprint(
            directory.path(),
            &evidence_root,
            &[component.clone()],
        )
        .expect("initial fingerprint");
        fs::write(
            directory.path().join(".medusa/sessions/session.json"),
            "{\"revision\":2}\n",
        )
        .expect("updated session state");
        let after_runtime = complete_repository_state_fingerprint(
            directory.path(),
            &evidence_root,
            &[component.clone()],
        )
        .expect("runtime-only fingerprint");
        assert_eq!(before, after_runtime, "Medusa-owned runtime state must be excluded");

        fs::write(directory.path().join("input.txt"), "changed\n").expect("product drift");
        let after_product = complete_repository_state_fingerprint(
            directory.path(),
            &evidence_root,
            &[component],
        )
        .expect("product fingerprint");
        assert_ne!(before, after_product, "product drift must remain authoritative");
    }

'''
if marker not in text:
    raise SystemExit("test insertion anchor not found")
text = text.replace(marker, test + marker, 1)
path.write_text(text)
