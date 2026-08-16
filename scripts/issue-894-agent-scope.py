from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    target = Path(path)
    text = target.read_text()
    if old not in text:
        raise SystemExit(f"missing anchor in {target}: {old[:140]!r}")
    target.write_text(text.replace(old, new, 1))


# Public surface.
replace_once(
    "crates/medusa-agent/src/lib.rs",
    "pub mod analysis_host;\nmod approval;",
    "pub mod agent_scope;\npub mod analysis_host;\nmod approval;",
)
replace_once(
    "crates/medusa-agent/src/lib.rs",
    "pub use approval::{",
    "pub use agent_scope::{\n"
    "    AGENT_SCOPE_SCHEMA_VERSION, AgentScopeContract, AgentScopeLifecycle, AgentScopeRef,\n"
    "    AgentScopeStopReceipt, load_published_scope_ref, prepare_agent_scope, publish_agent_scope,\n"
    "    resume_agent_scope, stop_agent_scope, validate_agent_scope,\n"
    "};\n"
    "pub use approval::{",
)

# Team relationship identity becomes available to scope composition without exposing mutable state.
replace_once(
    "crates/medusa-agent/src/team.rs",
    "impl TeamMemberContext {\n    #[must_use]\n    pub fn definitions",
    "impl TeamMemberContext {\n"
    "    #[must_use]\n"
    "    pub fn member_id(&self) -> &str {\n"
    "        &self.member_id\n"
    "    }\n\n"
    "    pub fn team_id(&self) -> Result<String, String> {\n"
    "        self.team.team_id()\n"
    "    }\n\n"
    "    #[must_use]\n"
    "    pub fn definitions",
)

# Engine: scope creation is completed before SessionCreated is persisted; every execution boundary
# revalidates the durable scope against current readiness and the current (possibly narrower) policy.
path = Path("crates/medusa-agent/src/engine.rs")
text = path.read_text()
old = """use crate::{
    analysis_host::{ANALYSIS_WORKSPACE_TOOL, AnalysisWorkspaceHost},"""
new = """use crate::{
    agent_scope::{
        AgentScopeRef, prepare_agent_scope, publish_agent_scope, resume_agent_scope,
        stop_agent_scope, validate_agent_scope,
    },
    analysis_host::{ANALYSIS_WORKSPACE_TOOL, AnalysisWorkspaceHost},"""
if old not in text:
    raise SystemExit("engine import anchor missing")
text = text.replace(old, new, 1)

anchor = """    #[must_use]
    pub fn with_analysis_workspace_host(mut self, host: Arc<dyn AnalysisWorkspaceHost>) -> Self {
        self.analysis_host = Some(host);
        self
    }

    fn execute_desktop_commander("""
insert = """    #[must_use]
    pub fn with_analysis_workspace_host(mut self, host: Arc<dyn AnalysisWorkspaceHost>) -> Self {
        self.analysis_host = Some(host);
        self
    }

    fn scope_provider_profile(&self) -> MedusaResult<serde_json::Value> {
        serde_json::to_value(&self.config.model).map_err(json_error)
    }

    fn scope_effective_tools(&self, repo: &Path) -> MedusaResult<Vec<String>> {
        let mut tools = available_tools(
            self.config.agent.mode,
            repo,
            &self.desktop_commander_settings,
        )?;
        if let Some(team) = &self.team_context {
            tools.extend(team.definitions());
        }
        if self.analysis_host.is_some() {
            tools.push(crate::analysis_host::tool_definition());
        }
        tools.retain(|tool| self.execution_policy.allows(&tool.name));
        let mut names = tools.into_iter().map(|tool| tool.name).collect::<Vec<_>>();
        names.sort();
        names.dedup();
        Ok(names)
    }

    fn scope_team_identity(&self) -> MedusaResult<(Option<String>, Option<String>)> {
        let Some(team) = &self.team_context else {
            return Ok((None, None));
        };
        Ok((
            Some(team.team_id().map_err(|error| {
                MedusaError::new(ErrorCode::InternalInvariant, ErrorCategory::Internal, error)
            })?),
            Some(team.member_id().to_owned()),
        ))
    }

    fn validate_scope(&self, session: &AgentSession) -> MedusaResult<AgentScopeRef> {
        validate_agent_scope(
            &session.repo,
            session.id.as_str(),
            self.scope_provider_profile()?,
            self.execution_policy.audit_projection(),
            self.scope_effective_tools(&session.repo)?,
        )
    }

    pub fn stop_session_scope(
        &self,
        session: &AgentSession,
        cause: impl Into<String>,
    ) -> MedusaResult<crate::agent_scope::AgentScopeStopReceipt> {
        self.cancellation
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Ok(mut client) = self.desktop_commander.lock() {
            client.take();
        }
        stop_agent_scope(&session.repo, session.id.as_str(), cause)
    }

    fn execute_desktop_commander("""
if anchor not in text:
    raise SystemExit("engine scope helper insertion anchor missing")
text = text.replace(anchor, insert, 1)

old = """        bootstrap(repo)?;
        medusa_intelligence::recover_patch_transactions(repo)?;
        let now = OffsetDateTime::now_utc();
        let world_model = create_for_session(repo, id.as_str(), objective.clone()).ok();"""
new = """        bootstrap(repo)?;
        medusa_intelligence::recover_patch_transactions(repo)?;
        let effective_tools = self.scope_effective_tools(repo)?;
        let provider_profile = self.scope_provider_profile()?;
        let execution_policy = self.execution_policy.audit_projection();
        let (team_id, member_id) = self.scope_team_identity()?;
        let scope = prepare_agent_scope(
            repo,
            &id,
            self.config.agent.mode,
            provider_profile.clone(),
            execution_policy.clone(),
            effective_tools.clone(),
            team_id,
            member_id,
            self.analysis_host.is_some(),
        )?;
        publish_agent_scope(
            repo,
            &scope,
            provider_profile,
            execution_policy,
            effective_tools,
        )?;
        let now = OffsetDateTime::now_utc();
        let world_model = create_for_session(repo, id.as_str(), objective.clone()).ok();"""
if old not in text:
    raise SystemExit("session scope publication anchor missing")
text = text.replace(old, new, 1)

old = """    pub fn load_session(&self, repo: &Path, session: &str) -> MedusaResult<AgentSession> {
        medusa_intelligence::recover_patch_transactions(repo)?;
        load(repo, session)
    }"""
new = """    pub fn load_session(&self, repo: &Path, session: &str) -> MedusaResult<AgentSession> {
        medusa_intelligence::recover_patch_transactions(repo)?;
        let loaded = load(repo, session)?;
        resume_agent_scope(
            repo,
            loaded.id.as_str(),
            self.scope_provider_profile()?,
            self.execution_policy.audit_projection(),
            self.scope_effective_tools(repo)?,
        )?;
        Ok(loaded)
    }"""
if old not in text:
    raise SystemExit("session scope resume anchor missing")
text = text.replace(old, new, 1)

# User/session mutations that can later trigger execution require a valid published scope.
for signature in [
    "    pub fn append_user_message(\n",
    "    pub fn append_queued_user_message(\n",
    "    pub fn answer_pending_question(\n",
]:
    pos = text.find(signature)
    if pos < 0:
        raise SystemExit(f"engine method missing: {signature}")
    brace = text.find("    ) -> MedusaResult<()> {", pos)
    if brace < 0:
        raise SystemExit(f"engine method return anchor missing: {signature}")
    insert_at = brace + len("    ) -> MedusaResult<()> {")
    text = text[:insert_at] + "\n        self.validate_scope(session)?;" + text[insert_at:]

old = """    {
        if session.completed {
            return Ok(StepOutcome::Completed);
        }"""
new = """    {
        self.validate_scope(session)?;
        if session.completed {
            return Ok(StepOutcome::Completed);
        }"""
# Anchor occurs at the principal step boundary after the generic where clause.
idx = text.find(old, text.find("pub fn step_with_observer_and_context_and_turn_instruction_for_phase"))
if idx < 0:
    raise SystemExit("step scope validation anchor missing")
text = text[:idx] + text[idx:].replace(old, new, 1)

# Durable tool receipts prove the same scope identity as request manifests.
old = """fn journal_certified_tool_execution(
    session: &mut AgentSession,
    tool_use_id: &str,
    name: &str,
    input: &serde_json::Value,
    receipt: serde_json::Value,
    execution_policy: &AgentExecutionPolicy,
) -> MedusaResult<()> {
    append_event("""
new = """fn journal_certified_tool_execution(
    session: &mut AgentSession,
    tool_use_id: &str,
    name: &str,
    input: &serde_json::Value,
    receipt: serde_json::Value,
    execution_policy: &AgentExecutionPolicy,
) -> MedusaResult<()> {
    let scope = crate::agent_scope::load_published_scope_ref(&session.repo, session.id.as_str())?;
    append_event("""
if old not in text:
    raise SystemExit("tool receipt scope anchor missing")
text = text.replace(old, new, 1)
old = '''                "execution_authority": execution_policy.audit_projection(),
            }),'''
new = '''                "execution_authority": execution_policy.audit_projection(),
                "agent_scope_id": scope.scope_id,
                "agent_scope_fingerprint": scope.scope_fingerprint,
                "agent_scope_generation": scope.generation,
            }),'''
if old not in text:
    raise SystemExit("tool receipt evidence projection anchor missing")
text = text.replace(old, new, 1)
path.write_text(text)

# Request manifests bind the exact scope generation in addition to the execution policy.
path = Path("crates/medusa-agent/src/engine/effective_request.rs")
text = path.read_text()
text = text.replace(
    "    session_id: String,\n    started_event_sequence: u64,",
    "    session_id: String,\n    agent_scope_id: String,\n    agent_scope_fingerprint: String,\n    agent_scope_generation: u64,\n    started_event_sequence: u64,",
    1,
)
text = text.replace(
    "    execution_policy_fingerprint: &'a str,\n    assembly_provenance:",
    "    execution_policy_fingerprint: &'a str,\n    agent_scope_id: &'a str,\n    agent_scope_fingerprint: &'a str,\n    agent_scope_generation: u64,\n    assembly_provenance:",
    1,
)
text = text.replace(
    "    session_id: &'a str,\n    started_event_sequence:",
    "    session_id: &'a str,\n    agent_scope_id: &'a str,\n    agent_scope_fingerprint: &'a str,\n    agent_scope_generation: u64,\n    started_event_sequence:",
    1,
)
old = """    let preceding_event_sequence = session.events.last().map_or(0, |event| event.sequence);
    let started_event_sequence = preceding_event_sequence.saturating_add(1);"""
new = """    let scope = crate::agent_scope::load_published_scope_ref(&session.repo, session.id.as_str())?;
    let preceding_event_sequence = session.events.last().map_or(0, |event| event.sequence);
    let started_event_sequence = preceding_event_sequence.saturating_add(1);"""
if old not in text:
    raise SystemExit("request manifest scope load anchor missing")
text = text.replace(old, new, 1)
old = """        execution_policy_fingerprint: &execution_policy_fingerprint,
        assembly_provenance: &assembly_provenance,"""
new = """        execution_policy_fingerprint: &execution_policy_fingerprint,
        agent_scope_id: &scope.scope_id,
        agent_scope_fingerprint: &scope.scope_fingerprint,
        agent_scope_generation: scope.generation,
        assembly_provenance: &assembly_provenance,"""
if old not in text:
    raise SystemExit("request fingerprint scope anchor missing")
text = text.replace(old, new, 1)
old = """        session_id: session.id.as_str(),
        started_event_sequence,"""
new = """        session_id: session.id.as_str(),
        agent_scope_id: &scope.scope_id,
        agent_scope_fingerprint: &scope.scope_fingerprint,
        agent_scope_generation: scope.generation,
        started_event_sequence,"""
if old not in text:
    raise SystemExit("manifest fingerprint scope anchor missing")
text = text.replace(old, new, 1)
old = """        session_id: session.id.to_string(),
        started_event_sequence,"""
new = """        session_id: session.id.to_string(),
        agent_scope_id: scope.scope_id,
        agent_scope_fingerprint: scope.scope_fingerprint,
        agent_scope_generation: scope.generation,
        started_event_sequence,"""
if old not in text:
    raise SystemExit("manifest body scope anchor missing")
text = text.replace(old, new, 1)
path.write_text(text)
