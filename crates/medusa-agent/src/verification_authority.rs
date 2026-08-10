use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, UNIX_EPOCH},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_evidence::{ArtifactId, ChangeKind, ChangedComponent, VerificationReceipt};
use sha2::{Digest, Sha256};

use crate::{
    verification::VerificationResult,
    verification_dag::resource_pool::{
        WarmResourceKey, WarmResourceKind, WarmResourcePool, WarmResourcePoolLimits,
    },
};

#[path = "verification_cancellation.rs"]
pub(crate) mod verification_cancellation;
#[path = "verification_checkpoint.rs"]
pub(crate) mod verification_checkpoint;

use verification_cancellation::{
    VerificationRuntimeMetrics, register_verification_cancellation, take_runtime_metrics,
};
use verification_checkpoint::VerificationCheckpointStore;

#[allow(dead_code)]
#[path = "verification_authority_legacy.rs"]
mod legacy;

pub use legacy::{AuthoritativeVerificationResult, prepare_components_for_verification};

const MAX_STABLE_VERIFICATION_ATTEMPTS: usize = 3;
const STATE_FINGERPRINT_FILE: &str = "verification-state-fingerprint";
const STATE_WATCH_INTERVAL: Duration = Duration::from_millis(100);
const WARM_RESOURCE_TRUST_DOMAIN: &str = "local-authoritative-verification";

pub(crate) fn prepare_paths_for_verification(repo: &Path, paths: &[String]) -> MedusaResult<()> {
    legacy::prepare_paths_for_verification(repo, paths)
}

pub fn authoritative_verification_for_components(
    repo: &Path,
    repository_fingerprint: &str,
    commit: &str,
    components: &[ChangedComponent],
) -> MedusaResult<AuthoritativeVerificationResult> {
    authoritative_verification_for_components_at(
        repo,
        &repo.join(".medusa/evidence"),
        repository_fingerprint,
        commit,
        components,
    )
}

pub fn authoritative_verification_for_components_at(
    repo: &Path,
    evidence_root: &Path,
    repository_fingerprint: &str,
    commit: &str,
    components: &[ChangedComponent],
) -> MedusaResult<AuthoritativeVerificationResult> {
    // A repository-scoped verification authority owns one metrics lifecycle. Clear any
    // abandoned prior sample before execution, then always drain after the inner
    // authority returns so failed attempts cannot contaminate a later verification.
    let _ = take_runtime_metrics(repo);
    let authority_started = Instant::now();
    let initial_state = match complete_repository_state_fingerprint(repo, evidence_root, components)
    {
        Ok(state) => state,
        Err(error) => {
            let _ = take_runtime_metrics(repo);
            return Err(error);
        }
    };
    let warm_started = Instant::now();
    let warm_metrics = warm_authoritative_resources(
        repo,
        evidence_root,
        repository_fingerprint,
        commit,
        &initial_state,
    );
    let warm_setup_duration_ms = warm_started.elapsed().as_millis() as u64;
    let result = authoritative_verification_for_components_at_inner(
        repo,
        evidence_root,
        repository_fingerprint,
        commit,
        components,
        initial_state,
    );
    let metrics = take_runtime_metrics(repo);
    let authority_wall_duration_ms = authority_started.elapsed().as_millis() as u64;
    result.map(|mut execution| {
        let exact_reuse_hits = if execution
            .result
            .summary
            .iter()
            .any(|line| line == "verification_reuse=exact-persisted-receipt")
        {
            1
        } else {
            0
        };
        append_runtime_metrics(
            &mut execution.result.summary,
            &metrics,
            exact_reuse_hits,
            execution.reruns,
            authority_wall_duration_ms,
            warm_setup_duration_ms,
            &warm_metrics,
        );
        execution.result
    })
}

struct AuthorityExecution {
    result: AuthoritativeVerificationResult,
    reruns: u64,
}

fn authoritative_verification_for_components_at_inner(
    repo: &Path,
    evidence_root: &Path,
    repository_fingerprint: &str,
    commit: &str,
    components: &[ChangedComponent],
    initial_state: String,
) -> MedusaResult<AuthorityExecution> {
    fs::create_dir_all(evidence_root)?;
    let store_root = evidence_root.join(short_hash(&(repository_fingerprint.to_owned() + commit)));
    let mut reruns = 0u64;
    let mut first_state = Some(initial_state);

    for _ in 0..MAX_STABLE_VERIFICATION_ATTEMPTS {
        let before = match first_state.take() {
            Some(state) => state,
            None => complete_repository_state_fingerprint(repo, evidence_root, components)?,
        };
        if !persisted_receipt_safe(&store_root) || !state_marker_matches(&store_root, &before) {
            invalidate_persisted_reuse(&store_root)?;
        }

        let guarded =
            run_with_repository_state_guard(repo, evidence_root, components, &before, || {
                legacy::authoritative_verification_for_components_at(
                    repo,
                    evidence_root,
                    repository_fingerprint,
                    commit,
                    components,
                )
            })?;
        if guarded.cancelled {
            reruns = reruns.saturating_add(1);
            invalidate_persisted_reuse(&store_root)?;
            continue;
        }
        let result = guarded.result?;
        let after = complete_repository_state_fingerprint(repo, evidence_root, components)?;
        if before == after {
            fs::create_dir_all(&store_root)?;
            fs::write(store_root.join(STATE_FINGERPRINT_FILE), after)?;
            return Ok(AuthorityExecution { result, reruns });
        }

        reruns = reruns.saturating_add(1);
        invalidate_persisted_reuse(&store_root)?;
    }

    Err(invalid(
        "repository state changed repeatedly during authoritative verification",
    ))
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct WarmResourceMetrics {
    cache_hits: u64,
    cache_misses: u64,
    prepared: u64,
    reused_bytes: u64,
    prepared_bytes: u64,
    errors: u64,
    pruned: u64,
}

fn warm_authoritative_resources(
    repo: &Path,
    evidence_root: &Path,
    repository_fingerprint: &str,
    commit: &str,
    state_fingerprint: &str,
) -> WarmResourceMetrics {
    let mut metrics = WarmResourceMetrics::default();
    let pool = match WarmResourcePool::open(
        evidence_root.join("_warm-resources"),
        WarmResourcePoolLimits::default(),
    ) {
        Ok(pool) => pool,
        Err(_) => {
            metrics.errors = 1;
            return metrics;
        }
    };
    let branch_fingerprint = short_hash(
        &git_stdout(repo, &["rev-parse", "--abbrev-ref", "HEAD"])
            .unwrap_or_else(|| commit.to_owned()),
    );
    let trust_domain = std::env::var("MEDUSA_VERIFICATION_TRUST_DOMAIN")
        .unwrap_or_else(|_| WARM_RESOURCE_TRUST_DOMAIN.to_owned());
    let toolchain_fingerprint = verification_toolchain_fingerprint(repo);

    let key = |resource_kind| WarmResourceKey {
        repository_fingerprint: repository_fingerprint.to_owned(),
        branch_fingerprint: branch_fingerprint.clone(),
        trust_domain: trust_domain.clone(),
        resource_kind,
        input_fingerprint: state_fingerprint.to_owned(),
        toolchain_fingerprint: toolchain_fingerprint.clone(),
    };

    warm_bytes(
        &pool,
        key(WarmResourceKind::Worktree),
        state_fingerprint.as_bytes(),
        &mut metrics,
    );

    let dependency_descriptor = dependency_descriptor(repo);
    if !dependency_descriptor.is_empty() {
        warm_bytes(
            &pool,
            key(WarmResourceKind::DependencyCache),
            &dependency_descriptor,
            &mut metrics,
        );
    }

    if repo.join("Cargo.toml").is_file() {
        let build_key = key(WarmResourceKind::BuildMetadata);
        match pool.get(&build_key) {
            Ok(Some((receipt, _))) => {
                metrics.cache_hits = metrics.cache_hits.saturating_add(1);
                metrics.reused_bytes = metrics.reused_bytes.saturating_add(receipt.byte_len);
            }
            Ok(None) => {
                metrics.cache_misses = metrics.cache_misses.saturating_add(1);
                match Command::new(platform_program("cargo"))
                    .args(["metadata", "--format-version", "1", "--no-deps"])
                    .current_dir(repo)
                    .output()
                {
                    Ok(output) if output.status.success() && !output.stdout.is_empty() => {
                        match pool.put(build_key, &output.stdout) {
                            Ok(receipt) => {
                                metrics.prepared = metrics.prepared.saturating_add(1);
                                metrics.prepared_bytes =
                                    metrics.prepared_bytes.saturating_add(receipt.byte_len);
                            }
                            Err(_) => metrics.errors = metrics.errors.saturating_add(1),
                        }
                    }
                    Ok(_) | Err(_) => metrics.errors = metrics.errors.saturating_add(1),
                }
            }
            Err(_) => metrics.errors = metrics.errors.saturating_add(1),
        }
    }

    if let Ok(route) = std::env::var("MEDUSA_BROWSER_VERIFY_URL") {
        let browserd =
            std::env::var("MEDUSA_BROWSERD").unwrap_or_else(|_| "medusa-browserd".to_owned());
        let fixture = format!("route={route}\nbrowserd={browserd}\n");
        warm_bytes(
            &pool,
            key(WarmResourceKind::BrowserFixture),
            fixture.as_bytes(),
            &mut metrics,
        );
        warm_bytes(
            &pool,
            key(WarmResourceKind::Sidecar),
            browserd.as_bytes(),
            &mut metrics,
        );
    }

    match pool.prune() {
        Ok(pruned) => metrics.pruned = pruned as u64,
        Err(_) => metrics.errors = metrics.errors.saturating_add(1),
    }
    metrics
}

fn warm_bytes(
    pool: &WarmResourcePool,
    key: WarmResourceKey,
    bytes: &[u8],
    metrics: &mut WarmResourceMetrics,
) {
    match pool.get(&key) {
        Ok(Some((receipt, persisted))) if persisted == bytes => {
            metrics.cache_hits = metrics.cache_hits.saturating_add(1);
            metrics.reused_bytes = metrics.reused_bytes.saturating_add(receipt.byte_len);
        }
        Ok(Some(_)) | Ok(None) => {
            metrics.cache_misses = metrics.cache_misses.saturating_add(1);
            match pool.put(key, bytes) {
                Ok(receipt) => {
                    metrics.prepared = metrics.prepared.saturating_add(1);
                    metrics.prepared_bytes =
                        metrics.prepared_bytes.saturating_add(receipt.byte_len);
                }
                Err(_) => metrics.errors = metrics.errors.saturating_add(1),
            }
        }
        Err(_) => metrics.errors = metrics.errors.saturating_add(1),
    }
}

fn dependency_descriptor(repo: &Path) -> Vec<u8> {
    let mut descriptor = Vec::new();
    for name in [
        "Cargo.lock",
        "package-lock.json",
        "pnpm-lock.yaml",
        "yarn.lock",
        "bun.lock",
        "bun.lockb",
        "pyproject.toml",
        "uv.lock",
        "poetry.lock",
    ] {
        let path = repo.join(name);
        let Ok(bytes) = fs::read(path) else {
            continue;
        };
        descriptor.extend_from_slice(&(name.len() as u64).to_le_bytes());
        descriptor.extend_from_slice(name.as_bytes());
        descriptor.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        descriptor.extend_from_slice(&bytes);
    }
    descriptor
}

fn verification_toolchain_fingerprint(repo: &Path) -> String {
    Command::new(platform_program("rustc"))
        .arg("--version")
        .current_dir(repo)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| short_hash(&String::from_utf8_lossy(&output.stdout)))
        .unwrap_or_else(|| short_hash("rustc-unavailable"))
}

#[cfg(windows)]
fn platform_program(program: &str) -> &str {
    match program {
        "cargo" => "cargo.exe",
        "rustc" => "rustc.exe",
        _ => program,
    }
}

#[cfg(not(windows))]
fn platform_program(program: &str) -> &str {
    program
}

fn append_runtime_metrics(
    summary: &mut Vec<String>,
    metrics: &VerificationRuntimeMetrics,
    exact_reuse_hits: u64,
    authority_reruns: u64,
    authority_wall_duration_ms: u64,
    warm_setup_duration_ms: u64,
    warm_metrics: &WarmResourceMetrics,
) {
    let non_command_wall_duration_ms = authority_wall_duration_ms.saturating_sub(
        metrics
            .command_wall_duration_ms
            .min(authority_wall_duration_ms),
    );
    let serial_recorded_work_ms = metrics
        .command_serial_execution_ms
        .saturating_add(non_command_wall_duration_ms);
    let total_overlap_ms = serial_recorded_work_ms.saturating_sub(authority_wall_duration_ms);
    let critical_path_contribution_milli = if serial_recorded_work_ms == 0 {
        0
    } else {
        authority_wall_duration_ms
            .saturating_mul(1_000)
            .checked_div(serial_recorded_work_ms)
            .unwrap_or(0)
            .min(1_000)
    };
    summary.extend([
        format!("verification_command_waves={}", metrics.command_waves),
        format!(
            "verification_command_checks_executed={}",
            metrics.command_checks_executed
        ),
        format!(
            "verification_command_queue_duration_ms={}",
            metrics.command_queue_duration_ms
        ),
        format!(
            "verification_command_serial_execution_ms={}",
            metrics.command_serial_execution_ms
        ),
        format!(
            "verification_command_wall_duration_ms={}",
            metrics.command_wall_duration_ms
        ),
        format!(
            "verification_command_overlap_ms={}",
            metrics.command_overlap_ms
        ),
        format!("verification_exact_reuse_hits={exact_reuse_hits}"),
        format!("verification_authority_reruns={authority_reruns}"),
        format!("verification_authority_wall_duration_ms={authority_wall_duration_ms}"),
        format!("verification_non_command_wall_duration_ms={non_command_wall_duration_ms}"),
        format!("verification_total_overlap_ms={total_overlap_ms}"),
        format!("verification_critical_path_duration_ms={authority_wall_duration_ms}"),
        format!("verification_critical_path_contribution_milli={critical_path_contribution_milli}"),
        format!("verification_warm_setup_duration_ms={warm_setup_duration_ms}"),
        format!(
            "verification_warm_resource_hits={}",
            warm_metrics.cache_hits
        ),
        format!(
            "verification_warm_resource_misses={}",
            warm_metrics.cache_misses
        ),
        format!(
            "verification_warm_resource_prepared={}",
            warm_metrics.prepared
        ),
        format!(
            "verification_warm_resource_reused_bytes={}",
            warm_metrics.reused_bytes
        ),
        format!(
            "verification_warm_resource_prepared_bytes={}",
            warm_metrics.prepared_bytes
        ),
        format!("verification_warm_resource_errors={}", warm_metrics.errors),
        format!("verification_warm_resource_pruned={}", warm_metrics.pruned),
    ]);
}

struct GuardedVerification<T> {
    result: T,
    cancelled: bool,
}

fn run_with_repository_state_guard<T>(
    repo: &Path,
    evidence_root: &Path,
    components: &[ChangedComponent],
    expected: &str,
    operation: impl FnOnce() -> T,
) -> MedusaResult<GuardedVerification<T>> {
    let registration = register_verification_cancellation(repo);
    let cancellation = registration.token();
    let stop = Arc::new(AtomicBool::new(false));
    let watch_paths = filtered_repository_state_paths(repo, evidence_root, components)?;
    let initial_watch_signature =
        repository_watch_signature(repo, evidence_root, components, &watch_paths)?;

    thread::scope(|scope| {
        let watcher_cancellation = Arc::clone(&cancellation);
        let watcher_stop = Arc::clone(&stop);
        let watcher = scope.spawn(move || {
            let mut watch_signature = initial_watch_signature;
            while !watcher_stop.load(Ordering::Acquire) {
                thread::park_timeout(STATE_WATCH_INTERVAL);
                if watcher_stop.load(Ordering::Acquire) {
                    break;
                }
                let current_signature =
                    match repository_watch_signature(repo, evidence_root, components, &watch_paths)
                    {
                        Ok(signature) => signature,
                        Err(_) => {
                            watcher_cancellation.store(true, Ordering::Release);
                            break;
                        }
                    };
                if current_signature == watch_signature {
                    continue;
                }
                match complete_repository_state_fingerprint(repo, evidence_root, components) {
                    Ok(current) if current == expected => {
                        watch_signature = current_signature;
                    }
                    Ok(_) | Err(_) => {
                        watcher_cancellation.store(true, Ordering::Release);
                        break;
                    }
                }
            }
        });

        let result = operation();
        stop.store(true, Ordering::Release);
        watcher.thread().unpark();
        let _ = watcher.join();
        Ok(GuardedVerification {
            result,
            cancelled: cancellation.load(Ordering::Acquire),
        })
    })
}

fn repository_watch_signature(
    repo: &Path,
    evidence_root: &Path,
    components: &[ChangedComponent],
    watch_paths: &[PathBuf],
) -> MedusaResult<String> {
    let current_paths = filtered_repository_state_paths(repo, evidence_root, components)?;
    let mut hasher = Sha256::new();
    for relative in current_paths {
        hash_repository_state_entry(
            &mut hasher,
            relative.to_string_lossy().as_bytes(),
            b"path",
            b"",
        );
    }

    for relative in watch_paths {
        let path = repo.join(relative);
        let relative_bytes = relative.to_string_lossy();
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let modified = metadata_timestamp(&metadata);
                match fs::read_link(&path) {
                    Ok(target) => {
                        let mut payload = modified.to_le_bytes().to_vec();
                        payload.extend_from_slice(target.to_string_lossy().as_bytes());
                        hash_repository_state_entry(
                            &mut hasher,
                            relative_bytes.as_bytes(),
                            b"symlink",
                            &payload,
                        );
                    }
                    Err(_) => hash_repository_state_entry(
                        &mut hasher,
                        relative_bytes.as_bytes(),
                        b"symlink",
                        b"unreadable",
                    ),
                }
            }
            Ok(metadata) if metadata.is_file() => {
                let mut payload = metadata.len().to_le_bytes().to_vec();
                payload.extend_from_slice(&metadata_timestamp(&metadata).to_le_bytes());
                hash_repository_state_entry(
                    &mut hasher,
                    relative_bytes.as_bytes(),
                    b"file-metadata",
                    &payload,
                );
            }
            Ok(metadata) => {
                hash_repository_state_entry(
                    &mut hasher,
                    relative_bytes.as_bytes(),
                    b"other-metadata",
                    &metadata_timestamp(&metadata).to_le_bytes(),
                );
            }
            Err(_) => {
                hash_repository_state_entry(&mut hasher, relative_bytes.as_bytes(), b"missing", b"")
            }
        }
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn metadata_timestamp(metadata: &fs::Metadata) -> u128 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos())
}

fn state_marker_matches(store_root: &Path, expected: &str) -> bool {
    fs::read_to_string(store_root.join(STATE_FINGERPRINT_FILE))
        .is_ok_and(|actual| actual == expected)
}

fn persisted_receipt_safe(store_root: &Path) -> bool {
    let path = store_root.join("verification-receipt.json");
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return true,
        Err(_) => return false,
    };
    let receipt = match serde_json::from_slice::<VerificationReceipt>(&bytes) {
        Ok(receipt) => receipt,
        Err(_) => return false,
    };
    receipt
        .evidence
        .artifacts
        .iter()
        .all(|artifact| valid_cache_artifact_id(&artifact.id))
}

fn valid_cache_artifact_id(id: &ArtifactId) -> bool {
    id.0.strip_prefix("artifact-").is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn invalidate_persisted_reuse(store_root: &Path) -> MedusaResult<()> {
    VerificationCheckpointStore::new(store_root)
        .remove()
        .map_err(|error| {
            invalid(format!(
                "failed to invalidate verification checkpoint: {error}"
            ))
        })?;
    for name in [
        "verification-receipt.json",
        "verification-dag.json",
        STATE_FINGERPRINT_FILE,
    ] {
        match fs::remove_file(store_root.join(name)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn complete_repository_state_fingerprint(
    repo: &Path,
    evidence_root: &Path,
    components: &[ChangedComponent],
) -> MedusaResult<String> {
    let paths = filtered_repository_state_paths(repo, evidence_root, components)?;
    let mut hasher = Sha256::new();
    for relative in paths {
        let path = repo.join(&relative);
        let relative = relative.to_string_lossy();
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => match fs::read_link(&path) {
                Ok(target) => hash_repository_state_entry(
                    &mut hasher,
                    relative.as_bytes(),
                    b"symlink",
                    target.to_string_lossy().as_bytes(),
                ),
                Err(_) => hash_repository_state_entry(
                    &mut hasher,
                    relative.as_bytes(),
                    b"symlink",
                    b"unreadable",
                ),
            },
            Ok(metadata) if metadata.is_file() => match fs::read(&path) {
                Ok(bytes) => {
                    hash_repository_state_entry(&mut hasher, relative.as_bytes(), b"file", &bytes)
                }
                Err(_) => hash_repository_state_entry(
                    &mut hasher,
                    relative.as_bytes(),
                    b"file",
                    b"unreadable",
                ),
            },
            Ok(_) => hash_repository_state_entry(&mut hasher, relative.as_bytes(), b"other", b""),
            Err(_) => {
                hash_repository_state_entry(&mut hasher, relative.as_bytes(), b"missing", b"")
            }
        }
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn filtered_repository_state_paths(
    repo: &Path,
    evidence_root: &Path,
    components: &[ChangedComponent],
) -> MedusaResult<Vec<PathBuf>> {
    let mut paths = repository_state_paths(repo, components)?;
    paths.sort();
    paths.dedup();
    let repo_root = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    let evidence_root = evidence_root
        .canonicalize()
        .unwrap_or_else(|_| evidence_root.to_path_buf());
    paths.retain(|relative| {
        if relative.starts_with(".git") {
            return false;
        }
        let absolute = repo_root.join(relative);
        !absolute.starts_with(&evidence_root)
    });
    Ok(paths)
}

fn repository_state_paths(
    repo: &Path,
    components: &[ChangedComponent],
) -> MedusaResult<Vec<PathBuf>> {
    let mut result = if git_repository(repo) {
        let output = Command::new("git")
            .args([
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
            ])
            .current_dir(repo)
            .output()?;
        if output.status.success() {
            output
                .stdout
                .split(|byte| *byte == 0)
                .filter(|entry| !entry.is_empty())
                .map(|entry| PathBuf::from(String::from_utf8_lossy(entry).into_owned()))
                .collect()
        } else {
            collect_repository_paths(repo)?
        }
    } else {
        collect_repository_paths(repo)?
    };

    for component in components {
        result.extend(component.all_paths().into_iter().map(PathBuf::from));
    }
    Ok(result)
}

fn collect_repository_paths(repo: &Path) -> MedusaResult<Vec<PathBuf>> {
    fn collect(root: &Path, directory: &Path, result: &mut Vec<PathBuf>) -> MedusaResult<()> {
        let mut entries = fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            if relative.starts_with(".git") {
                continue;
            }
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                collect(root, &path, result)?;
            } else {
                result.push(relative);
            }
        }
        Ok(())
    }

    let mut result = Vec::new();
    collect(repo, repo, &mut result)?;
    Ok(result)
}

fn hash_repository_state_entry(hasher: &mut Sha256, path: &[u8], kind: &[u8], payload: &[u8]) {
    for part in [path, kind, payload] {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
}

fn git_repository(repo: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(repo)
        .output()
        .is_ok_and(|output| output.status.success() && output.stdout.starts_with(b"true"))
}

fn git_stdout(repo: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn short_hash(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
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

pub(crate) fn authoritative_verification_for_paths(
    repo: &Path,
    paths: &[String],
) -> MedusaResult<VerificationResult> {
    let components = paths
        .iter()
        .map(|path| ChangedComponent::new(ChangeKind::Modified, path.clone()))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(evidence_error)?;
    let commit = git_stdout(repo, &["rev-parse", "HEAD"]).unwrap_or_else(|| "worktree".to_owned());
    let repository_fingerprint = short_hash(&format!("{}:{commit}", repo.display()));
    let result = authoritative_verification_for_components(
        repo,
        &repository_fingerprint,
        &commit,
        &components,
    )?;
    Ok(VerificationResult {
        passed: result.receipt.passed,
        evidence: result.summary,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::verification_authority::verification_cancellation::{
        active_verification_cancellation, record_command_wave,
    };

    #[test]
    fn explicit_ignored_component_changes_repository_state_fingerprint() {
        let directory = tempfile::tempdir().expect("repository");
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .args(args)
                .current_dir(directory.path())
                .status()
                .expect("git");
            assert!(status.success());
        };
        git(&["init", "-q"]);
        fs::write(directory.path().join(".gitignore"), "artifact.json\n").expect("gitignore");
        fs::write(directory.path().join("artifact.json"), "{\"ok\":true}\n").expect("artifact");
        let component =
            ChangedComponent::new(ChangeKind::Modified, "artifact.json").expect("component");
        let evidence_root = directory.path().join(".medusa/evidence");
        fs::create_dir_all(&evidence_root).expect("evidence root");

        let before = complete_repository_state_fingerprint(
            directory.path(),
            &evidence_root,
            &[component.clone()],
        )
        .expect("before fingerprint");
        fs::write(directory.path().join("artifact.json"), "{broken}\n").expect("mutate");
        let after =
            complete_repository_state_fingerprint(directory.path(), &evidence_root, &[component])
                .expect("after fingerprint");

        assert_ne!(before, after);
    }

    #[test]
    fn active_state_guard_cancels_when_repository_changes() {
        let directory = tempfile::tempdir().expect("repository");
        fs::write(directory.path().join("input.txt"), "before\n").expect("input");
        let component =
            ChangedComponent::new(ChangeKind::Modified, "input.txt").expect("component");
        let evidence_root = directory.path().join(".medusa/evidence");
        fs::create_dir_all(&evidence_root).expect("evidence root");
        let before = complete_repository_state_fingerprint(
            directory.path(),
            &evidence_root,
            &[component.clone()],
        )
        .expect("before fingerprint");
        let input = directory.path().join("input.txt");

        let guarded = run_with_repository_state_guard(
            directory.path(),
            &evidence_root,
            &[component],
            &before,
            || {
                let cancellation = active_verification_cancellation(directory.path())
                    .expect("active cancellation token");
                let writer = thread::spawn(move || {
                    thread::sleep(Duration::from_millis(150));
                    fs::write(input, "after\n").expect("mutate repository");
                });
                let deadline = Instant::now() + Duration::from_secs(5);
                while !cancellation.load(Ordering::Acquire) && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(10));
                }
                writer.join().expect("writer");
                cancellation.load(Ordering::Acquire)
            },
        )
        .expect("guard");

        assert!(guarded.cancelled);
        assert!(guarded.result);
    }

    #[test]
    fn metadata_only_change_is_confirmed_before_cancellation() {
        let directory = tempfile::tempdir().expect("repository");
        fs::write(directory.path().join("input.txt"), "stable\n").expect("input");
        let component =
            ChangedComponent::new(ChangeKind::Modified, "input.txt").expect("component");
        let evidence_root = directory.path().join(".medusa/evidence");
        fs::create_dir_all(&evidence_root).expect("evidence root");
        let before = complete_repository_state_fingerprint(
            directory.path(),
            &evidence_root,
            &[component.clone()],
        )
        .expect("before fingerprint");
        let input = directory.path().join("input.txt");

        let guarded = run_with_repository_state_guard(
            directory.path(),
            &evidence_root,
            &[component],
            &before,
            || {
                let cancellation = active_verification_cancellation(directory.path())
                    .expect("active cancellation token");
                thread::sleep(Duration::from_millis(150));
                fs::write(input, "stable\n").expect("rewrite same content");
                thread::sleep(Duration::from_millis(350));
                cancellation.load(Ordering::Acquire)
            },
        )
        .expect("guard");

        assert!(!guarded.cancelled);
        assert!(!guarded.result);
    }

    #[test]
    fn cache_artifact_ids_reject_path_traversal() {
        assert!(!valid_cache_artifact_id(&ArtifactId(
            "artifact-../../../../../tmp/victim".to_owned()
        )));
        assert!(valid_cache_artifact_id(&ArtifactId(format!(
            "artifact-{}",
            "a".repeat(64)
        ))));
    }

    #[test]
    fn runtime_metrics_render_in_authoritative_summary_format() {
        let metrics = VerificationRuntimeMetrics {
            command_waves: 2,
            command_checks_executed: 5,
            command_queue_duration_ms: 7,
            command_serial_execution_ms: 120,
            command_wall_duration_ms: 70,
            command_overlap_ms: 50,
        };
        let warm = WarmResourceMetrics {
            cache_hits: 3,
            cache_misses: 2,
            prepared: 2,
            reused_bytes: 80,
            prepared_bytes: 40,
            errors: 0,
            pruned: 1,
        };
        let mut summary = Vec::new();
        append_runtime_metrics(&mut summary, &metrics, 1, 3, 100, 5, &warm);
        assert_eq!(
            summary,
            vec![
                "verification_command_waves=2",
                "verification_command_checks_executed=5",
                "verification_command_queue_duration_ms=7",
                "verification_command_serial_execution_ms=120",
                "verification_command_wall_duration_ms=70",
                "verification_command_overlap_ms=50",
                "verification_exact_reuse_hits=1",
                "verification_authority_reruns=3",
                "verification_authority_wall_duration_ms=100",
                "verification_non_command_wall_duration_ms=30",
                "verification_total_overlap_ms=50",
                "verification_critical_path_duration_ms=100",
                "verification_critical_path_contribution_milli=666",
                "verification_warm_setup_duration_ms=5",
                "verification_warm_resource_hits=3",
                "verification_warm_resource_misses=2",
                "verification_warm_resource_prepared=2",
                "verification_warm_resource_reused_bytes=80",
                "verification_warm_resource_prepared_bytes=40",
                "verification_warm_resource_errors=0",
                "verification_warm_resource_pruned=1",
            ]
        );
    }

    #[test]
    fn warm_resource_setup_reuses_only_exact_repository_state() {
        let directory = tempfile::tempdir().expect("repository");
        let evidence = tempfile::tempdir().expect("evidence");
        fs::write(directory.path().join("artifact.json"), "{\"ok\":true}\n").expect("artifact");
        let component =
            ChangedComponent::new(ChangeKind::Modified, "artifact.json").expect("component");
        let before = complete_repository_state_fingerprint(
            directory.path(),
            evidence.path(),
            &[component.clone()],
        )
        .expect("state");

        let first = warm_authoritative_resources(
            directory.path(),
            evidence.path(),
            "repo",
            "commit",
            &before,
        );
        assert!(first.cache_misses >= 1);
        assert!(first.prepared >= 1);
        assert_eq!(first.errors, 0);

        let second = warm_authoritative_resources(
            directory.path(),
            evidence.path(),
            "repo",
            "commit",
            &before,
        );
        assert!(second.cache_hits >= 1);
        assert_eq!(second.errors, 0);

        fs::write(directory.path().join("artifact.json"), "{\"ok\":false}\n").expect("mutate");
        let after =
            complete_repository_state_fingerprint(directory.path(), evidence.path(), &[component])
                .expect("changed state");
        assert_ne!(before, after);
        let changed = warm_authoritative_resources(
            directory.path(),
            evidence.path(),
            "repo",
            "commit",
            &after,
        );
        assert!(changed.cache_misses >= 1);
        assert_eq!(changed.errors, 0);
    }

    #[test]
    fn outer_authority_resets_stale_runtime_metrics() {
        let directory = tempfile::tempdir().expect("repository");
        fs::write(directory.path().join("artifact.json"), "{\"ok\":true}\n").expect("artifact");
        let component =
            ChangedComponent::new(ChangeKind::Modified, "artifact.json").expect("component");
        let evidence_root = directory.path().join("durable-evidence");
        record_command_wave(directory.path(), 1, 4, 10, 8);

        let result = authoritative_verification_for_components_at(
            directory.path(),
            &evidence_root,
            "repo",
            "commit",
            &[component],
        )
        .expect("verification");

        assert!(
            result
                .summary
                .iter()
                .any(|line| line == "verification_command_waves=0")
        );
        assert!(
            result
                .summary
                .iter()
                .any(|line| line == "verification_exact_reuse_hits=0")
        );
        assert!(
            result
                .summary
                .iter()
                .any(|line| line == "verification_authority_reruns=0")
        );
        assert!(
            result
                .summary
                .iter()
                .any(|line| line.starts_with("verification_critical_path_duration_ms="))
        );
        assert_eq!(
            take_runtime_metrics(directory.path()),
            VerificationRuntimeMetrics::default()
        );
    }

    #[test]
    fn outer_state_guard_reuses_only_stable_semantic_inputs() {
        let directory = tempfile::tempdir().expect("repository");
        fs::write(directory.path().join("artifact.json"), "{\"ok\":true}\n").expect("artifact");
        let component =
            ChangedComponent::new(ChangeKind::Modified, "artifact.json").expect("component");
        let evidence_root = directory.path().join("durable-evidence");

        let first = authoritative_verification_for_components_at(
            directory.path(),
            &evidence_root,
            "repo",
            "commit",
            &[component.clone()],
        )
        .expect("first verification");
        assert!(first.receipt.passed);
        assert!(
            first
                .summary
                .iter()
                .any(|line| line == "verification_exact_reuse_hits=0")
        );

        let reused = authoritative_verification_for_components_at(
            directory.path(),
            &evidence_root,
            "repo",
            "commit",
            &[component.clone()],
        )
        .expect("reuse");
        assert!(
            reused
                .summary
                .iter()
                .any(|line| line == "verification_reuse=exact-persisted-receipt")
        );
        assert!(
            reused
                .summary
                .iter()
                .any(|line| line == "verification_exact_reuse_hits=1")
        );
        assert!(
            reused
                .summary
                .iter()
                .any(|line| line == "verification_authority_reruns=0")
        );
        assert!(
            reused
                .summary
                .iter()
                .any(|line| line.starts_with("verification_warm_resource_hits="))
        );

        fs::write(directory.path().join("artifact.json"), "{broken}\n").expect("mutate");
        let changed = authoritative_verification_for_components_at(
            directory.path(),
            &evidence_root,
            "repo",
            "commit",
            &[component],
        )
        .expect("changed verification");
        assert!(!changed.receipt.passed);
        assert!(
            !changed
                .summary
                .iter()
                .any(|line| line == "verification_reuse=exact-persisted-receipt")
        );
        assert!(
            changed
                .summary
                .iter()
                .any(|line| line == "verification_exact_reuse_hits=0")
        );
    }
}
