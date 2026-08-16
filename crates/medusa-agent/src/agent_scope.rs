//! Durable transactional authority and lifecycle for one live agent session.
//!
//! Scope contracts are immutable authority. Lifecycle state is mutable only through explicit
//! publication/resume/stop transitions. A model request or executable tool call must validate
//! the published scope first, so ambient runtime changes can narrow authority but cannot widen it.

use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use medusa_capabilities::CapabilityRegistry;
use medusa_config::Mode;
use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult, SessionId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

pub const AGENT_SCOPE_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentScopeLifecycle {
    Prepared,
    Published,
    Stopping,
    Stopped,
    FailedStart,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentScopeContract {
    pub schema_version: u16,
    pub scope_id: String,
    pub fingerprint: String,
    pub session_id: String,
    pub repository_identity: String,
    pub initial_repository_revision: Option<String>,
    pub mode: Mode,
    pub provider_profile: Value,
    pub execution_policy: Value,
    pub effective_tools: Vec<String>,
    pub capability_registry_fingerprint: String,
    pub team_id: Option<String>,
    pub member_id: Option<String>,
    pub analysis_workspace: bool,
    pub cancellation_owner: String,
    pub created_at_unix_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct AgentScopeState {
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
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentScopeRef {
    pub scope_id: String,
    pub scope_fingerprint: String,
    pub generation: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentScopeStopReceipt {
    pub scope: AgentScopeRef,
    pub cause: String,
    pub stopped_at_unix_ms: i64,
}

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

#[derive(Serialize)]
struct ScopeAuthorityMaterial<'a> {
    schema_version: u16,
    session_id: &'a str,
    repository_identity: &'a str,
    initial_repository_revision: &'a Option<String>,
    mode: Mode,
    provider_profile: &'a Value,
    execution_policy: &'a Value,
    effective_tools: &'a [String],
    capability_registry_fingerprint: &'a str,
    team_id: &'a Option<String>,
    member_id: &'a Option<String>,
    analysis_workspace: bool,
    cancellation_owner: &'a str,
}

pub fn prepare_agent_scope(
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
    let repository_identity = repository_identity(repo)?;
    let initial_repository_revision = repository_revision(repo);
    let provider_profile = canonicalize_value(provider_profile);
    let execution_policy = canonicalize_value(execution_policy);
    let effective_tools = canonical_strings(effective_tools);
    let capability_registry_fingerprint = capability_registry_fingerprint(repo)?;
    let cancellation_owner = format!("agent-session:{}", session_id.as_str());
    let material = ScopeAuthorityMaterial {
        schema_version: AGENT_SCOPE_SCHEMA_VERSION,
        session_id: session_id.as_str(),
        repository_identity: &repository_identity,
        initial_repository_revision: &initial_repository_revision,
        mode,
        provider_profile: &provider_profile,
        execution_policy: &execution_policy,
        effective_tools: &effective_tools,
        capability_registry_fingerprint: &capability_registry_fingerprint,
        team_id: &team_id,
        member_id: &member_id,
        analysis_workspace,
        cancellation_owner: &cancellation_owner,
    };
    let fingerprint = sha256(&serde_json::to_vec(&material).map_err(json_error)?);
    let contract = AgentScopeContract {
        schema_version: AGENT_SCOPE_SCHEMA_VERSION,
        scope_id: format!("agent-scope-v1-{fingerprint}"),
        fingerprint,
        session_id: session_id.to_string(),
        repository_identity,
        initial_repository_revision,
        mode,
        provider_profile,
        execution_policy,
        effective_tools,
        capability_registry_fingerprint,
        team_id,
        member_id,
        analysis_workspace,
        cancellation_owner,
        created_at_unix_ms: unix_ms(),
    };
    validate_contract(&contract)?;
    persist_immutable_json(&contract_path(repo, session_id.as_str()), &contract)?;
    let state_path = state_path(repo, session_id.as_str());
    if !state_path.is_file() {
        persist_state(
            &state_path,
            &AgentScopeState {
                schema_version: AGENT_SCOPE_SCHEMA_VERSION,
                scope_id: contract.scope_id.clone(),
                scope_fingerprint: contract.fingerprint.clone(),
                generation: 1,
                lifecycle: AgentScopeLifecycle::Prepared,
                updated_at_unix_ms: unix_ms(),
                stop_cause: None,
                failed_start_cause: None,
                revoked_tools: Vec::new(),
                owned_resources: initial_resources(
                    contract.team_id.as_deref(),
                    contract.member_id.as_deref(),
                    contract.analysis_workspace,
                ),
            },
        )?;
    }
    Ok(contract)
}

pub fn publish_agent_scope(
    repo: &Path,
    contract: &AgentScopeContract,
    current_provider_profile: Value,
    current_execution_policy: Value,
    current_effective_tools: Vec<String>,
) -> MedusaResult<AgentScopeRef> {
    validate_contract(contract)?;
    let stored = load_contract(repo, &contract.session_id)?;
    if stored != *contract {
        return Err(scope_error(
            "agent scope contract changed before publication",
        ));
    }
    if repository_identity(repo)? != contract.repository_identity {
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
    let path = state_path(repo, &contract.session_id);
    let mut state = load_state(&path)?;
    validate_state_binding(contract, &state)?;
    match state.lifecycle {
        AgentScopeLifecycle::Prepared => {
            state.lifecycle = AgentScopeLifecycle::Published;
            state.updated_at_unix_ms = unix_ms();
            persist_state(&path, &state)?;
        }
        AgentScopeLifecycle::Published => {}
        _ => {
            return Err(scope_error(format!(
                "agent scope cannot be published from lifecycle {:?}",
                state.lifecycle
            )));
        }
    }
    Ok(scope_ref(&state))
}

pub fn validate_agent_scope(
    repo: &Path,
    session_id: &str,
    current_provider_profile: Value,
    current_execution_policy: Value,
    current_effective_tools: Vec<String>,
) -> MedusaResult<AgentScopeRef> {
    let contract = load_contract(repo, session_id)?;
    let state = load_state(&state_path(repo, session_id))?;
    validate_state_binding(&contract, &state)?;
    if state.lifecycle != AgentScopeLifecycle::Published {
        return Err(scope_error(format!(
            "agent scope is not executable in lifecycle {:?}",
            state.lifecycle
        )));
    }
    validate_runtime_authority(
        &contract,
        current_provider_profile,
        current_execution_policy,
        current_effective_tools,
    )?;
    Ok(scope_ref(&state))
}

pub fn resume_agent_scope(
    repo: &Path,
    session_id: &str,
    current_provider_profile: Value,
    current_execution_policy: Value,
    current_effective_tools: Vec<String>,
) -> MedusaResult<AgentScopeRef> {
    let contract = load_contract(repo, session_id)?;
    validate_runtime_authority(
        &contract,
        current_provider_profile,
        current_execution_policy,
        current_effective_tools,
    )?;
    let path = state_path(repo, session_id);
    let mut state = load_state(&path)?;
    validate_state_binding(&contract, &state)?;
    if state.lifecycle != AgentScopeLifecycle::Published {
        return Err(scope_error(format!(
            "agent scope cannot resume from lifecycle {:?}",
            state.lifecycle
        )));
    }
    state.generation = state
        .generation
        .checked_add(1)
        .ok_or_else(|| scope_error("agent scope lifecycle generation overflowed during resume"))?;
    for resource in &mut state.owned_resources {
        if resource.active {
            resource.generation = state.generation;
        }
    }
    state.updated_at_unix_ms = unix_ms();
    persist_state(&path, &state)?;
    Ok(scope_ref(&state))
}

pub fn load_published_scope_ref(repo: &Path, session_id: &str) -> MedusaResult<AgentScopeRef> {
    let contract = load_contract(repo, session_id)?;
    let state = load_state(&state_path(repo, session_id))?;
    validate_state_binding(&contract, &state)?;
    if state.lifecycle != AgentScopeLifecycle::Published {
        return Err(scope_error(format!(
            "agent scope is not published: {:?}",
            state.lifecycle
        )));
    }
    Ok(scope_ref(&state))
}

pub fn stop_agent_scope(
    repo: &Path,
    session_id: &str,
    cause: impl Into<String>,
) -> MedusaResult<AgentScopeStopReceipt> {
    let cause = cause.into();
    let contract = load_contract(repo, session_id)?;
    let path = state_path(repo, session_id);
    let mut state = load_state(&path)?;
    validate_state_binding(&contract, &state)?;
    if state.lifecycle == AgentScopeLifecycle::Stopped {
        return Ok(AgentScopeStopReceipt {
            scope: scope_ref(&state),
            cause: state.stop_cause.unwrap_or(cause),
            stopped_at_unix_ms: state.updated_at_unix_ms,
        });
    }
    if state.lifecycle != AgentScopeLifecycle::Published {
        return Err(scope_error(format!(
            "agent scope cannot stop from lifecycle {:?}",
            state.lifecycle
        )));
    }
    state.lifecycle = AgentScopeLifecycle::Stopping;
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
    persist_state(&path, &state)?;
    Ok(AgentScopeStopReceipt {
        scope: scope_ref(&state),
        cause,
        stopped_at_unix_ms: state.updated_at_unix_ms,
    })
}

pub fn agent_runtime_handle(
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
        AgentScopeLifecycle::Prepared
            | AgentScopeLifecycle::Published
            | AgentScopeLifecycle::FailedStart
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
        return Err(scope_error(
            "agent scope is not published for tool projection",
        ));
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
    if contract
        .effective_tools
        .binary_search(&tool.to_owned())
        .is_err()
    {
        return Err(scope_error(format!(
            "tool {tool} was never admitted to this agent scope"
        )));
    }
    let path = state_path(repo, session_id);
    let mut state = load_state(&path)?;
    validate_state_binding(&contract, &state)?;
    validate_expected_generation(expected, &state)?;
    if state.lifecycle != AgentScopeLifecycle::Published {
        return Err(scope_error(
            "agent scope is not live for capability revocation",
        ));
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
        return Err(scope_error(
            "agent scope is not live for resource registration",
        ));
    }
    let id = id.into();
    if let Some(resource) = state
        .owned_resources
        .iter_mut()
        .find(|resource| resource.id == id)
    {
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

fn validate_runtime_authority(
    contract: &AgentScopeContract,
    current_provider_profile: Value,
    current_execution_policy: Value,
    current_effective_tools: Vec<String>,
) -> MedusaResult<()> {
    let current_provider_profile = canonicalize_value(current_provider_profile);
    if current_provider_profile != contract.provider_profile {
        return Err(reconciliation_error(
            "provider/model profile differs from the published agent scope",
        ));
    }
    let current_execution_policy = canonicalize_value(current_execution_policy);
    if !policy_narrows_or_equals(&contract.execution_policy, &current_execution_policy) {
        return Err(reconciliation_error(
            "execution policy would widen the published agent scope",
        ));
    }
    let current_tools = canonical_strings(current_effective_tools);
    if current_tools
        .iter()
        .any(|tool| contract.effective_tools.binary_search(tool).is_err())
    {
        return Err(reconciliation_error(
            "runtime tool set would widen the published agent scope",
        ));
    }
    Ok(())
}

fn policy_narrows_or_equals(accepted: &Value, current: &Value) -> bool {
    let accepted_tools = string_set(accepted.get("allowed_tools"));
    let current_tools = string_set(current.get("allowed_tools"));
    if !optional_set_narrows(accepted_tools, current_tools) {
        return false;
    }
    let accepted_paths = string_set(accepted.get("allowed_write_paths"));
    let current_paths = string_set(current.get("allowed_write_paths"));
    if !optional_set_narrows(accepted_paths, current_paths) {
        return false;
    }
    let accepted_questions = accepted
        .get("allow_user_questions")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let current_questions = current
        .get("allow_user_questions")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !accepted_questions && current_questions {
        return false;
    }
    for key in ["delegation_contract_id", "delegation_contract_fingerprint"] {
        if accepted.get(key) != current.get(key) {
            return false;
        }
    }
    true
}

fn optional_set_narrows(
    accepted: Option<BTreeSet<String>>,
    current: Option<BTreeSet<String>>,
) -> bool {
    match (accepted, current) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(accepted), Some(current)) => current.is_subset(&accepted),
    }
}

fn string_set(value: Option<&Value>) -> Option<BTreeSet<String>> {
    let value = value?;
    if value.is_null() {
        return None;
    }
    value.as_array().map(|values| {
        values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    })
}

fn validate_contract(contract: &AgentScopeContract) -> MedusaResult<()> {
    if contract.schema_version != AGENT_SCOPE_SCHEMA_VERSION
        || contract.scope_id.trim().is_empty()
        || contract.fingerprint.trim().is_empty()
        || contract.session_id.trim().is_empty()
        || contract.repository_identity.trim().is_empty()
        || contract.cancellation_owner.trim().is_empty()
    {
        return Err(scope_error("agent scope contract is incomplete"));
    }
    let material = ScopeAuthorityMaterial {
        schema_version: contract.schema_version,
        session_id: &contract.session_id,
        repository_identity: &contract.repository_identity,
        initial_repository_revision: &contract.initial_repository_revision,
        mode: contract.mode,
        provider_profile: &contract.provider_profile,
        execution_policy: &contract.execution_policy,
        effective_tools: &contract.effective_tools,
        capability_registry_fingerprint: &contract.capability_registry_fingerprint,
        team_id: &contract.team_id,
        member_id: &contract.member_id,
        analysis_workspace: contract.analysis_workspace,
        cancellation_owner: &contract.cancellation_owner,
    };
    let expected = sha256(&serde_json::to_vec(&material).map_err(json_error)?);
    if contract.fingerprint != expected || contract.scope_id != format!("agent-scope-v1-{expected}")
    {
        return Err(scope_error("agent scope contract fingerprint mismatch"));
    }
    Ok(())
}

fn validate_state_binding(
    contract: &AgentScopeContract,
    state: &AgentScopeState,
) -> MedusaResult<()> {
    if state.schema_version != AGENT_SCOPE_SCHEMA_VERSION
        || state.scope_id != contract.scope_id
        || state.scope_fingerprint != contract.fingerprint
        || state.generation == 0
    {
        return Err(scope_error(
            "agent scope lifecycle state does not match its immutable contract",
        ));
    }
    Ok(())
}

fn load_contract(repo: &Path, session_id: &str) -> MedusaResult<AgentScopeContract> {
    let path = contract_path(repo, session_id);
    let bytes = fs::read(&path).map_err(|error| {
        scope_error(format!(
            "legacy_scope_unknown: missing durable agent scope for {session_id}: {error}"
        ))
    })?;
    let contract: AgentScopeContract = serde_json::from_slice(&bytes).map_err(json_error)?;
    validate_contract(&contract)?;
    if contract.session_id != session_id {
        return Err(scope_error("agent scope session identity mismatch"));
    }
    Ok(contract)
}

fn load_state(path: &Path) -> MedusaResult<AgentScopeState> {
    serde_json::from_slice(&fs::read(path).map_err(|error| {
        scope_error(format!(
            "agent scope lifecycle state is unavailable: {error}"
        ))
    })?)
    .map_err(json_error)
}

fn persist_immutable_json<T: Serialize>(path: &Path, value: &T) -> MedusaResult<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(json_error)?;
    if path.is_file() {
        let existing = fs::read(path)?;
        if existing == bytes {
            return Ok(());
        }
        return Err(scope_error(format!(
            "immutable agent scope artifact already exists with different content: {}",
            path.display()
        )));
    }
    let parent = path
        .parent()
        .ok_or_else(|| scope_error("agent scope artifact path has no parent"))?;
    fs::create_dir_all(parent)?;
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(())
}

fn persist_state(path: &Path, state: &AgentScopeState) -> MedusaResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| scope_error("agent scope state path has no parent"))?;
    fs::create_dir_all(parent)?;
    let bytes = serde_json::to_vec_pretty(state).map_err(json_error)?;
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    fs::rename(&temporary, path)?;
    Ok(())
}

fn capability_registry_fingerprint(repo: &Path) -> MedusaResult<String> {
    let registry = CapabilityRegistry::discover(repo.to_path_buf()).map_err(|error| {
        scope_error(format!("agent scope capability discovery failed: {error}"))
    })?;
    Ok(sha256(
        &serde_json::to_vec(&registry.protocol_report()).map_err(json_error)?,
    ))
}

fn repository_identity(repo: &Path) -> MedusaResult<String> {
    Ok(repo
        .canonicalize()
        .map_err(|error| scope_error(format!("repository identity is unavailable: {error}")))?
        .to_string_lossy()
        .into_owned())
}

fn repository_revision(repo: &Path) -> Option<String> {
    let mut graph = medusa_intelligence::RepositoryGraph::open(repo).ok()?;
    if graph.freshness() != medusa_intelligence::RepositoryGraphFreshness::Current {
        graph.refresh().ok()?;
    }
    (graph.freshness() == medusa_intelligence::RepositoryGraphFreshness::Current)
        .then(|| graph.snapshot().repository_revision.clone())
}

fn contract_path(repo: &Path, session_id: &str) -> PathBuf {
    repo.join(".medusa/agent-scopes")
        .join(format!("{session_id}.contract.json"))
}

fn state_path(repo: &Path, session_id: &str) -> PathBuf {
    repo.join(".medusa/agent-scopes")
        .join(format!("{session_id}.state.json"))
}

fn scope_ref(state: &AgentScopeState) -> AgentScopeRef {
    AgentScopeRef {
        scope_id: state.scope_id.clone(),
        scope_fingerprint: state.scope_fingerprint.clone(),
        generation: state.generation,
    }
}

fn canonical_strings(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values.dedup();
    values
}

fn canonicalize_value(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize_value).collect()),
        Value::Object(values) => {
            let mut entries = values.into_iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            let mut canonical = serde_json::Map::new();
            for (key, value) in entries {
                canonical.insert(key, canonicalize_value(value));
            }
            Value::Object(canonical)
        }
        other => other,
    }
}

fn unix_ms() -> i64 {
    i64::try_from(OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000).unwrap_or(i64::MAX)
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn json_error(error: serde_json::Error) -> MedusaError {
    scope_error(format!("agent scope serialization failed: {error}"))
}

fn scope_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::PolicyDenied, ErrorCategory::Policy, message)
}

fn reconciliation_error(message: impl Into<String>) -> MedusaError {
    let mut error = scope_error(format!(
        "agent_scope_reconciliation_required: {}",
        message.into()
    ));
    error.context.insert(
        "agent_scope_reconciliation_required".to_owned(),
        json!(true),
    );
    error
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn policy(tools: Option<&[&str]>, paths: Option<&[&str]>, questions: bool) -> Value {
        json!({
            "allowed_tools": tools.map(|values| values.to_vec()),
            "allow_user_questions": questions,
            "allowed_write_paths": paths.map(|values| values.to_vec()),
            "delegation_contract_id": null,
            "delegation_contract_fingerprint": null,
        })
    }

    #[test]
    fn policy_may_narrow_but_not_widen() {
        let accepted = policy(Some(&["fs_read", "fs_write"]), Some(&["src/lib.rs"]), false);
        assert!(policy_narrows_or_equals(
            &accepted,
            &policy(Some(&["fs_read"]), Some(&["src/lib.rs"]), false)
        ));
        assert!(!policy_narrows_or_equals(
            &accepted,
            &policy(
                Some(&["fs_read", "fs_write", "shell_run"]),
                Some(&["src/lib.rs"]),
                false
            )
        ));
        assert!(!policy_narrows_or_equals(
            &accepted,
            &policy(Some(&["fs_read"]), Some(&["src/lib.rs"]), true)
        ));
    }

    #[test]
    fn unrestricted_accepted_policy_can_be_narrowed() {
        let accepted = policy(None, None, true);
        assert!(policy_narrows_or_equals(
            &accepted,
            &policy(Some(&["fs_read"]), Some(&["src/lib.rs"]), false)
        ));
    }

    #[test]
    fn stop_is_terminal_and_idempotent() {
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
        publish_agent_scope(
            repo.path(),
            &contract,
            provider,
            execution,
            vec!["fs_read".into()],
        )
        .expect("publish");
        let first = stop_agent_scope(repo.path(), session.as_str(), "done").expect("stop");
        let second =
            stop_agent_scope(repo.path(), session.as_str(), "ignored").expect("idempotent");
        assert_eq!(first.scope, second.scope);
        assert_eq!(second.cause, "done");
        assert!(load_published_scope_ref(repo.path(), session.as_str()).is_err());
    }

    #[test]
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
        assert!(
            publish_agent_scope(
                repo.path(),
                &contract,
                json!({"provider":"test","model":"test"}),
                policy(Some(&["fs_read"]), None, false),
                vec!["fs_read".into()],
            )
            .is_err()
        );
        let state = load_state(&state_path(repo.path(), session.as_str())).expect("state");
        assert_eq!(state.lifecycle, AgentScopeLifecycle::FailedStart);
        assert!(
            state
                .owned_resources
                .iter()
                .all(|resource| !resource.active)
        );
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
        revoke_agent_scope_tool(repo.path(), session.as_str(), &scope, "shell_run")
            .expect("revoke");
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
        assert!(
            stop_agent_scope_generation(repo.path(), session.as_str(), &stale, "stale").is_err()
        );
        stop_agent_scope_generation(repo.path(), session.as_str(), &current, "current")
            .expect("stop");
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
        assert!(
            state
                .owned_resources
                .iter()
                .all(|resource| !resource.active)
        );
    }

    #[test]
    fn resume_advances_generation_without_changing_authority() {
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
        let first = publish_agent_scope(
            repo.path(),
            &contract,
            provider.clone(),
            execution.clone(),
            vec!["fs_read".into()],
        )
        .expect("publish");
        let resumed = resume_agent_scope(
            repo.path(),
            session.as_str(),
            provider,
            execution,
            vec!["fs_read".into()],
        )
        .expect("resume");
        assert_eq!(first.scope_id, resumed.scope_id);
        assert_eq!(first.scope_fingerprint, resumed.scope_fingerprint);
        assert_eq!(resumed.generation, first.generation + 1);
    }
}
