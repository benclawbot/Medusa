//! Immutable worker delegation contracts and per-attempt bindings.
//!
//! A model-backed worker receives authority only through a sealed contract persisted before
//! its session is created. Retries reuse the same contract and may only lose capabilities when
//! current runtime policy is narrower.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use medusa_capabilities::{CAPABILITY_REGISTRY_SCHEMA_VERSION, CapabilityRegistry};
use medusa_config::{Mode, ModelConfig};
use medusa_core::MedusaResult;
use medusa_protocol::{Actor, EventPayload};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::{
    evidence::append_event,
    session::{AgentSession, persist},
    team::{AgentExecutionPolicy, TeamRole},
};

pub const DELEGATION_CONTRACT_SCHEMA_VERSION: u16 = 1;
pub const DELEGATION_ROLE_POLICY_VERSION: u16 = 1;
pub const DELEGATION_SYSTEM_POLICY_VERSION: &str = "medusa-delegation-v1";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegatedMutationAuthority {
    None,
    IsolatedWorktree,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegatedApprovalPolicy {
    Never,
    ParentOnly,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DelegationContractMaterial {
    pub root_execution_id: String,
    pub parent_worker_id: String,
    pub parent_session_id: Option<String>,
    pub worker_id: String,
    pub task_id: String,
    pub accepted_lease_epoch: u64,
    pub delegation_depth: u8,
    pub max_delegation_depth: u8,
    pub initial_session_id: String,
    pub repository_identity: String,
    pub repository_revision: String,
    pub worktree_identity: Option<String>,
    pub read_scopes: Vec<String>,
    pub write_scopes: Vec<String>,
    pub mutation_authority: DelegatedMutationAuthority,
    pub capability_registry_schema_version: u16,
    pub capability_registry_fingerprint: String,
    pub allowed_tools: Vec<String>,
    pub role_policy_version: u16,
    pub role_policy_fingerprint: String,
    pub allow_user_questions: bool,
    pub approval_policy: DelegatedApprovalPolicy,
    pub network_allowed: bool,
    pub process_allowed: bool,
    pub browser_allowed: bool,
    pub credentialed_actions_allowed: bool,
    pub model: ModelConfig,
    pub mode: Mode,
    pub max_turns: u32,
    pub max_model_calls: u32,
    pub retry_attempts: u32,
    pub context_fingerprint: String,
    pub plan_fingerprint: String,
    pub objective: String,
    pub required_evidence: Vec<String>,
    pub system_policy_version: String,
}

impl DelegationContractMaterial {
    fn canonicalize(&mut self) {
        for values in [
            &mut self.read_scopes,
            &mut self.write_scopes,
            &mut self.allowed_tools,
            &mut self.required_evidence,
        ] {
            values.sort();
            values.dedup();
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.root_execution_id.trim().is_empty()
            || self.parent_worker_id.trim().is_empty()
            || self.worker_id.trim().is_empty()
            || self.task_id.trim().is_empty()
            || self.accepted_lease_epoch == 0
            || self.initial_session_id.trim().is_empty()
            || self.repository_identity.trim().is_empty()
            || self.repository_revision.trim().is_empty()
            || self.capability_registry_fingerprint.trim().is_empty()
            || self.role_policy_fingerprint.trim().is_empty()
            || self.context_fingerprint.trim().is_empty()
            || self.plan_fingerprint.trim().is_empty()
            || self.objective.trim().is_empty()
            || self.system_policy_version.trim().is_empty()
            || self.max_turns == 0
            || self.max_model_calls == 0
            || self.retry_attempts == 0
        {
            return Err("delegation contract authority is incomplete".to_owned());
        }
        if self.delegation_depth > self.max_delegation_depth {
            return Err("delegation depth exceeds the contract limit".to_owned());
        }
        match self.mutation_authority {
            DelegatedMutationAuthority::None if !self.write_scopes.is_empty() => {
                return Err("read-only delegation contract cannot contain write scopes".to_owned());
            }
            DelegatedMutationAuthority::IsolatedWorktree if self.write_scopes.is_empty() => {
                return Err(
                    "mutating delegation contract requires explicit write scopes".to_owned(),
                );
            }
            _ => {}
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DelegationContract {
    pub schema_version: u16,
    pub contract_id: String,
    pub fingerprint: String,
    pub predecessor_contract_id: Option<String>,
    pub authority: DelegationContractMaterial,
}

impl DelegationContract {
    pub fn seal(
        predecessor_contract_id: Option<String>,
        mut authority: DelegationContractMaterial,
    ) -> Result<Self, String> {
        authority.canonicalize();
        authority.validate()?;
        let fingerprint = sha256(
            &serde_json::to_vec(&(
                DELEGATION_CONTRACT_SCHEMA_VERSION,
                &predecessor_contract_id,
                &authority,
            ))
            .map_err(|error| error.to_string())?,
        );
        Ok(Self {
            schema_version: DELEGATION_CONTRACT_SCHEMA_VERSION,
            contract_id: format!("delegation-v1-{fingerprint}"),
            fingerprint,
            predecessor_contract_id,
            authority,
        })
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != DELEGATION_CONTRACT_SCHEMA_VERSION {
            return Err(format!(
                "unsupported delegation contract schema version {}",
                self.schema_version
            ));
        }
        self.authority.validate()?;
        let fingerprint = sha256(
            &serde_json::to_vec(&(
                self.schema_version,
                &self.predecessor_contract_id,
                &self.authority,
            ))
            .map_err(|error| error.to_string())?,
        );
        if fingerprint != self.fingerprint
            || self.contract_id != format!("delegation-v1-{fingerprint}")
        {
            return Err("delegation contract fingerprint mismatch".to_owned());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DelegationAttemptBinding {
    pub schema_version: u16,
    pub fingerprint: String,
    pub contract_id: String,
    pub contract_fingerprint: String,
    pub lease_epoch: u64,
    pub attempt_ordinal: u32,
    pub session_id: String,
    pub predecessor_session_id: Option<String>,
}

impl DelegationAttemptBinding {
    pub fn new(
        contract: &DelegationContract,
        lease_epoch: u64,
        attempt_ordinal: u32,
        session_id: String,
        predecessor_session_id: Option<String>,
    ) -> Result<Self, String> {
        if lease_epoch == 0 || attempt_ordinal == 0 || session_id.trim().is_empty() {
            return Err("delegation attempt binding is incomplete".to_owned());
        }
        contract.validate()?;
        let fingerprint = sha256(
            &serde_json::to_vec(&(
                DELEGATION_CONTRACT_SCHEMA_VERSION,
                &contract.contract_id,
                &contract.fingerprint,
                lease_epoch,
                attempt_ordinal,
                &session_id,
                &predecessor_session_id,
            ))
            .map_err(|error| error.to_string())?,
        );
        Ok(Self {
            schema_version: DELEGATION_CONTRACT_SCHEMA_VERSION,
            fingerprint,
            contract_id: contract.contract_id.clone(),
            contract_fingerprint: contract.fingerprint.clone(),
            lease_epoch,
            attempt_ordinal,
            session_id,
            predecessor_session_id,
        })
    }

    pub fn validate(&self, contract: &DelegationContract) -> Result<(), String> {
        if self.schema_version != DELEGATION_CONTRACT_SCHEMA_VERSION
            || self.contract_id != contract.contract_id
            || self.contract_fingerprint != contract.fingerprint
            || self.lease_epoch == 0
            || self.attempt_ordinal == 0
            || self.session_id.trim().is_empty()
        {
            return Err("delegation attempt does not match its contract".to_owned());
        }
        let expected = Self::new(
            contract,
            self.lease_epoch,
            self.attempt_ordinal,
            self.session_id.clone(),
            self.predecessor_session_id.clone(),
        )?;
        if expected.fingerprint != self.fingerprint {
            return Err("delegation attempt fingerprint mismatch".to_owned());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelegationCapabilitySnapshot {
    pub schema_version: u16,
    pub fingerprint: String,
    pub allowed_tools: Vec<String>,
}

pub fn snapshot_delegated_capabilities(
    repo: &Path,
    mode: Mode,
    policy: &AgentExecutionPolicy,
) -> Result<DelegationCapabilitySnapshot, String> {
    let registry =
        CapabilityRegistry::discover(repo.to_path_buf()).map_err(|error| error.to_string())?;
    let report = registry.protocol_report();
    let fingerprint = sha256(&serde_json::to_vec(&report).map_err(|error| error.to_string())?);
    let mut available = registry
        .model_tools(mode == Mode::ReadOnly)
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    available.extend([
        "team_list_members".to_owned(),
        "team_read_messages".to_owned(),
        "team_send_message".to_owned(),
    ]);
    if let Some(allowed) = policy.allowed_tool_ids() {
        available.retain(|tool| allowed.binary_search(tool).is_ok());
    }
    // Skills are repository/user mutable packages. Delegated workers do not inherit a dynamic
    // skill namespace; a future contract schema can admit digest-pinned skill packages.
    available.retain(|tool| !matches!(tool.as_str(), "skill_read" | "skill_execute"));
    available.sort();
    available.dedup();
    Ok(DelegationCapabilitySnapshot {
        schema_version: CAPABILITY_REGISTRY_SCHEMA_VERSION,
        fingerprint,
        allowed_tools: available,
    })
}

#[must_use]
pub fn delegation_execution_policy(
    contract: &DelegationContract,
    role: TeamRole,
) -> AgentExecutionPolicy {
    AgentExecutionPolicy::for_team_role(role)
        .intersect_allowed_tools(contract.authority.allowed_tools.clone())
        .with_allowed_write_paths(contract.authority.write_scopes.clone())
        .with_delegation_binding(contract.contract_id.clone(), contract.fingerprint.clone())
}

pub fn bind_session_to_delegation(
    session: &mut AgentSession,
    contract: &DelegationContract,
    attempt: &DelegationAttemptBinding,
) -> MedusaResult<()> {
    contract.validate().map_err(delegation_error)?;
    attempt.validate(contract).map_err(delegation_error)?;
    if session.id.as_str() != attempt.session_id {
        return Err(delegation_error(
            "session identity does not match its delegation attempt",
        ));
    }
    append_event(
        session,
        Actor::Coordinator,
        EventPayload::WorkerEvidenceRecorded {
            evidence: json!({
                "kind": "delegation_contract_binding",
                "contract_id": contract.contract_id,
                "contract_fingerprint": contract.fingerprint,
                "attempt_fingerprint": attempt.fingerprint,
                "lease_epoch": attempt.lease_epoch,
                "attempt_ordinal": attempt.attempt_ordinal,
                "predecessor_session_id": attempt.predecessor_session_id,
            }),
        },
    )?;
    session.updated_at = OffsetDateTime::now_utc();
    persist(session)
}

#[derive(Clone, Debug)]
pub struct DelegationContractStore {
    root: PathBuf,
}

impl DelegationContractStore {
    #[must_use]
    pub fn new(execution_root: impl Into<PathBuf>) -> Self {
        Self {
            root: execution_root.into().join("delegation-contracts"),
        }
    }

    pub fn persist_contract(&self, contract: &DelegationContract) -> Result<PathBuf, String> {
        contract.validate()?;
        let path = self
            .root
            .join("contracts")
            .join(format!("{}.json", contract.contract_id));
        persist_immutable_json(&path, contract)?;
        Ok(path)
    }

    pub fn load_contract(&self, contract_id: &str) -> Result<DelegationContract, String> {
        if !contract_id.starts_with("delegation-v1-") {
            return Err("invalid delegation contract identifier".to_owned());
        }
        let path = self
            .root
            .join("contracts")
            .join(format!("{contract_id}.json"));
        let contract: DelegationContract =
            serde_json::from_slice(&fs::read(&path).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        contract.validate()?;
        Ok(contract)
    }

    pub fn persist_attempt(&self, attempt: &DelegationAttemptBinding) -> Result<PathBuf, String> {
        let path = self
            .root
            .join("attempts")
            .join(&attempt.contract_id)
            .join(format!(
                "{:020}-{}.json",
                attempt.attempt_ordinal, attempt.fingerprint
            ));
        persist_immutable_json(&path, attempt)?;
        Ok(path)
    }

    pub fn latest_attempt(
        &self,
        contract_id: &str,
    ) -> Result<Option<DelegationAttemptBinding>, String> {
        let directory = self.root.join("attempts").join(contract_id);
        if !directory.is_dir() {
            return Ok(None);
        }
        let mut paths = fs::read_dir(directory)
            .map_err(|error| error.to_string())?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .collect::<Vec<_>>();
        paths.sort();
        let Some(path) = paths.pop() else {
            return Ok(None);
        };
        serde_json::from_slice(&fs::read(path).map_err(|error| error.to_string())?)
            .map(Some)
            .map_err(|error| error.to_string())
    }
}

pub fn fingerprint_json(value: &Value) -> Result<String, String> {
    Ok(sha256(
        &serde_json::to_vec(value).map_err(|error| error.to_string())?,
    ))
}

fn persist_immutable_json<T>(path: &Path, value: &T) -> Result<(), String>
where
    T: Serialize + for<'de> Deserialize<'de> + PartialEq,
{
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    if path.is_file() {
        let restored: T =
            serde_json::from_slice(&fs::read(path).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        if &restored == value {
            return Ok(());
        }
        return Err(format!(
            "immutable delegation artifact already exists with different content: {}",
            path.display()
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| "delegation artifact path has no parent".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    file.write_all(&bytes).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn delegation_error(message: impl Into<String>) -> medusa_core::MedusaError {
    medusa_core::MedusaError::new(
        medusa_core::ErrorCode::PolicyDenied,
        medusa_core::ErrorCategory::Policy,
        message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn material() -> DelegationContractMaterial {
        DelegationContractMaterial {
            root_execution_id: "exec".into(),
            parent_worker_id: "lead".into(),
            parent_session_id: None,
            worker_id: "worker-a".into(),
            task_id: "analyze".into(),
            accepted_lease_epoch: 1,
            delegation_depth: 1,
            max_delegation_depth: 1,
            initial_session_id: "session-a".into(),
            repository_identity: "repo-fingerprint".into(),
            repository_revision: "rev-a".into(),
            worktree_identity: None,
            read_scopes: vec!["repository".into()],
            write_scopes: Vec::new(),
            mutation_authority: DelegatedMutationAuthority::None,
            capability_registry_schema_version: CAPABILITY_REGISTRY_SCHEMA_VERSION,
            capability_registry_fingerprint: "capabilities".into(),
            allowed_tools: vec!["fs_read".into()],
            role_policy_version: DELEGATION_ROLE_POLICY_VERSION,
            role_policy_fingerprint: "policy".into(),
            allow_user_questions: false,
            approval_policy: DelegatedApprovalPolicy::Never,
            network_allowed: false,
            process_allowed: false,
            browser_allowed: false,
            credentialed_actions_allowed: false,
            model: ModelConfig::default(),
            mode: Mode::ReadOnly,
            max_turns: 4,
            max_model_calls: 4,
            retry_attempts: 2,
            context_fingerprint: "context".into(),
            plan_fingerprint: "plan".into(),
            objective: "inspect".into(),
            required_evidence: vec!["summary".into()],
            system_policy_version: DELEGATION_SYSTEM_POLICY_VERSION.into(),
        }
    }

    #[test]
    fn contract_fingerprint_is_stable_after_round_trip() {
        let contract = DelegationContract::seal(None, material()).expect("seal");
        let encoded = serde_json::to_vec(&contract).expect("serialize");
        let restored: DelegationContract = serde_json::from_slice(&encoded).expect("deserialize");
        restored.validate().expect("valid");
        assert_eq!(restored, contract);
    }

    #[test]
    fn authority_widening_changes_contract_identity() {
        let base = DelegationContract::seal(None, material()).expect("base");
        let mut wider = material();
        wider.allowed_tools.push("web_fetch".into());
        let wider = DelegationContract::seal(None, wider).expect("wider");
        assert_ne!(base.contract_id, wider.contract_id);
    }

    #[test]
    fn persisted_contract_cannot_be_overwritten() {
        let directory = tempfile::tempdir().expect("tempdir");
        let store = DelegationContractStore::new(directory.path());
        let contract = DelegationContract::seal(None, material()).expect("contract");
        store.persist_contract(&contract).expect("persist");
        let restored = store.load_contract(&contract.contract_id).expect("load");
        assert_eq!(restored, contract);
    }
}
