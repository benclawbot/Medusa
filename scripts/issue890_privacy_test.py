from pathlib import Path
p = Path('crates/medusa-agent/tests/effective_request_manifest.rs')
s = p.read_text()
s += r'''

#[test]
fn provider_credentials_and_endpoint_are_absent_from_request_authority() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let secret = "sk-issue890-never-persist-this";
    let endpoint = "https://signed.example.invalid/path?token=never-persist";
    let mut config = Config::default();
    config.agent.mode = Mode::ReadOnly;
    config.model.auth = secret.to_owned();
    config.model.base_url = Some(endpoint.to_owned());
    let engine = AgentEngine::new(
        CountingProvider {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        config,
    );
    let mut session = engine
        .create_session(directory.path(), "inspect the repository".to_owned())
        .expect("create session");
    engine.step(&mut session).expect("model step");
    let (_, manifest_ref) = request_manifests(&session)[0].clone();
    let audit = inspect_effective_model_request(
        directory.path(),
        session.id.as_str(),
        &manifest_ref,
    )
    .expect("inspect request");
    let request_path = request_artifact_path(
        directory.path(),
        &session,
        audit["request_content_ref"].as_str().expect("content ref"),
    );
    let manifest_hash = manifest_ref
        .strip_prefix("model-request-manifest:sha256:")
        .expect("manifest reference");
    let manifest_path = directory
        .path()
        .join(".medusa/request-manifests")
        .join(session.id.as_str())
        .join(format!("{manifest_hash}.json"));
    let authority = format!(
        "{}\n{}",
        fs::read_to_string(request_path).expect("request artifact"),
        fs::read_to_string(manifest_path).expect("manifest artifact")
    );
    assert!(!authority.contains(secret));
    assert!(!authority.contains(endpoint));
    assert!(!authority.contains("token=never-persist"));
}
'''
p.write_text(s)
