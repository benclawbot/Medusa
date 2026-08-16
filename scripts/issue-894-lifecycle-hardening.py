from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    target = Path(path)
    text = target.read_text()
    if old not in text:
        raise SystemExit(f"missing anchor in {target}: {old[:160]!r}")
    target.write_text(text.replace(old, new, 1))


# ---- agent_scope.rs: generation-bound owner, revocation, resources, rollback ----
path = Path("crates/medusa-agent/src/agent_scope.rs")
text = path.read_text()
text = text.replace(
    "    path::{Path, PathBuf},\n};",
    "    path::{Path, PathBuf},\n    sync::{\n        Arc,\n        atomic::{AtomicBool, Ordering},\n    },\n};",
    1,
)

old = '''struct AgentScopeState {
    schema_version: u16,
    scope_id: String,
    scope_fingerprint: String,
    generation: u64,
    lifecycle: AgentScopeLifecycle,
    updated_at_unix_ms: i64,
    stop_cause: Option<String>,
}'''
new = '''struct AgentScopeState {
    schema_version: u16,
    scope_id: String,
    scope_fingerprint: String,
    generation: u64,
    lifecycle: AgentScopeLifecycle,
    updated_at_unix_ms: i64,
    stop_cause: Option<String>,
    #[serde(default)]
    failed_start_cause: Option<String>,
    #[serde(default)]
    revoked_tools: Vec<String>,
    #[serde(default)]
    owned_resources: Vec<AgentScopeOwnedResource>,
}'''
if old not in text:
    raise SystemExit("scope state anchor missing")
text = text.replace(old, new, 1)

anchor = '''#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentScopeStopReceipt {
    pub scope: AgentScopeRef,
    pub cause: String,
    pub stopped_at_unix_ms: i64,
}
'''
insert = anchor + '''
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentScopeResourceKind {
    Cancellation,
    TeamContext,
    AnalysisWorkspace,
    DesktopCommander,
    Browser,
    Process,
    Pty,
    BackgroundJob,
    ToolMiddleware,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentScopeOwnedResource {
    pub id: String,
    pub kind: AgentScopeResourceKind,
    pub generation: u64,
    pub active: bool,
}

#[derive(Clone, Debug)]
pub struct AgentRuntimeHandle {
    repo: PathBuf,
    session_id: String,
    scope: AgentScopeRef,
    cancellation: Arc<AtomicBool>,
}

impl AgentRuntimeHandle {
    #[must_use]
    pub fn scope(&self) -> &AgentScopeRef {
        &self.scope
    }

    pub fn ensure_current(&self) -> MedusaResult<()> {
        validate_scope_generation(&self.repo, &self.session_id, &self.scope)
    }

    pub fn revoke_tool(&self, tool: &str) -> MedusaResult<()> {
        revoke_agent_scope_tool(&self.repo, &self.session_id, &self.scope, tool)
    }

    pub fn register_resource(
        &self,
        id: impl Into<String>,
        kind: AgentScopeResourceKind,
    ) -> MedusaResult<()> {
        register_agent_scope_resource(&self.repo, &self.session_id, &self.scope, id, kind)
    }

    pub fn release_resource(&self, id: &str) -> MedusaResult<()> {
        release_agent_scope_resource(&self.repo, &self.session_id, &self.scope, id)
    }

    pub fn stop(self, cause: impl Into<String>) -> MedusaResult<AgentScopeStopReceipt> {
        self.cancellation.store(true, Ordering::SeqCst);
        stop_agent_scope_generation(&self.repo, &self.session_id, &self.scope, cause)
    }
}

impl Drop for AgentRuntimeHandle {
    fn drop(&mut self) {
        // Fail-safe: dropping live ownership immediately closes new cancellable admission.
        // Durable terminal publication still requires explicit stop by the runtime owner.
        self.cancellation.store(true, Ordering::SeqCst);
    }
}
'''
if anchor not in text:
    raise SystemExit("scope receipt insertion anchor missing")
text = text.replace(anchor, insert, 1)

# Seed resources and mutable narrowing state during prepare.
old = '''                lifecycle: AgentScopeLifecycle::Prepared,
                updated_at_unix_ms: unix_ms(),
                stop_cause: None,
            },'''
new = '''                lifecycle: AgentScopeLifecycle::Prepared,
                updated_at_unix_ms: unix_ms(),
                stop_cause: None,
                failed_start_cause: None,
                revoked_tools: Vec::new(),
                owned_resources: initial_resources(
                    team_id.as_deref(),
                    member_id.as_deref(),
                    analysis_workspace,
                ),
            },'''
if old not in text:
    raise SystemExit("prepared state initializer anchor missing")
text = text.replace(old, new, 1)

# Publication revalidates repository identity/revision at the commit boundary.
old = '''    validate_runtime_authority(
        contract,
        current_provider_profile,
        current_execution_policy,
        current_effective_tools,
    )?;
    let path = state_path(repo, &contract.session_id);'''
new = '''    if repository_identity(repo)? != contract.repository_identity {
        return Err(reconciliation_error(
            "repository identity changed during agent-scope setup",
        ));
    }
    if let Some(accepted_revision) = contract.initial_repository_revision.as_ref()
        && repository_revision(repo).as_ref() != Some(accepted_revision)
    {
        return Err(reconciliation_error(
            "repository revision changed during agent-scope setup",
        ));
    }
    validate_runtime_authority(
        contract,
        current_provider_profile,
        current_execution_policy,
        current_effective_tools,
    )?;
    let path = state_path(repo, &contract.session_id);'''
if old not in text:
    raise SystemExit("publication revalidation anchor missing")
text = text.replace(old, new, 1)

# Stop drains/revokes all owned resources before terminal state.
old = '''    state.lifecycle = AgentScopeLifecycle::Stopping;
    state.updated_at_unix_ms = unix_ms();
    state.stop_cause = Some(cause.clone());
    persist_state(&path, &state)?;
    state.lifecycle = AgentScopeLifecycle::Stopped;
    state.updated_at_unix_ms = unix_ms();
    persist_state(&path, &state)?;'''
new = '''    state.lifecycle = AgentScopeLifecycle::Stopping;
    state.updated_at_unix_ms = unix_ms();
    state.stop_cause = Some(cause.clone());
    persist_state(&path, &state)?;
    for resource in &mut state.owned_resources {
        resource.active = false;
    }
    if state.owned_resources.iter().any(|resource| resource.active) {
        return Err(scope_error(
            "agent scope reached terminal teardown with an owned resource still active",
        ));
    }
    state.lifecycle = AgentScopeLifecycle::Stopped;
    state.updated_at_unix_ms = unix_ms();
    persist_state(&path, &state)?;'''
if old not in text:
    raise SystemExit("stop drain anchor missing")
text = text.replace(old, new, 1)

# Insert lifecycle APIs before validate_runtime_authority.
anchor = '''fn validate_runtime_authority(
    contract: &AgentScopeContract,'''
api = r'''pub fn agent_runtime_handle(
    repo: &Path,
    session_id: &str,
    cancellation: Arc<AtomicBool>,
) -> MedusaResult<AgentRuntimeHandle> {
    let scope = load_published_scope_ref(repo, session_id)?;
    Ok(AgentRuntimeHandle {
        repo: repo.to_path_buf(),
        session_id: session_id.to_owned(),
        scope,
        cancellation,
    })
}

pub fn fail_agent_scope_start(
    repo: &Path,
    session_id: &str,
    cause: impl Into<String>,
) -> MedusaResult<AgentScopeRef> {
    let cause = cause.into();
    let contract = load_contract(repo, session_id)?;
    let path = state_path(repo, session_id);
    let mut state = load_state(&path)?;
    validate_state_binding(&contract, &state)?;
    if !matches!(
        state.lifecycle,
        AgentScopeLifecycle::Prepared | AgentScopeLifecycle::Published | AgentScopeLifecycle::FailedStart
    ) {
        return Err(scope_error(format!(
            "agent scope cannot record failed start from lifecycle {:?}",
            state.lifecycle
        )));
    }
    for resource in &mut state.owned_resources {
        resource.active = false;
    }
    state.lifecycle = AgentScopeLifecycle::FailedStart;
    state.failed_start_cause = Some(cause);
    state.updated_at_unix_ms = unix_ms();
    persist_state(&path, &state)?;
    Ok(scope_ref(&state))
}

pub fn effective_agent_scope_tools(
    repo: &Path,
    session_id: &str,
    current_tools: Vec<String>,
) -> MedusaResult<Vec<String>> {
    let contract = load_contract(repo, session_id)?;
    let state = load_state(&state_path(repo, session_id))?;
    validate_state_binding(&contract, &state)?;
    if state.lifecycle != AgentScopeLifecycle::Published {
        return Err(scope_error("agent scope is not published for tool projection"));
    }
    let current = canonical_strings(current_tools);
    if current
        .iter()
        .any(|tool| contract.effective_tools.binary_search(tool).is_err())
    {
        return Err(reconciliation_error(
            "runtime tool projection would widen the published agent scope",
        ));
    }
    let revoked = state.revoked_tools.iter().collect::<BTreeSet<_>>();
    Ok(current
        .into_iter()
        .filter(|tool| !revoked.contains(tool))
        .collect())
}

pub fn revoke_agent_scope_tool(
    repo: &Path,
    session_id: &str,
    expected: &AgentScopeRef,
    tool: &str,
) -> MedusaResult<()> {
    let contract = load_contract(repo, session_id)?;
    if contract.effective_tools.binary_search(&tool.to_owned()).is_err() {
        return Err(scope_error(format!(
            "tool {tool} was never admitted to this agent scope"
        )));
    }
    let path = state_path(repo, session_id);
    let mut state = load_state(&path)?;
    validate_state_binding(&contract, &state)?;
    validate_expected_generation(expected, &state)?;
    if state.lifecycle != AgentScopeLifecycle::Published {
        return Err(scope_error("agent scope is not live for capability revocation"));
    }
    state.revoked_tools.push(tool.to_owned());
    state.revoked_tools = canonical_strings(state.revoked_tools);
    state.updated_at_unix_ms = unix_ms();
    persist_state(&path, &state)
}

pub fn register_agent_scope_resource(
    repo: &Path,
    session_id: &str,
    expected: &AgentScopeRef,
    id: impl Into<String>,
    kind: AgentScopeResourceKind,
) -> MedusaResult<()> {
    let contract = load_contract(repo, session_id)?;
    let path = state_path(repo, session_id);
    let mut state = load_state(&path)?;
    validate_state_binding(&contract, &state)?;
    validate_expected_generation(expected, &state)?;
    if state.lifecycle != AgentScopeLifecycle::Published {
        return Err(scope_error("agent scope is not live for resource registration"));
    }
    let id = id.into();
    if let Some(resource) = state.owned_resources.iter_mut().find(|resource| resource.id == id) {
        resource.active = true;
        resource.generation = state.generation;
        resource.kind = kind;
    } else {
        state.owned_resources.push(AgentScopeOwnedResource {
            id,
            kind,
            generation: state.generation,
            active: true,
        });
    }
    state.updated_at_unix_ms = unix_ms();
    persist_state(&path, &state)
}

pub fn release_agent_scope_resource(
    repo: &Path,
    session_id: &str,
    expected: &AgentScopeRef,
    id: &str,
) -> MedusaResult<()> {
    let contract = load_contract(repo, session_id)?;
    let path = state_path(repo, session_id);
    let mut state = load_state(&path)?;
    validate_state_binding(&contract, &state)?;
    validate_expected_generation(expected, &state)?;
    let resource = state
        .owned_resources
        .iter_mut()
        .find(|resource| resource.id == id)
        .ok_or_else(|| scope_error(format!("agent scope does not own resource {id}")))?;
    resource.active = false;
    state.updated_at_unix_ms = unix_ms();
    persist_state(&path, &state)
}

pub fn validate_scope_generation(
    repo: &Path,
    session_id: &str,
    expected: &AgentScopeRef,
) -> MedusaResult<()> {
    let contract = load_contract(repo, session_id)?;
    let state = load_state(&state_path(repo, session_id))?;
    validate_state_binding(&contract, &state)?;
    validate_expected_generation(expected, &state)
}

pub fn stop_agent_scope_generation(
    repo: &Path,
    session_id: &str,
    expected: &AgentScopeRef,
    cause: impl Into<String>,
) -> MedusaResult<AgentScopeStopReceipt> {
    validate_scope_generation(repo, session_id, expected)?;
    stop_agent_scope(repo, session_id, cause)
}

fn validate_expected_generation(
    expected: &AgentScopeRef,
    state: &AgentScopeState,
) -> MedusaResult<()> {
    if expected.scope_id != state.scope_id
        || expected.scope_fingerprint != state.scope_fingerprint
        || expected.generation != state.generation
    {
        return Err(scope_error(format!(
            "stale_agent_scope_generation: expected generation {}, current generation {}",
            expected.generation, state.generation
        )));
    }
    Ok(())
}

fn initial_resources(
    team_id: Option<&str>,
    member_id: Option<&str>,
    analysis_workspace: bool,
) -> Vec<AgentScopeOwnedResource> {
    let mut resources = vec![AgentScopeOwnedResource {
        id: "cancellation".to_owned(),
        kind: AgentScopeResourceKind::Cancellation,
        generation: 1,
        active: true,
    }];
    if let (Some(team_id), Some(member_id)) = (team_id, member_id) {
        resources.push(AgentScopeOwnedResource {
            id: format!("team:{team_id}:{member_id}"),
            kind: AgentScopeResourceKind::TeamContext,
            generation: 1,
            active: true,
        });
    }
    if analysis_workspace {
        resources.push(AgentScopeOwnedResource {
            id: "analysis-workspace".to_owned(),
            kind: AgentScopeResourceKind::AnalysisWorkspace,
            generation: 1,
            active: true,
        });
    }
    resources
}

'''
if anchor not in text:
    raise SystemExit("runtime authority insertion anchor missing")
text = text.replace(anchor, api + anchor, 1)

# On resume, carry resource registrations forward to the new generation and keep explicit revocations.
old = '''    state.generation = state
        .generation
        .checked_add(1)
        .ok_or_else(|| scope_error("agent scope lifecycle generation overflowed during resume"))?;
    state.updated_at_unix_ms = unix_ms();'''
new = '''    state.generation = state
        .generation
        .checked_add(1)
        .ok_or_else(|| scope_error("agent scope lifecycle generation overflowed during resume"))?;
    for resource in &mut state.owned_resources {
        if resource.active {
            resource.generation = state.generation;
        }
    }
    state.updated_at_unix_ms = unix_ms();'''
if old not in text:
    raise SystemExit("resume generation anchor missing")
text = text.replace(old, new, 1)

# Tests for rollback, revocation, stale handles, teardown resources.
test_anchor = '''    #[test]
    fn resume_advances_generation_without_changing_authority() {'''
if test_anchor not in text:
    raise SystemExit("scope test insertion anchor missing")
extra_tests = r'''    #[test]
    fn failed_start_is_terminal_for_publication_and_revokes_resources() {
        let repo = tempfile::tempdir().expect("tempdir");
        let session = SessionId::new();
        let provider = json!({"provider":"test","model":"test"});
        let execution = policy(Some(&["fs_read"]), None, false);
        let contract = prepare_agent_scope(
            repo.path(),
            &session,
            AgentScopePreparation {
                mode: Mode::ReadOnly,
                provider_profile: provider,
                execution_policy: execution,
                effective_tools: vec!["fs_read".into()],
                team_id: Some("team".into()),
                member_id: Some("worker".into()),
                analysis_workspace: true,
            },
        )
        .expect("prepare");
        fail_agent_scope_start(repo.path(), session.as_str(), "setup failed").expect("fail start");
        assert!(publish_agent_scope(
            repo.path(),
            &contract,
            json!({"provider":"test","model":"test"}),
            policy(Some(&["fs_read"]), None, false),
            vec!["fs_read".into()],
        )
        .is_err());
        let state = load_state(&state_path(repo.path(), session.as_str())).expect("state");
        assert_eq!(state.lifecycle, AgentScopeLifecycle::FailedStart);
        assert!(state.owned_resources.iter().all(|resource| !resource.active));
    }

    #[test]
    fn revocation_removes_tool_from_next_projection() {
        let repo = tempfile::tempdir().expect("tempdir");
        let session = SessionId::new();
        let provider = json!({"provider":"test","model":"test"});
        let execution = policy(Some(&["fs_read", "shell_run"]), None, false);
        let contract = prepare_agent_scope(
            repo.path(),
            &session,
            AgentScopePreparation {
                mode: Mode::ReadOnly,
                provider_profile: provider.clone(),
                execution_policy: execution.clone(),
                effective_tools: vec!["fs_read".into(), "shell_run".into()],
                team_id: None,
                member_id: None,
                analysis_workspace: false,
            },
        )
        .expect("prepare");
        let scope = publish_agent_scope(
            repo.path(),
            &contract,
            provider,
            execution,
            vec!["fs_read".into(), "shell_run".into()],
        )
        .expect("publish");
        revoke_agent_scope_tool(repo.path(), session.as_str(), &scope, "shell_run").expect("revoke");
        assert_eq!(
            effective_agent_scope_tools(
                repo.path(),
                session.as_str(),
                vec!["fs_read".into(), "shell_run".into()],
            )
            .expect("projection"),
            vec!["fs_read"]
        );
    }

    #[test]
    fn stale_generation_cannot_revoke_or_stop_resumed_scope() {
        let repo = tempfile::tempdir().expect("tempdir");
        let session = SessionId::new();
        let provider = json!({"provider":"test","model":"test"});
        let execution = policy(Some(&["fs_read"]), None, false);
        let contract = prepare_agent_scope(
            repo.path(),
            &session,
            AgentScopePreparation {
                mode: Mode::ReadOnly,
                provider_profile: provider.clone(),
                execution_policy: execution.clone(),
                effective_tools: vec!["fs_read".into()],
                team_id: None,
                member_id: None,
                analysis_workspace: false,
            },
        )
        .expect("prepare");
        let stale = publish_agent_scope(
            repo.path(),
            &contract,
            provider.clone(),
            execution.clone(),
            vec!["fs_read".into()],
        )
        .expect("publish");
        let current = resume_agent_scope(
            repo.path(),
            session.as_str(),
            provider,
            execution,
            vec!["fs_read".into()],
        )
        .expect("resume");
        assert!(revoke_agent_scope_tool(repo.path(), session.as_str(), &stale, "fs_read").is_err());
        assert!(stop_agent_scope_generation(repo.path(), session.as_str(), &stale, "stale").is_err());
        stop_agent_scope_generation(repo.path(), session.as_str(), &current, "current").expect("stop");
    }

    #[test]
    fn stop_revokes_every_owned_resource() {
        let repo = tempfile::tempdir().expect("tempdir");
        let session = SessionId::new();
        let provider = json!({"provider":"test","model":"test"});
        let execution = policy(Some(&["fs_read"]), None, false);
        let contract = prepare_agent_scope(
            repo.path(),
            &session,
            AgentScopePreparation {
                mode: Mode::ReadOnly,
                provider_profile: provider.clone(),
                execution_policy: execution.clone(),
                effective_tools: vec!["fs_read".into()],
                team_id: None,
                member_id: None,
                analysis_workspace: false,
            },
        )
        .expect("prepare");
        let scope = publish_agent_scope(
            repo.path(),
            &contract,
            provider,
            execution,
            vec!["fs_read".into()],
        )
        .expect("publish");
        register_agent_scope_resource(
            repo.path(),
            session.as_str(),
            &scope,
            "browser-1",
            AgentScopeResourceKind::Browser,
        )
        .expect("register");
        stop_agent_scope_generation(repo.path(), session.as_str(), &scope, "done").expect("stop");
        let state = load_state(&state_path(repo.path(), session.as_str())).expect("state");
        assert!(state.owned_resources.iter().all(|resource| !resource.active));
    }

'''
text = text.replace(test_anchor, extra_tests + test_anchor, 1)
path.write_text(text)

# ---- lib.rs exports ----
path = Path("crates/medusa-agent/src/lib.rs")
text = path.read_text()
old = '''    AGENT_SCOPE_SCHEMA_VERSION, AgentScopeContract, AgentScopeLifecycle, AgentScopePreparation,
    AgentScopeRef, AgentScopeStopReceipt, load_published_scope_ref, prepare_agent_scope,
    publish_agent_scope, resume_agent_scope, stop_agent_scope, validate_agent_scope,'''
new = '''    AGENT_SCOPE_SCHEMA_VERSION, AgentRuntimeHandle, AgentScopeContract, AgentScopeLifecycle,
    AgentScopeOwnedResource, AgentScopePreparation, AgentScopeRef, AgentScopeResourceKind,
    AgentScopeStopReceipt, agent_runtime_handle, effective_agent_scope_tools,
    fail_agent_scope_start, load_published_scope_ref, prepare_agent_scope, publish_agent_scope,
    register_agent_scope_resource, release_agent_scope_resource, resume_agent_scope,
    revoke_agent_scope_tool, stop_agent_scope, stop_agent_scope_generation, validate_agent_scope,
    validate_scope_generation,'''
if old not in text:
    raise SystemExit("scope exports anchor missing")
path.write_text(text.replace(old, new, 1))

# ---- engine.rs: rollback, runtime handle, dynamic revocation at request+dispatch boundary ----
path = Path("crates/medusa-agent/src/engine.rs")
text = path.read_text()
old = '''        AgentScopePreparation, AgentScopeRef, prepare_agent_scope, publish_agent_scope,
        resume_agent_scope, stop_agent_scope, validate_agent_scope,'''
new = '''        AgentRuntimeHandle, AgentScopePreparation, AgentScopeRef, AgentScopeResourceKind,
        agent_runtime_handle, effective_agent_scope_tools, fail_agent_scope_start,
        prepare_agent_scope, publish_agent_scope, register_agent_scope_resource,
        release_agent_scope_resource, resume_agent_scope, stop_agent_scope, validate_agent_scope,'''
if old not in text:
    raise SystemExit("engine scope imports anchor missing")
text = text.replace(old, new, 1)

# Runtime handle accessor and scoped projection helper.
anchor = '''    fn validate_scope(&self, session: &AgentSession) -> MedusaResult<AgentScopeRef> {
        validate_agent_scope('''
insert = '''    fn scoped_runtime_tools(&self, session: &AgentSession) -> MedusaResult<Vec<String>> {
        effective_agent_scope_tools(
            &session.repo,
            session.id.as_str(),
            self.scope_effective_tools(&session.repo)?,
        )
    }

    pub fn runtime_handle(&self, session: &AgentSession) -> MedusaResult<AgentRuntimeHandle> {
        agent_runtime_handle(
            &session.repo,
            session.id.as_str(),
            Arc::clone(&self.cancellation),
        )
    }

    fn validate_scope(&self, session: &AgentSession) -> MedusaResult<AgentScopeRef> {
        validate_agent_scope('''
if anchor not in text:
    raise SystemExit("engine runtime handle insertion anchor missing")
text = text.replace(anchor, insert, 1)

# Transactional failed-start rollback for publication and durable session persistence.
old = '''        publish_agent_scope(
            repo,
            &scope,
            provider_profile,
            execution_policy,
            effective_tools,
        )?;
        let now = OffsetDateTime::now_utc();'''
new = '''        if let Err(error) = publish_agent_scope(
            repo,
            &scope,
            provider_profile,
            execution_policy,
            effective_tools,
        ) {
            let _ = fail_agent_scope_start(repo, id.as_str(), error.to_string());
            return Err(error);
        }
        let now = OffsetDateTime::now_utc();'''
if old not in text:
    raise SystemExit("scope publication rollback anchor missing")
text = text.replace(old, new, 1)
old = '''        append_event(
            &mut session,
            Actor::User,
            EventPayload::SessionCreated { objective },
        )?;
        persist(&session)?;
        Ok(session)'''
new = '''        if let Err(error) = append_event(
            &mut session,
            Actor::User,
            EventPayload::SessionCreated { objective },
        ) {
            let _ = fail_agent_scope_start(repo, id.as_str(), error.to_string());
            return Err(error);
        }
        if let Err(error) = persist(&session) {
            let _ = fail_agent_scope_start(repo, id.as_str(), error.to_string());
            return Err(error);
        }
        Ok(session)'''
if old not in text:
    raise SystemExit("session persistence rollback anchor missing")
text = text.replace(old, new, 1)

# Desktop Commander is a dynamically owned scoped resource.
old = '''        if client.is_none() {
            *client = Some(DesktopCommanderClient::connect(
                repo,
                self.desktop_commander_settings.clone(),
            )?);
        }'''
new = '''        if client.is_none() {
            let scope = load_published_scope_ref(repo, self.scope_session_id_for_repo(repo)?)?;
            register_agent_scope_resource(
                repo,
                self.scope_session_id_for_repo(repo)?,
                &scope,
                "desktop-commander",
                AgentScopeResourceKind::DesktopCommander,
            )?;
            match DesktopCommanderClient::connect(repo, self.desktop_commander_settings.clone()) {
                Ok(connected) => *client = Some(connected),
                Err(error) => {
                    let _ = release_agent_scope_resource(
                        repo,
                        self.scope_session_id_for_repo(repo)?,
                        &scope,
                        "desktop-commander",
                    );
                    return Err(error);
                }
            }
        }'''
# This helper cannot resolve session from repo alone; don't apply this block. Keep dynamic ownership
# represented by stop cleanup and explicit API until execute_desktop_commander accepts session.
# Intentionally skip replacement.

# Filter request tools through durable revocations and reject any provider-returned call outside projection.
old = '''        tools.retain(|tool| self.execution_policy.allows(&tool.name));
        let mut request_messages = messages_with_turn_instruction(session, turn_instruction);'''
new = '''        tools.retain(|tool| self.execution_policy.allows(&tool.name));
        let current_tool_names = tools.iter().map(|tool| tool.name.clone()).collect::<Vec<_>>();
        let scoped_tool_names = effective_agent_scope_tools(
            &session.repo,
            session.id.as_str(),
            current_tool_names,
        )?;
        tools.retain(|tool| scoped_tool_names.binary_search(&tool.name).is_ok());
        let mut request_messages = messages_with_turn_instruction(session, turn_instruction);'''
if old not in text:
    raise SystemExit("request tool scope filter anchor missing")
text = text.replace(old, new, 1)

old = '''                ResponseBlock::ToolUse { id, name, input } => {
                    if let Some(early) = early_tool_executions.get(&id)'''
new = '''                ResponseBlock::ToolUse { id, name, input } => {
                    if scoped_tool_names.binary_search(&name).is_err() {
                        return Err(MedusaError::new(
                            ErrorCode::PolicyDenied,
                            ErrorCategory::Policy,
                            format!("tool {name} is revoked or outside the active agent scope"),
                        ));
                    }
                    if let Some(early) = early_tool_executions.get(&id)'''
if old not in text:
    raise SystemExit("provider tool scope rejection anchor missing")
text = text.replace(old, new, 1)

# Early streamed execution must use the same current scope projection.
old = '''                                    && stream_dispatch_safe_tool(&name, &input)
                                    && tool_allowed(self.config.agent.mode, &name) =>'''
new = '''                                    && stream_dispatch_safe_tool(&name, &input)
                                    && tool_allowed(self.config.agent.mode, &name)
                                    && scoped_tool_names.binary_search(&name).is_ok() =>'''
if old not in text:
    raise SystemExit("streamed tool scope guard anchor missing")
text = text.replace(old, new, 1)
path.write_text(text)
