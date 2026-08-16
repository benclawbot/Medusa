from pathlib import Path

path = Path("crates/medusa-agent/src/agent_scope.rs")
text = path.read_text()

anchor = '''#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentScopeStopReceipt {
    pub scope: AgentScopeRef,
    pub cause: String,
    pub stopped_at_unix_ms: i64,
}
'''
insert = anchor + '''
#[derive(Clone, Debug)]
pub struct AgentScopePreparation {
    pub mode: Mode,
    pub provider_profile: Value,
    pub execution_policy: Value,
    pub effective_tools: Vec<String>,
    pub team_id: Option<String>,
    pub member_id: Option<String>,
    pub analysis_workspace: bool,
}
'''
if anchor not in text:
    raise SystemExit("agent scope preparation type anchor missing")
text = text.replace(anchor, insert, 1)

old = '''pub fn prepare_agent_scope(
    repo: &Path,
    session_id: &SessionId,
    mode: Mode,
    provider_profile: Value,
    execution_policy: Value,
    effective_tools: Vec<String>,
    team_id: Option<String>,
    member_id: Option<String>,
    analysis_workspace: bool,
) -> MedusaResult<AgentScopeContract> {
    let repository_identity = repository_identity(repo)?;'''
new = '''pub fn prepare_agent_scope(
    repo: &Path,
    session_id: &SessionId,
    preparation: AgentScopePreparation,
) -> MedusaResult<AgentScopeContract> {
    let AgentScopePreparation {
        mode,
        provider_profile,
        execution_policy,
        effective_tools,
        team_id,
        member_id,
        analysis_workspace,
    } = preparation;
    let repository_identity = repository_identity(repo)?;'''
if old not in text:
    raise SystemExit("prepare_agent_scope signature anchor missing")
text = text.replace(old, new, 1)

old = '''            Mode::ReadOnly,
            provider.clone(),
            execution.clone(),
            vec!["fs_read".into()],
            None,
            None,
            false,
        )'''
new = '''            AgentScopePreparation {
                mode: Mode::ReadOnly,
                provider_profile: provider.clone(),
                execution_policy: execution.clone(),
                effective_tools: vec!["fs_read".into()],
                team_id: None,
                member_id: None,
                analysis_workspace: false,
            },
        )'''
count = text.count(old)
if count != 2:
    raise SystemExit(f"expected two scope preparation test calls, found {count}")
text = text.replace(old, new)
path.write_text(text)

# Re-export the typed preparation boundary.
path = Path("crates/medusa-agent/src/lib.rs")
text = path.read_text()
old = '''    AGENT_SCOPE_SCHEMA_VERSION, AgentScopeContract, AgentScopeLifecycle, AgentScopeRef,
    AgentScopeStopReceipt, load_published_scope_ref, prepare_agent_scope, publish_agent_scope,'''
new = '''    AGENT_SCOPE_SCHEMA_VERSION, AgentScopeContract, AgentScopeLifecycle, AgentScopePreparation,
    AgentScopeRef, AgentScopeStopReceipt, load_published_scope_ref, prepare_agent_scope,
    publish_agent_scope,'''
if old not in text:
    raise SystemExit("agent scope re-export anchor missing")
path.write_text(text.replace(old, new, 1))

# Engine startup now passes one transactional preparation object.
path = Path("crates/medusa-agent/src/engine.rs")
text = path.read_text()
old = '''        AgentScopeRef, prepare_agent_scope, publish_agent_scope, resume_agent_scope,
        stop_agent_scope, validate_agent_scope,'''
new = '''        AgentScopePreparation, AgentScopeRef, prepare_agent_scope, publish_agent_scope,
        resume_agent_scope, stop_agent_scope, validate_agent_scope,'''
if old not in text:
    raise SystemExit("engine preparation import anchor missing")
text = text.replace(old, new, 1)
old = '''        let scope = prepare_agent_scope(
            repo,
            &id,
            self.config.agent.mode,
            provider_profile.clone(),
            execution_policy.clone(),
            effective_tools.clone(),
            team_id,
            member_id,
            self.analysis_host.is_some(),
        )?;'''
new = '''        let scope = prepare_agent_scope(
            repo,
            &id,
            AgentScopePreparation {
                mode: self.config.agent.mode,
                provider_profile: provider_profile.clone(),
                execution_policy: execution_policy.clone(),
                effective_tools: effective_tools.clone(),
                team_id,
                member_id,
                analysis_workspace: self.analysis_host.is_some(),
            },
        )?;'''
if old not in text:
    raise SystemExit("engine scope preparation call anchor missing")
path.write_text(text.replace(old, new, 1))
