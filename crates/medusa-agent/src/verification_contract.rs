use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult, hidden_command};
use medusa_evidence::{
    ChangeKind, ChangedComponent, VerificationCheck, VerificationCheckKind, VerificationPlanner,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const CONTRACT_SCHEMA_VERSION: u16 = 1;
const MIN_RESERVED_OUTPUT_TOKENS: u32 = 512;
const DEFAULT_RESERVED_TIME_MS: u64 = 300_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationRequirement {
    Required,
    Recommended,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationResolution {
    Pending,
    Passed,
    Failed,
    Stale,
    Superseded,
    Unavailable,
    ExplicitlyWaived,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VerificationConstraint {
    pub source: String,
    pub requested_scope: Vec<String>,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VerificationBudgetReservation {
    pub reserved_output_tokens: u32,
    pub reserved_time_ms: u64,
}

impl VerificationBudgetReservation {
    #[must_use]
    pub fn for_output_budget(total_output_tokens: u32) -> Self {
        let proportional = total_output_tokens.div_ceil(8);
        Self {
            reserved_output_tokens: proportional
                .max(MIN_RESERVED_OUTPUT_TOKENS.min(total_output_tokens))
                .min(total_output_tokens),
            reserved_time_ms: DEFAULT_RESERVED_TIME_MS,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RepositoryStateIdentity {
    pub head: String,
    pub workspace_fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContractCheck {
    pub id: String,
    pub kind: VerificationCheckKind,
    pub requirement: VerificationRequirement,
    pub resolution: VerificationResolution,
    pub reason: String,
    pub scope_paths: BTreeSet<String>,
    pub platform: Option<String>,
    pub receipt_scope_fingerprint: Option<String>,
}

impl ContractCheck {
    fn from_planned(check: &VerificationCheck, changed_paths: &BTreeSet<String>) -> Self {
        let scope_paths = check_scope(check, changed_paths);
        Self {
            id: check.id.clone(),
            kind: check.kind,
            requirement: if check.required {
                VerificationRequirement::Required
            } else {
                VerificationRequirement::Recommended
            },
            resolution: VerificationResolution::Pending,
            reason: check.reason.clone(),
            scope_paths,
            platform: None,
            receipt_scope_fingerprint: None,
        }
    }

    fn blocks_completion(&self) -> bool {
        self.requirement == VerificationRequirement::Required
            && !matches!(
                self.resolution,
                VerificationResolution::Passed | VerificationResolution::ExplicitlyWaived
            )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VerificationContract {
    pub schema_version: u16,
    pub session_id: String,
    pub changed_paths: BTreeSet<String>,
    pub checks: Vec<ContractCheck>,
    pub constraints: Vec<VerificationConstraint>,
    pub budget: VerificationBudgetReservation,
    pub planned_identity: RepositoryStateIdentity,
    pub completion_identity: Option<RepositoryStateIdentity>,
    pub authoritative_passed: bool,
}

impl VerificationContract {
    pub fn derive(
        repo: &Path,
        session_id: &str,
        changed_paths: BTreeSet<String>,
        total_output_tokens: u32,
    ) -> MedusaResult<Self> {
        if session_id.trim().is_empty() || changed_paths.is_empty() {
            return Err(invalid(
                "verification contract requires session id and changed paths",
            ));
        }
        let identity = workspace_identity(repo, &changed_paths)?;
        let components = components_for(repo, &changed_paths)?;
        let repository_fingerprint = repository_fingerprint(repo, &identity.head);
        let plan = VerificationPlanner::plan(
            repo,
            repository_fingerprint,
            identity.head.clone(),
            &components,
            &[],
        )
        .map_err(evidence_error)?;
        let mut checks = plan
            .checks
            .iter()
            .map(|check| ContractCheck::from_planned(check, &changed_paths))
            .collect::<Vec<_>>();
        checks.extend(platform_policy_checks(repo, &changed_paths)?);
        checks.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(Self {
            schema_version: CONTRACT_SCHEMA_VERSION,
            session_id: session_id.to_owned(),
            changed_paths,
            checks,
            constraints: Vec::new(),
            budget: VerificationBudgetReservation::for_output_budget(total_output_tokens),
            planned_identity: identity,
            completion_identity: None,
            authoritative_passed: false,
        })
    }

    pub fn refresh_after_mutation(
        &mut self,
        repo: &Path,
        newly_affected_paths: &BTreeSet<String>,
        total_output_tokens: u32,
    ) -> MedusaResult<()> {
        let mut all_paths = self.changed_paths.clone();
        all_paths.extend(newly_affected_paths.iter().cloned());
        let mut next = Self::derive(repo, &self.session_id, all_paths, total_output_tokens)?;
        next.constraints = self.constraints.clone();
        for check in &mut next.checks {
            let Some(previous) = self.checks.iter().find(|prior| prior.id == check.id) else {
                continue;
            };
            let overlaps = paths_overlap(&previous.scope_paths, newly_affected_paths);
            if overlaps {
                check.resolution = match previous.resolution {
                    VerificationResolution::Passed | VerificationResolution::Failed => {
                        VerificationResolution::Stale
                    }
                    VerificationResolution::ExplicitlyWaived => {
                        VerificationResolution::ExplicitlyWaived
                    }
                    VerificationResolution::Superseded => VerificationResolution::Superseded,
                    VerificationResolution::Unavailable => VerificationResolution::Unavailable,
                    VerificationResolution::Pending | VerificationResolution::Stale => {
                        previous.resolution
                    }
                };
                check.receipt_scope_fingerprint = None;
            } else {
                check.resolution = previous.resolution;
                check.receipt_scope_fingerprint = previous.receipt_scope_fingerprint.clone();
            }
        }
        *self = next;
        Ok(())
    }

    #[cfg(test)]
    pub fn record_user_reduced_scope(
        &mut self,
        requested_scope: Vec<String>,
        reason: impl Into<String>,
    ) {
        self.constraints.push(VerificationConstraint {
            source: "user".to_owned(),
            requested_scope,
            reason: reason.into(),
        });
    }

    #[cfg(test)]
    pub fn waive_check(&mut self, check_id: &str, reason: impl Into<String>) -> MedusaResult<()> {
        let reason = reason.into();
        if reason.trim().is_empty() {
            return Err(invalid("explicit verification waiver requires a reason"));
        }
        let check = self
            .checks
            .iter_mut()
            .find(|check| check.id == check_id)
            .ok_or_else(|| invalid(format!("unknown verification check {check_id}")))?;
        check.resolution = VerificationResolution::ExplicitlyWaived;
        check.receipt_scope_fingerprint = None;
        self.constraints.push(VerificationConstraint {
            source: "explicit_waiver".to_owned(),
            requested_scope: check.scope_paths.iter().cloned().collect(),
            reason,
        });
        self.completion_identity = None;
        self.authoritative_passed = false;
        Ok(())
    }

    #[cfg(test)]
    pub fn supersede_check(&mut self, check_id: &str) -> MedusaResult<()> {
        let check = self
            .checks
            .iter_mut()
            .find(|check| check.id == check_id)
            .ok_or_else(|| invalid(format!("unknown verification check {check_id}")))?;
        check.resolution = VerificationResolution::Superseded;
        check.receipt_scope_fingerprint = None;
        self.completion_identity = None;
        self.authoritative_passed = false;
        Ok(())
    }

    #[cfg(test)]
    pub fn mark_unavailable(&mut self, check_id: &str) -> MedusaResult<()> {
        let check = self
            .checks
            .iter_mut()
            .find(|check| check.id == check_id)
            .ok_or_else(|| invalid(format!("unknown verification check {check_id}")))?;
        check.resolution = VerificationResolution::Unavailable;
        check.receipt_scope_fingerprint = None;
        self.completion_identity = None;
        self.authoritative_passed = false;
        Ok(())
    }

    #[cfg(test)]
    pub fn record_check_result(
        &mut self,
        repo: &Path,
        check_id: &str,
        passed: bool,
    ) -> MedusaResult<()> {
        let check = self
            .checks
            .iter_mut()
            .find(|check| check.id == check_id)
            .ok_or_else(|| invalid(format!("unknown verification check {check_id}")))?;
        let fingerprint = scope_fingerprint(repo, &check.scope_paths)?;
        check.resolution = if passed {
            VerificationResolution::Passed
        } else {
            VerificationResolution::Failed
        };
        check.receipt_scope_fingerprint = Some(fingerprint);
        self.completion_identity = None;
        self.authoritative_passed = false;
        Ok(())
    }

    pub fn apply_authoritative_summary(
        &mut self,
        repo: &Path,
        summary: &[String],
        aggregate_passed: bool,
    ) -> MedusaResult<()> {
        let parsed = summary
            .iter()
            .filter_map(|line| parse_check_summary(line))
            .collect::<BTreeMap<_, _>>();
        let current_platform = current_platform();
        for check in &mut self.checks {
            if let Some(passed) = parsed.get(check.id.as_str()).copied() {
                check.resolution = if passed {
                    VerificationResolution::Passed
                } else {
                    VerificationResolution::Failed
                };
                check.receipt_scope_fingerprint =
                    Some(scope_fingerprint(repo, &check.scope_paths)?);
            } else if check.platform.as_deref() == Some(current_platform) && aggregate_passed {
                check.resolution = VerificationResolution::Passed;
                check.receipt_scope_fingerprint =
                    Some(scope_fingerprint(repo, &check.scope_paths)?);
            }
        }
        let identity = workspace_identity(repo, &self.changed_paths)?;
        self.authoritative_passed = aggregate_passed;
        self.completion_identity = aggregate_passed.then_some(identity);
        Ok(())
    }

    #[must_use]
    pub fn unresolved_required(&self) -> Vec<&ContractCheck> {
        self.checks
            .iter()
            .filter(|check| check.blocks_completion())
            .collect()
    }

    pub fn completion_ready(&self, repo: &Path) -> MedusaResult<bool> {
        if !self.authoritative_passed || !self.unresolved_required().is_empty() {
            return Ok(false);
        }
        for check in &self.checks {
            if check.resolution != VerificationResolution::Passed {
                continue;
            }
            let Some(expected) = check.receipt_scope_fingerprint.as_deref() else {
                return Ok(false);
            };
            if scope_fingerprint(repo, &check.scope_paths)? != expected {
                return Ok(false);
            }
        }
        let current = workspace_identity(repo, &self.changed_paths)?;
        Ok(self.completion_identity.as_ref() == Some(&current))
    }
}

pub(crate) fn refresh_persisted_after_mutation(
    repo: &Path,
    session_id: &str,
    declared_paths: &[String],
    total_output_tokens: u32,
) -> MedusaResult<VerificationContract> {
    let discovered = workspace_changed_paths(repo);
    let mut affected = declared_paths
        .iter()
        .filter(|path| !path.trim().is_empty())
        .cloned()
        .collect::<BTreeSet<_>>();
    if affected.is_empty() {
        affected.extend(discovered.iter().cloned());
    }
    let path = contract_path(repo, session_id);
    let mut contract = if path.is_file() {
        let mut prior: VerificationContract = serde_json::from_slice(&fs::read(&path)?)?;
        prior.refresh_after_mutation(repo, &affected, total_output_tokens)?;
        prior
    } else {
        let mut changed = discovered;
        changed.extend(affected.iter().cloned());
        VerificationContract::derive(repo, session_id, changed, total_output_tokens)?
    };
    contract.completion_identity = None;
    contract.authoritative_passed = false;
    persist_contract(repo, &contract)?;
    Ok(contract)
}

pub(crate) fn apply_persisted_authoritative_summary(
    repo: &Path,
    session_id: &str,
    summary: &[String],
    aggregate_passed: bool,
) -> MedusaResult<VerificationContract> {
    let mut contract = load_contract(repo, session_id)?.ok_or_else(|| {
        invalid("coding mutation reached final verification without a verification contract")
    })?;
    contract.apply_authoritative_summary(repo, summary, aggregate_passed)?;
    persist_contract(repo, &contract)?;
    Ok(contract)
}

pub(crate) fn completion_ready(repo: &Path, session_id: &str) -> MedusaResult<bool> {
    let Some(contract) = load_contract(repo, session_id)? else {
        return Ok(false);
    };
    contract.completion_ready(repo)
}

pub(crate) fn mark_persisted_unavailable(
    repo: &Path,
    session_id: &str,
    reason: &str,
) -> MedusaResult<VerificationContract> {
    let mut contract = load_contract(repo, session_id)?.ok_or_else(|| {
        invalid("coding mutation reached unavailable verification without a verification contract")
    })?;
    for check in &mut contract.checks {
        if check.requirement == VerificationRequirement::Required
            && !matches!(
                check.resolution,
                VerificationResolution::Passed
                    | VerificationResolution::ExplicitlyWaived
                    | VerificationResolution::Superseded
            )
        {
            check.resolution = VerificationResolution::Unavailable;
            check.receipt_scope_fingerprint = None;
        }
    }
    contract.constraints.push(VerificationConstraint {
        source: "verification_authority".to_owned(),
        requested_scope: contract.changed_paths.iter().cloned().collect(),
        reason: reason.to_owned(),
    });
    contract.authoritative_passed = false;
    contract.completion_identity = None;
    persist_contract(repo, &contract)?;
    Ok(contract)
}

pub(crate) fn unresolved_summary(repo: &Path, session_id: &str) -> MedusaResult<Vec<String>> {
    let Some(contract) = load_contract(repo, session_id)? else {
        return Ok(vec!["verification_contract=missing".to_owned()]);
    };
    Ok(contract
        .unresolved_required()
        .into_iter()
        .map(|check| {
            format!(
                "verification_contract_unresolved={}:{}:{:?}",
                check.id, check.reason, check.resolution
            )
        })
        .collect())
}

fn persist_contract(repo: &Path, contract: &VerificationContract) -> MedusaResult<()> {
    let path = contract_path(repo, &contract.session_id);
    let parent = path
        .parent()
        .ok_or_else(|| invalid("verification contract path has no parent"))?;
    fs::create_dir_all(parent)?;
    fs::write(path, serde_json::to_vec_pretty(contract)?)?;
    Ok(())
}

fn load_contract(repo: &Path, session_id: &str) -> MedusaResult<Option<VerificationContract>> {
    let path = contract_path(repo, session_id);
    if !path.is_file() {
        return Ok(None);
    }
    let contract: VerificationContract = serde_json::from_slice(&fs::read(path)?)?;
    if contract.schema_version != CONTRACT_SCHEMA_VERSION || contract.session_id != session_id {
        return Err(invalid("verification contract is stale or corrupted"));
    }
    Ok(Some(contract))
}

fn contract_path(repo: &Path, session_id: &str) -> PathBuf {
    repo.join(".medusa/verification-contracts")
        .join(format!("{session_id}.json"))
}

pub(crate) fn workspace_changed_paths(repo: &Path) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    for args in [
        vec!["diff", "--name-only", "--relative", "HEAD", "--"],
        vec!["ls-files", "--others", "--exclude-standard"],
    ] {
        let Ok(output) = hidden_command("git").args(&args).current_dir(repo).output() else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let path = line.trim().replace('\\', "/");
            if !path.is_empty() && !path.starts_with(".medusa/") {
                paths.insert(path);
            }
        }
    }
    paths
}

pub(crate) fn workspace_identity(
    repo: &Path,
    changed_paths: &BTreeSet<String>,
) -> MedusaResult<RepositoryStateIdentity> {
    let head = git_stdout(repo, &["rev-parse", "HEAD"]).unwrap_or_else(|| "worktree".to_owned());
    Ok(RepositoryStateIdentity {
        head,
        workspace_fingerprint: scope_fingerprint(repo, changed_paths)?,
    })
}

fn scope_fingerprint(repo: &Path, paths: &BTreeSet<String>) -> MedusaResult<String> {
    let mut hasher = Sha256::new();
    for relative in paths {
        hash_part(&mut hasher, relative.as_bytes());
        let path = repo.join(relative);
        match fs::read(&path) {
            Ok(bytes) => {
                hash_part(&mut hasher, b"file");
                hash_part(&mut hasher, &bytes);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                hash_part(&mut hasher, b"missing");
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(hex::encode(hasher.finalize()))
}

fn components_for(repo: &Path, paths: &BTreeSet<String>) -> MedusaResult<Vec<ChangedComponent>> {
    paths
        .iter()
        .map(|path| {
            let kind = if repo.join(path).exists() {
                ChangeKind::Modified
            } else {
                ChangeKind::Deleted
            };
            ChangedComponent::new(kind, path.clone()).map_err(evidence_error)
        })
        .collect()
}

fn check_scope(check: &VerificationCheck, changed_paths: &BTreeSet<String>) -> BTreeSet<String> {
    let root = check.working_directory.trim().trim_end_matches('/');
    if root.is_empty() || root == "." {
        return changed_paths.clone();
    }
    let prefix = format!("{root}/");
    let scoped = changed_paths
        .iter()
        .filter(|path| *path == root || path.starts_with(&prefix))
        .cloned()
        .collect::<BTreeSet<_>>();
    if scoped.is_empty() {
        changed_paths.clone()
    } else {
        scoped
    }
}

fn paths_overlap(left: &BTreeSet<String>, right: &BTreeSet<String>) -> bool {
    left.iter().any(|path| right.contains(path))
}

#[derive(Deserialize)]
struct PlatformPolicy {
    #[serde(default)]
    required: Vec<String>,
}

fn platform_policy_checks(
    repo: &Path,
    changed_paths: &BTreeSet<String>,
) -> MedusaResult<Vec<ContractCheck>> {
    let path = repo.join(".medusa/verification-platforms.json");
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let policy: PlatformPolicy = serde_json::from_slice(&fs::read(path)?)?;
    let current = current_platform();
    let mut checks = Vec::new();
    for platform in policy.required {
        let platform = platform.trim().to_ascii_lowercase();
        if platform.is_empty() {
            continue;
        }
        let id = format!(
            "platform-{}",
            &hex::encode(Sha256::digest(platform.as_bytes()))[..24]
        );
        checks.push(ContractCheck {
            id,
            kind: VerificationCheckKind::RepositoryDefined,
            requirement: VerificationRequirement::Required,
            resolution: if platform == current {
                VerificationResolution::Pending
            } else {
                VerificationResolution::Unavailable
            },
            reason: format!("repository policy requires authoritative {platform} verification"),
            scope_paths: changed_paths.clone(),
            platform: Some(platform),
            receipt_scope_fingerprint: None,
        });
    }
    Ok(checks)
}

fn current_platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

fn parse_check_summary(line: &str) -> Option<(String, bool)> {
    let payload = line.strip_prefix("verification_check=")?;
    let mut parts = payload.rsplitn(3, ':');
    let passed = parts.next()?.parse::<bool>().ok()?;
    let id = parts.next()?.to_owned();
    let _kind = parts.next()?;
    Some((id, passed))
}

fn repository_fingerprint(repo: &Path, head: &str) -> String {
    let canonical = repo
        .canonicalize()
        .unwrap_or_else(|_| repo.to_path_buf())
        .to_string_lossy()
        .into_owned();
    hex::encode(Sha256::digest(format!("{canonical}:{head}").as_bytes()))
}

fn git_stdout(repo: &Path, args: &[&str]) -> Option<String> {
    let output = hidden_command("git")
        .args(args)
        .current_dir(repo)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn hash_part(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn evidence_error(error: medusa_evidence::EvidenceError) -> MedusaError {
    MedusaError::new(
        ErrorCode::InternalInvariant,
        ErrorCategory::Internal,
        error.to_string(),
    )
}

fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(repo: &Path, args: &[&str]) {
        let status = hidden_command("git")
            .args(args)
            .current_dir(repo)
            .status()
            .expect("git");
        assert!(status.success());
    }

    fn repository() -> tempfile::TempDir {
        let directory = tempfile::tempdir().expect("tempdir");
        git(directory.path(), &["init", "-q"]);
        git(
            directory.path(),
            &["config", "user.email", "medusa@example.invalid"],
        );
        git(directory.path(), &["config", "user.name", "Medusa Test"]);
        fs::write(directory.path().join("a.txt"), "a0\n").expect("a");
        fs::write(directory.path().join("b.txt"), "b0\n").expect("b");
        git(directory.path(), &["add", "."]);
        git(directory.path(), &["commit", "-qm", "fixture"]);
        directory
    }

    fn check(id: &str, scope: &[&str]) -> ContractCheck {
        ContractCheck {
            id: id.to_owned(),
            kind: VerificationCheckKind::Unit,
            requirement: VerificationRequirement::Required,
            resolution: VerificationResolution::Pending,
            reason: "fixture".to_owned(),
            scope_paths: scope.iter().map(|path| (*path).to_owned()).collect(),
            platform: None,
            receipt_scope_fingerprint: None,
        }
    }

    #[test]
    fn overlapping_edit_stales_only_affected_receipts() {
        let directory = repository();
        let repo = directory.path();
        let paths = BTreeSet::from(["a.txt".to_owned(), "b.txt".to_owned()]);
        let identity = workspace_identity(repo, &paths).expect("identity");
        let mut contract = VerificationContract {
            schema_version: 1,
            session_id: "session".to_owned(),
            changed_paths: paths,
            checks: vec![check("a", &["a.txt"]), check("b", &["b.txt"])],
            constraints: Vec::new(),
            budget: VerificationBudgetReservation::for_output_budget(4096),
            planned_identity: identity,
            completion_identity: None,
            authoritative_passed: false,
        };
        contract
            .record_check_result(repo, "a", true)
            .expect("a pass");
        contract
            .record_check_result(repo, "b", true)
            .expect("b pass");
        fs::write(repo.join("a.txt"), "a1\n").expect("edit a");
        let affected = BTreeSet::from(["a.txt".to_owned()]);
        let old_b = contract
            .checks
            .iter()
            .find(|check| check.id == "b")
            .expect("b")
            .receipt_scope_fingerprint
            .clone();
        for check in &mut contract.checks {
            if paths_overlap(&check.scope_paths, &affected) {
                check.resolution = VerificationResolution::Stale;
                check.receipt_scope_fingerprint = None;
            }
        }
        assert_eq!(contract.checks[0].resolution, VerificationResolution::Stale);
        assert_eq!(
            contract.checks[1].resolution,
            VerificationResolution::Passed
        );
        assert_eq!(contract.checks[1].receipt_scope_fingerprint, old_b);
    }

    #[test]
    fn completion_identity_rejects_later_workspace_change() {
        let directory = repository();
        let repo = directory.path();
        let paths = BTreeSet::from(["a.txt".to_owned()]);
        let identity = workspace_identity(repo, &paths).expect("identity");
        let mut contract = VerificationContract {
            schema_version: 1,
            session_id: "session".to_owned(),
            changed_paths: paths.clone(),
            checks: vec![check("a", &["a.txt"])],
            constraints: Vec::new(),
            budget: VerificationBudgetReservation::for_output_budget(4096),
            planned_identity: identity.clone(),
            completion_identity: Some(identity),
            authoritative_passed: true,
        };
        contract.record_check_result(repo, "a", true).expect("pass");
        contract.authoritative_passed = true;
        contract.completion_identity = Some(workspace_identity(repo, &paths).expect("final"));
        assert!(contract.completion_ready(repo).expect("ready"));
        fs::write(repo.join("a.txt"), "a2\n").expect("edit");
        assert!(!contract.completion_ready(repo).expect("stale"));
    }

    #[test]
    fn unavailable_required_platform_is_explicit_and_blocks_completion() {
        let directory = repository();
        let repo = directory.path();
        fs::create_dir_all(repo.join(".medusa")).expect("medusa");
        let unavailable = if current_platform() == "windows" {
            "linux"
        } else {
            "windows"
        };
        fs::write(
            repo.join(".medusa/verification-platforms.json"),
            format!(
                "{{\"required\":[\"{}\",\"{unavailable}\"]}}",
                current_platform()
            ),
        )
        .expect("policy");
        fs::write(repo.join("a.txt"), "a1\n").expect("edit");
        let contract = VerificationContract::derive(
            repo,
            "session",
            BTreeSet::from(["a.txt".to_owned()]),
            4096,
        )
        .expect("contract");
        assert!(contract.checks.iter().any(|check| {
            check.platform.as_deref() == Some(unavailable)
                && check.resolution == VerificationResolution::Unavailable
        }));
        assert!(!contract.unresolved_required().is_empty());
    }

    #[test]
    fn budget_reservation_survives_nonessential_pressure() {
        let reservation = VerificationBudgetReservation::for_output_budget(4096);
        assert!(reservation.reserved_output_tokens >= MIN_RESERVED_OUTPUT_TOKENS);
        assert_eq!(
            4096_u32.saturating_sub(reservation.reserved_output_tokens)
                + reservation.reserved_output_tokens,
            4096
        );
    }

    #[test]
    fn reduced_scope_is_recorded_as_explicit_user_constraint() {
        let directory = repository();
        let repo = directory.path();
        fs::write(repo.join("a.txt"), "a1\n").expect("edit");
        let mut contract = VerificationContract::derive(
            repo,
            "session",
            BTreeSet::from(["a.txt".to_owned()]),
            4096,
        )
        .expect("contract");
        contract.record_user_reduced_scope(
            vec!["unit".to_owned()],
            "user explicitly requested unit-only verification",
        );
        assert_eq!(contract.constraints.len(), 1);
        assert_eq!(contract.constraints[0].source, "user");
    }

    #[test]
    fn focused_pass_does_not_override_other_required_failure() {
        let directory = repository();
        let repo = directory.path();
        let paths = BTreeSet::from(["a.txt".to_owned()]);
        let identity = workspace_identity(repo, &paths).expect("identity");
        let mut contract = VerificationContract {
            schema_version: 1,
            session_id: "session".to_owned(),
            changed_paths: paths.clone(),
            checks: vec![check("focused", &["a.txt"]), check("workspace", &["a.txt"])],
            constraints: Vec::new(),
            budget: VerificationBudgetReservation::for_output_budget(4096),
            planned_identity: identity,
            completion_identity: None,
            authoritative_passed: false,
        };
        contract
            .record_check_result(repo, "focused", true)
            .expect("focused pass");
        contract
            .record_check_result(repo, "workspace", false)
            .expect("workspace fail");
        contract.authoritative_passed = false;
        assert!(!contract.completion_ready(repo).expect("not ready"));
    }

    #[test]
    fn explicit_terminal_state_transitions_are_typed_and_auditable() {
        let directory = repository();
        let repo = directory.path();
        let paths = BTreeSet::from(["a.txt".to_owned()]);
        let identity = workspace_identity(repo, &paths).expect("identity");
        let mut contract = VerificationContract {
            schema_version: 1,
            session_id: "session".to_owned(),
            changed_paths: paths.clone(),
            checks: vec![
                check("waived", &["a.txt"]),
                check("superseded", &["a.txt"]),
                check("unavailable", &["a.txt"]),
            ],
            constraints: Vec::new(),
            budget: VerificationBudgetReservation::for_output_budget(4096),
            planned_identity: identity,
            completion_identity: None,
            authoritative_passed: false,
        };
        contract
            .waive_check("waived", "user explicitly accepted this verification gap")
            .expect("waive");
        contract.supersede_check("superseded").expect("supersede");
        contract
            .mark_unavailable("unavailable")
            .expect("unavailable");
        assert_eq!(
            contract
                .checks
                .iter()
                .find(|check| check.id == "waived")
                .expect("waived")
                .resolution,
            VerificationResolution::ExplicitlyWaived
        );
        assert_eq!(
            contract
                .checks
                .iter()
                .find(|check| check.id == "superseded")
                .expect("superseded")
                .resolution,
            VerificationResolution::Superseded
        );
        assert_eq!(
            contract
                .checks
                .iter()
                .find(|check| check.id == "unavailable")
                .expect("unavailable")
                .resolution,
            VerificationResolution::Unavailable
        );
        assert_eq!(contract.constraints.len(), 1);
        assert_eq!(contract.constraints[0].source, "explicit_waiver");
        assert!(
            contract
                .unresolved_required()
                .iter()
                .any(|check| check.id == "unavailable")
        );
    }
}
