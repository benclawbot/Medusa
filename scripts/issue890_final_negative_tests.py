from pathlib import Path
p = Path('crates/medusa-agent/tests/effective_request_manifest.rs')
s = p.read_text()
s = s.replace('strip_prefix("model-request-manifest:sha256:")', 'strip_prefix("request-manifest:sha256:")')
s += r'''

#[test]
fn immutable_manifest_persistence_conflict_prevents_provider_invocation() {
    let directory = tempfile::tempdir().expect("temporary repository");
    let mut config = Config::default();
    config.agent.mode = Mode::ReadOnly;

    let baseline_engine = AgentEngine::new(
        CountingProvider {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        config.clone(),
    );
    let mut baseline_session = baseline_engine
        .create_session(directory.path(), "inspect the repository".to_owned())
        .expect("baseline session");
    baseline_engine
        .step(&mut baseline_session)
        .expect("baseline model step");
    let baseline_ref = request_manifests(&baseline_session)[0].1.clone();
    let baseline_audit = inspect_effective_model_request(
        directory.path(),
        baseline_session.id.as_str(),
        &baseline_ref,
    )
    .expect("baseline audit");
    let request_hash = baseline_audit["request_content_ref"]
        .as_str()
        .and_then(|reference| reference.strip_prefix("request-content:sha256:"))
        .expect("request content hash")
        .to_owned();

    let blocked_calls = Arc::new(AtomicUsize::new(0));
    let blocked_engine = AgentEngine::new(
        CountingProvider {
            calls: Arc::clone(&blocked_calls),
        },
        config,
    );
    let mut blocked_session = blocked_engine
        .create_session(directory.path(), "inspect the repository".to_owned())
        .expect("blocked session");
    let conflicting = directory
        .path()
        .join(".medusa/request-artifacts")
        .join(blocked_session.id.as_str())
        .join(format!("{request_hash}.json"));
    fs::create_dir_all(conflicting.parent().expect("artifact parent"))
        .expect("create artifact parent");
    fs::write(&conflicting, b"conflicting immutable bytes")
        .expect("seed immutable conflict");

    let error = blocked_engine
        .step(&mut blocked_session)
        .expect_err("manifest persistence conflict must block the call");
    assert!(error.to_string().contains("immutable artifact conflict"));
    assert_eq!(
        blocked_calls.load(Ordering::SeqCst),
        0,
        "provider must not be invoked when request authority cannot persist"
    );
    assert!(
        blocked_session.events.iter().all(|event| !matches!(
            event.payload,
            EventPayload::ModelRequestStarted { .. }
        )),
        "request start must not be journaled after a persistence failure"
    );
}
'''
p.write_text(s)
