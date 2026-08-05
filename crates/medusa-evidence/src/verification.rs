use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::change::{package_owners, requires_artifact_semantics, requires_security};
use crate::{
    ArtifactId, ChangedComponent, EvidenceBundle, EvidenceError, EvidenceId, Result,
    SCHEMA_VERSION, changed_scope_fingerprint, fingerprint, normalize_components,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationCheckKind {
    Format,
    Lint,
    Typecheck,
    Unit,
    Integration,
    Build,
    BrowserBehavior,
    Accessibility,
    Packaging,
    Security,
    ArtifactSemantic,
    RepositoryDefined,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VerificationCheck {
    pub id: String,
    pub kind: VerificationCheckKind,
    pub program: Option<String>,
    pub args: Vec<String>,
    pub working_directory: String,
    pub required: bool,
    pub reason: String,
    pub input_fingerprint: String,
}

impl VerificationCheck {
    pub fn command(
        kind: VerificationCheckKind,
        program: &str,
        args: &[&str],
        working_directory: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self::new(kind, Some(program), args, working_directory, reason)
    }

    pub fn behavior(kind: VerificationCheckKind, reason: impl Into<String>) -> Self {
        Self::new(kind, None, &[], ".", reason)
    }

    fn new(
        kind: VerificationCheckKind,
        program: Option<&str>,
        args: &[&str],
        working_directory: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        let working_directory = working_directory.into();
        let reason = reason.into();
        let args = args
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>();
        let input_fingerprint = fingerprint(&(kind, program, &args, &working_directory, &reason));
        Self {
            id: format!("check-{}", &input_fingerprint[..24]),
            kind,
            program: program.map(str::to_owned),
            args,
            working_directory,
            required: true,
            reason,
            input_fingerprint,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VerificationExemption {
    pub kind: VerificationCheckKind,
    pub scope_fingerprint: String,
    pub reviewer: String,
    pub reason: String,
    pub fingerprint: String,
}

impl VerificationExemption {
    pub fn new(
        kind: VerificationCheckKind,
        scope_fingerprint: impl Into<String>,
        reviewer: impl Into<String>,
        reason: impl Into<String>,
    ) -> Result<Self> {
        let mut value = Self {
            kind,
            scope_fingerprint: scope_fingerprint.into(),
            reviewer: reviewer.into(),
            reason: reason.into(),
            fingerprint: String::new(),
        };
        value.fingerprint = exemption_fingerprint(&value);
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<()> {
        if self.scope_fingerprint.trim().is_empty()
            || self.reviewer.trim().is_empty()
            || self.reason.trim().is_empty()
            || self.fingerprint != exemption_fingerprint(self)
        {
            return Err(EvidenceError::Validation(
                "verification exemption is incomplete or corrupted".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VerificationPlan {
    pub schema_version: u16,
    pub repository_fingerprint: String,
    pub commit: String,
    pub components: Vec<ChangedComponent>,
    pub checks: Vec<VerificationCheck>,
    pub exemptions: Vec<VerificationExemption>,
    pub fingerprint: String,
}

pub struct VerificationPlanner;

impl VerificationPlanner {
    pub fn plan(
        repo: &Path,
        repository_fingerprint: impl Into<String>,
        commit: impl Into<String>,
        components: &[ChangedComponent],
        exemptions: &[VerificationExemption],
    ) -> Result<VerificationPlan> {
        let repository_fingerprint = repository_fingerprint.into();
        let commit = commit.into();
        if repository_fingerprint.trim().is_empty() || commit.trim().is_empty() {
            return Err(EvidenceError::Validation(
                "verification planning requires repository fingerprint and commit".to_owned(),
            ));
        }
        let components = normalize_components(repo, components)?;
        let scope = changed_scope_fingerprint(&components);
        for exemption in exemptions {
            exemption.validate()?;
            if exemption.scope_fingerprint != scope {
                return Err(EvidenceError::Validation(
                    "verification exemption is not bound to exact changed scope".to_owned(),
                ));
            }
        }
        let mut checks = repository_defined_checks(repo)?;
        add_manifest_checks(repo, &components, &mut checks)?;
        if components.iter().any(|component| component.effective_ui) {
            checks.push(VerificationCheck::behavior(
                VerificationCheckKind::BrowserBehavior,
                "effective UI change requires real browser behavior",
            ));
            checks.push(VerificationCheck::behavior(
                VerificationCheckKind::Accessibility,
                "effective UI change requires accessibility behavior",
            ));
        }
        if components.iter().any(requires_artifact_semantics) {
            checks.push(VerificationCheck::behavior(
                VerificationCheckKind::ArtifactSemantic,
                "generated or standalone artifacts require semantic validation",
            ));
        }
        if components.iter().any(requires_security) {
            add_security_checks(repo, &mut checks);
        }
        if checks.is_empty() {
            checks.push(VerificationCheck::behavior(
                VerificationCheckKind::ArtifactSemantic,
                "unrecognized repositories require semantic validation of changed files",
            ));
        }
        checks.sort_by(|left, right| left.id.cmp(&right.id));
        checks.dedup_by(|left, right| left.id == right.id);
        let exempted = exemptions
            .iter()
            .map(|value| value.kind)
            .collect::<BTreeSet<_>>();
        checks.retain(|check| !exempted.contains(&check.kind));
        let mut plan = VerificationPlan {
            schema_version: SCHEMA_VERSION,
            repository_fingerprint,
            commit,
            components,
            checks,
            exemptions: exemptions.to_vec(),
            fingerprint: String::new(),
        };
        plan.fingerprint = plan_fingerprint(&plan);
        plan.validate()?;
        Ok(plan)
    }
}

impl VerificationPlan {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION
            || self.repository_fingerprint.trim().is_empty()
            || self.commit.trim().is_empty()
            || self.components.is_empty()
            || self.checks.is_empty() && self.exemptions.is_empty()
            || self.fingerprint != plan_fingerprint(self)
        {
            return Err(EvidenceError::Validation(
                "verification plan is incomplete or corrupted".to_owned(),
            ));
        }
        let scope = changed_scope_fingerprint(&self.components);
        let mut ids = BTreeSet::new();
        for check in &self.checks {
            if !ids.insert(&check.id)
                || check.reason.trim().is_empty()
                || check.input_fingerprint
                    != fingerprint(&(
                        check.kind,
                        check.program.as_deref(),
                        &check.args,
                        &check.working_directory,
                        &check.reason,
                    ))
            {
                return Err(EvidenceError::Validation(
                    "verification check is duplicated or corrupted".to_owned(),
                ));
            }
        }
        for exemption in &self.exemptions {
            exemption.validate()?;
            if exemption.scope_fingerprint != scope {
                return Err(EvidenceError::Validation(
                    "verification exemption scope mismatch".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CommandReceipt {
    pub schema_version: u16,
    pub id: String,
    pub check_id: String,
    pub command_hash: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration_ms: u64,
    pub stdout_artifact: ArtifactId,
    pub stderr_artifact: ArtifactId,
    pub passed: bool,
    pub fingerprint: String,
}

impl CommandReceipt {
    pub fn new(
        check: &VerificationCheck,
        exit_code: Option<i32>,
        timed_out: bool,
        duration_ms: u64,
        stdout_artifact: ArtifactId,
        stderr_artifact: ArtifactId,
    ) -> Self {
        let mut receipt = Self {
            schema_version: SCHEMA_VERSION,
            id: format!("command-{}", Ulid::new()),
            check_id: check.id.clone(),
            command_hash: check.input_fingerprint.clone(),
            exit_code,
            timed_out,
            duration_ms,
            stdout_artifact,
            stderr_artifact,
            passed: !timed_out && exit_code == Some(0),
            fingerprint: String::new(),
        };
        receipt.fingerprint = command_fingerprint(&receipt);
        receipt
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION
            || self.id.trim().is_empty()
            || self.check_id.trim().is_empty()
            || self.command_hash.trim().is_empty()
            || self.fingerprint != command_fingerprint(self)
        {
            return Err(EvidenceError::Validation(
                "command receipt is incomplete or corrupted".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VerificationCheckReceipt {
    pub check_id: String,
    pub kind: VerificationCheckKind,
    pub passed: bool,
    pub command: Option<CommandReceipt>,
    pub evidence_ids: Vec<EvidenceId>,
    pub artifact_ids: Vec<ArtifactId>,
    pub details: Vec<String>,
    pub fingerprint: String,
}

impl VerificationCheckReceipt {
    pub fn new(
        check: &VerificationCheck,
        passed: bool,
        command: Option<CommandReceipt>,
        evidence_ids: Vec<EvidenceId>,
        artifact_ids: Vec<ArtifactId>,
        details: Vec<String>,
    ) -> Self {
        let mut receipt = Self {
            check_id: check.id.clone(),
            kind: check.kind,
            passed,
            command,
            evidence_ids,
            artifact_ids,
            details,
            fingerprint: String::new(),
        };
        receipt.fingerprint = check_fingerprint(&receipt);
        receipt
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VerificationReceipt {
    pub schema_version: u16,
    pub plan: VerificationPlan,
    pub checks: Vec<VerificationCheckReceipt>,
    pub evidence: EvidenceBundle,
    pub passed: bool,
    pub coverage: Vec<String>,
    pub fingerprint: String,
}

impl VerificationReceipt {
    pub fn new(
        plan: VerificationPlan,
        mut checks: Vec<VerificationCheckReceipt>,
        mut evidence: EvidenceBundle,
    ) -> Result<Self> {
        checks.sort_by(|left, right| left.check_id.cmp(&right.check_id));
        evidence.refresh();
        let passed = checks.iter().all(|check| check.passed)
            && plan.checks.iter().all(|planned| {
                !planned.required || checks.iter().any(|receipt| receipt.check_id == planned.id)
            });
        let coverage = plan
            .components
            .iter()
            .map(|component| component.path.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut receipt = Self {
            schema_version: SCHEMA_VERSION,
            plan,
            checks,
            evidence,
            passed,
            coverage,
            fingerprint: String::new(),
        };
        receipt.fingerprint = receipt_fingerprint(&receipt);
        receipt.validate()?;
        Ok(receipt)
    }

    pub fn validate(&self) -> Result<()> {
        self.plan.validate()?;
        self.evidence.validate()?;
        if self.schema_version != SCHEMA_VERSION
            || self.evidence.repository_fingerprint != self.plan.repository_fingerprint
            || self.evidence.commit != self.plan.commit
            || self.fingerprint != receipt_fingerprint(self)
        {
            return Err(EvidenceError::Validation(
                "verification receipt is stale or corrupted".to_owned(),
            ));
        }
        let planned = self
            .plan
            .checks
            .iter()
            .map(|check| (check.id.as_str(), check))
            .collect::<BTreeMap<_, _>>();
        let mut seen = BTreeSet::new();
        for receipt in &self.checks {
            let check = planned.get(receipt.check_id.as_str()).ok_or_else(|| {
                EvidenceError::Validation("receipt contains unplanned check".to_owned())
            })?;
            if !seen.insert(&receipt.check_id)
                || receipt.kind != check.kind
                || receipt.fingerprint != check_fingerprint(receipt)
            {
                return Err(EvidenceError::Validation(
                    "check receipt is duplicated or corrupted".to_owned(),
                ));
            }
            if let Some(command) = &receipt.command {
                command.validate()?;
                if command.check_id != check.id
                    || command.command_hash != check.input_fingerprint
                    || command.passed != receipt.passed
                {
                    return Err(EvidenceError::Validation(
                        "command receipt does not match plan".to_owned(),
                    ));
                }
            }
            if receipt
                .evidence_ids
                .iter()
                .any(|id| !self.evidence.records.iter().any(|record| &record.id == id))
                || receipt.artifact_ids.iter().any(|id| {
                    !self
                        .evidence
                        .artifacts
                        .iter()
                        .any(|artifact| &artifact.id == id)
                })
            {
                return Err(EvidenceError::Validation(
                    "check references missing evidence or artifacts".to_owned(),
                ));
            }
        }
        if self
            .plan
            .checks
            .iter()
            .filter(|check| check.required)
            .any(|check| !seen.contains(&check.id))
        {
            return Err(EvidenceError::Validation(
                "required verification check has no receipt".to_owned(),
            ));
        }
        let expected = self
            .plan
            .components
            .iter()
            .map(|component| component.path.clone())
            .collect::<BTreeSet<_>>();
        if self.coverage.iter().cloned().collect::<BTreeSet<_>>() != expected
            || self.passed != self.checks.iter().all(|check| check.passed)
        {
            return Err(EvidenceError::Validation(
                "verification decision or coverage is incomplete".to_owned(),
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn summary_lines(&self) -> Vec<String> {
        let mut lines = vec![
            format!("verification_plan={}", self.plan.fingerprint),
            format!("verification_passed={}", self.passed),
            format!("verification_coverage={}", self.coverage.join(",")),
        ];
        for check in &self.checks {
            lines.push(format!(
                "verification_check={:?}:{}:{}",
                check.kind, check.check_id, check.passed
            ));
            lines.extend(check.details.iter().cloned());
        }
        lines
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtifactSemanticClass {
    Json,
    Html,
    Text,
    Png,
    Pdf,
    Zip,
    Binary,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactSemanticResult {
    pub class: ArtifactSemanticClass,
    pub passed: bool,
    pub details: Vec<String>,
}

pub fn validate_artifact_semantics(path: &Path) -> Result<ArtifactSemanticResult> {
    if !path.is_file() {
        return Ok(ArtifactSemanticResult {
            class: ArtifactSemanticClass::Binary,
            passed: false,
            details: vec![format!("missing_artifact={}", path.display())],
        });
    }
    let bytes = fs::read(path)?;
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let (class, passed, details) = match extension.as_str() {
        "json" => {
            let valid = serde_json::from_slice::<serde_json::Value>(&bytes).is_ok();
            (
                ArtifactSemanticClass::Json,
                valid,
                vec![format!("json_parseable={valid}")],
            )
        }
        "html" | "htm" => {
            let valid = std::str::from_utf8(&bytes).ok().is_some_and(|text| {
                let text = text.to_ascii_lowercase();
                text.contains("<html") && text.contains("</html>")
            });
            (
                ArtifactSemanticClass::Html,
                valid,
                vec![format!("html_document={valid}")],
            )
        }
        "png" => {
            let valid = bytes.starts_with(b"\x89PNG\r\n\x1a\n");
            (
                ArtifactSemanticClass::Png,
                valid,
                vec![format!("png_signature={valid}")],
            )
        }
        "pdf" => {
            let valid = bytes.starts_with(b"%PDF-");
            (
                ArtifactSemanticClass::Pdf,
                valid,
                vec![format!("pdf_signature={valid}")],
            )
        }
        "zip" | "jar" | "docx" | "xlsx" | "pptx" => {
            let valid = bytes.starts_with(b"PK\x03\x04");
            (
                ArtifactSemanticClass::Zip,
                valid,
                vec![format!("zip_signature={valid}")],
            )
        }
        "txt" | "md" | "css" | "scss" | "js" | "jsx" | "ts" | "tsx" | "rs" | "py" | "go"
        | "java" | "cs" | "toml" | "yaml" | "yml" | "xml" => {
            let valid = std::str::from_utf8(&bytes)
                .ok()
                .is_some_and(|text| !text.trim().is_empty());
            (
                ArtifactSemanticClass::Text,
                valid,
                vec![format!("utf8_nonempty={valid}")],
            )
        }
        _ => {
            let valid = !bytes.is_empty();
            (
                ArtifactSemanticClass::Binary,
                valid,
                vec![format!("binary_nonempty={valid}")],
            )
        }
    };
    Ok(ArtifactSemanticResult {
        class,
        passed,
        details,
    })
}

fn repository_defined_checks(repo: &Path) -> Result<Vec<VerificationCheck>> {
    #[derive(Deserialize)]
    struct Definition {
        checks: Vec<DefinedCheck>,
    }
    #[derive(Deserialize)]
    struct DefinedCheck {
        kind: VerificationCheckKind,
        program: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default = "dot")]
        working_directory: String,
        reason: String,
    }
    fn dot() -> String {
        ".".to_owned()
    }
    let mut checks = Vec::new();
    #[cfg(windows)]
    if repo.join("verify.ps1").is_file() {
        checks.push(VerificationCheck::command(
            VerificationCheckKind::RepositoryDefined,
            "powershell",
            &["-NoProfile", "-File", "verify.ps1"],
            ".",
            "repository verification script",
        ));
    }
    #[cfg(not(windows))]
    if repo.join("verify.sh").is_file() {
        checks.push(VerificationCheck::command(
            VerificationCheckKind::RepositoryDefined,
            "bash",
            &["verify.sh"],
            ".",
            "repository verification script",
        ));
    }
    if repo.join("verify.py").is_file() {
        checks.push(VerificationCheck::command(
            VerificationCheckKind::RepositoryDefined,
            "python",
            &["verify.py"],
            ".",
            "repository verification script",
        ));
    }
    let path = repo.join(".medusa/verification.json");
    if !path.is_file() {
        return Ok(checks);
    }
    let definition: Definition = serde_json::from_slice(&fs::read(path)?)?;
    let configured = definition
        .checks
        .into_iter()
        .map(|check| {
            if check.program.trim().is_empty() || check.reason.trim().is_empty() {
                return Err(EvidenceError::Validation(
                    "repository-defined check requires program and reason".to_owned(),
                ));
            }
            let args = check.args.iter().map(String::as_str).collect::<Vec<_>>();
            Ok(VerificationCheck::command(
                check.kind,
                &check.program,
                &args,
                check.working_directory,
                check.reason,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    checks.extend(configured);
    Ok(checks)
}

fn add_manifest_checks(
    repo: &Path,
    components: &[ChangedComponent],
    checks: &mut Vec<VerificationCheck>,
) -> Result<()> {
    for owner in package_owners(components) {
        let root = if owner == "." {
            repo.to_path_buf()
        } else {
            repo.join(owner)
        };
        if root.join("Cargo.toml").is_file() {
            checks.extend([
                VerificationCheck::command(
                    VerificationCheckKind::Format,
                    "cargo",
                    &["fmt", "--all", "--", "--check"],
                    owner,
                    "Rust changes require rustfmt",
                ),
                VerificationCheck::command(
                    VerificationCheckKind::Lint,
                    "cargo",
                    &[
                        "clippy",
                        "--all-targets",
                        "--all-features",
                        "--",
                        "-D",
                        "warnings",
                    ],
                    owner,
                    "Rust changes require Clippy",
                ),
                VerificationCheck::command(
                    VerificationCheckKind::Unit,
                    "cargo",
                    &["test", "--all-targets", "--all-features"],
                    owner,
                    "Rust changes require tests",
                ),
                VerificationCheck::command(
                    VerificationCheckKind::Build,
                    "cargo",
                    &["build", "--all-targets"],
                    owner,
                    "Rust changes require a build",
                ),
            ]);
        }
        if root.join("package.json").is_file() {
            add_package_checks(&root, owner, checks)?;
        }
        if root.join("pyproject.toml").is_file()
            || root.join("pytest.ini").is_file()
            || root.join("setup.cfg").is_file()
        {
            checks.push(VerificationCheck::command(
                VerificationCheckKind::Unit,
                "python",
                &["-m", "pytest"],
                owner,
                "Python changes require pytest",
            ));
        }
        if root.join("go.mod").is_file() {
            checks.push(VerificationCheck::command(
                VerificationCheckKind::Unit,
                "go",
                &["test", "./..."],
                owner,
                "Go changes require go test",
            ));
        }
        if root.join("pom.xml").is_file() {
            checks.push(VerificationCheck::command(
                VerificationCheckKind::Integration,
                "mvn",
                &["test"],
                owner,
                "Maven changes require test lifecycle",
            ));
        }
        if root.join("gradlew").is_file()
            || root.join("build.gradle").is_file()
            || root.join("build.gradle.kts").is_file()
        {
            checks.push(VerificationCheck::command(
                VerificationCheckKind::Integration,
                if cfg!(windows) {
                    "gradlew.bat"
                } else {
                    "./gradlew"
                },
                &["test"],
                owner,
                "Gradle changes require test lifecycle",
            ));
        }
        let has_dotnet = fs::read_dir(&root)
            .ok()
            .into_iter()
            .flat_map(|entries| entries.filter_map(std::result::Result::ok))
            .any(|entry| {
                entry
                    .path()
                    .extension()
                    .and_then(|value| value.to_str())
                    .is_some_and(|extension| {
                        extension.eq_ignore_ascii_case("sln")
                            || extension.eq_ignore_ascii_case("csproj")
                    })
            });
        if has_dotnet {
            checks.push(VerificationCheck::command(
                VerificationCheckKind::Unit,
                "dotnet",
                &["test"],
                owner,
                ".NET changes require dotnet test",
            ));
        }
        if root.join("CMakeLists.txt").is_file() {
            checks.extend([
                VerificationCheck::command(
                    VerificationCheckKind::Build,
                    "cmake",
                    &["-S", ".", "-B", ".medusa/cmake-build"],
                    owner,
                    "CMake changes require configuration",
                ),
                VerificationCheck::command(
                    VerificationCheckKind::Integration,
                    "ctest",
                    &["--test-dir", ".medusa/cmake-build", "--output-on-failure"],
                    owner,
                    "CMake changes require CTest",
                ),
            ]);
        }
    }
    Ok(())
}

fn add_package_checks(root: &Path, owner: &str, checks: &mut Vec<VerificationCheck>) -> Result<()> {
    let package: serde_json::Value = serde_json::from_slice(&fs::read(root.join("package.json"))?)?;
    let scripts = package
        .get("scripts")
        .and_then(serde_json::Value::as_object);
    let (manager, prefix): (&str, &[&str]) = if root.join("pnpm-lock.yaml").is_file() {
        ("pnpm", &["run"])
    } else if root.join("yarn.lock").is_file() {
        ("yarn", &[])
    } else if root.join("bun.lockb").is_file() || root.join("bun.lock").is_file() {
        ("bun", &["run"])
    } else {
        ("npm", &["run"])
    };
    for (script, kind, reason) in [
        (
            "format:check",
            VerificationCheckKind::Format,
            "frontend changes require formatting",
        ),
        (
            "lint",
            VerificationCheckKind::Lint,
            "frontend changes require linting",
        ),
        (
            "typecheck",
            VerificationCheckKind::Typecheck,
            "frontend changes require type checking",
        ),
        (
            "test",
            VerificationCheckKind::Unit,
            "frontend changes require tests",
        ),
        (
            "build",
            VerificationCheckKind::Build,
            "frontend changes require build",
        ),
    ] {
        if scripts.is_some_and(|scripts| {
            scripts
                .get(script)
                .and_then(serde_json::Value::as_str)
                .is_some()
        }) {
            let mut args = prefix.to_vec();
            args.push(script);
            checks.push(VerificationCheck::command(
                kind, manager, &args, owner, reason,
            ));
        }
    }
    Ok(())
}

fn add_security_checks(repo: &Path, checks: &mut Vec<VerificationCheck>) {
    if repo.join("Cargo.lock").is_file() {
        checks.push(VerificationCheck::command(
            VerificationCheckKind::Security,
            "cargo",
            &["audit"],
            ".",
            "security-sensitive changes require cargo audit",
        ));
    } else if repo.join("package-lock.json").is_file() {
        checks.push(VerificationCheck::command(
            VerificationCheckKind::Security,
            "npm",
            &["audit", "--omit=dev"],
            ".",
            "security-sensitive changes require npm audit",
        ));
    }
}

fn exemption_fingerprint(exemption: &VerificationExemption) -> String {
    fingerprint(&(
        exemption.kind,
        &exemption.scope_fingerprint,
        &exemption.reviewer,
        &exemption.reason,
    ))
}

fn plan_fingerprint(plan: &VerificationPlan) -> String {
    fingerprint(&(
        plan.schema_version,
        &plan.repository_fingerprint,
        &plan.commit,
        &plan.components,
        &plan.checks,
        &plan.exemptions,
    ))
}

fn command_fingerprint(receipt: &CommandReceipt) -> String {
    fingerprint(&(
        receipt.schema_version,
        &receipt.id,
        &receipt.check_id,
        &receipt.command_hash,
        receipt.exit_code,
        receipt.timed_out,
        receipt.duration_ms,
        &receipt.stdout_artifact,
        &receipt.stderr_artifact,
        receipt.passed,
    ))
}

fn check_fingerprint(receipt: &VerificationCheckReceipt) -> String {
    fingerprint(&(
        &receipt.check_id,
        receipt.kind,
        receipt.passed,
        &receipt.command,
        &receipt.evidence_ids,
        &receipt.artifact_ids,
        &receipt.details,
    ))
}

fn receipt_fingerprint(receipt: &VerificationReceipt) -> String {
    fingerprint(&(
        receipt.schema_version,
        &receipt.plan,
        &receipt.checks,
        &receipt.evidence,
        receipt.passed,
        &receipt.coverage,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChangeKind, ChangedComponent, EvidenceBundle};

    #[test]
    fn same_components_select_same_checks() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("Cargo.toml"),
            "[package]\nname='x'\nversion='0.1.0'\n",
        )
        .unwrap();
        fs::create_dir_all(directory.path().join("src")).unwrap();
        fs::write(directory.path().join("src/lib.rs"), "pub fn x(){}\n").unwrap();
        let components = vec![ChangedComponent::new(ChangeKind::Modified, "src/lib.rs").unwrap()];
        let direct =
            VerificationPlanner::plan(directory.path(), "repo", "commit", &components, &[])
                .unwrap();
        let isolated =
            VerificationPlanner::plan(directory.path(), "repo", "commit", &components, &[])
                .unwrap();
        assert_eq!(direct, isolated);
        assert!(
            direct
                .checks
                .iter()
                .any(|check| check.kind == VerificationCheckKind::Lint)
        );
    }

    #[test]
    fn ui_change_requires_browser_and_accessibility() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("package.json"),
            r#"{"scripts":{"build":"vite build","test":"vitest"}}"#,
        )
        .unwrap();
        fs::write(directory.path().join("App.tsx"), "export default 1").unwrap();
        let components = vec![ChangedComponent::new(ChangeKind::Modified, "App.tsx").unwrap()];
        let plan = VerificationPlanner::plan(directory.path(), "repo", "commit", &components, &[])
            .unwrap();
        assert!(
            plan.checks
                .iter()
                .any(|check| check.kind == VerificationCheckKind::BrowserBehavior)
        );
        assert!(
            plan.checks
                .iter()
                .any(|check| check.kind == VerificationCheckKind::Accessibility)
        );
    }

    #[test]
    fn root_python_verifier_is_preserved_for_mixed_language_changes() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("package.json"),
            r#"{"scripts":{"test":"node test.mjs"}}"#,
        )
        .unwrap();
        fs::write(
            directory.path().join("verify.py"),
            "print('verified')\n",
        )
        .unwrap();
        fs::write(directory.path().join("value.txt"), "42\n").unwrap();
        fs::create_dir_all(directory.path().join("src")).unwrap();
        fs::write(
            directory.path().join("src/slugify.py"),
            "def slugify(): pass\n",
        )
        .unwrap();
        fs::write(
            directory.path().join("src/counter.js"),
            "export default 1;\n",
        )
        .unwrap();
        let components = vec![
            ChangedComponent::new(ChangeKind::Modified, "value.txt").unwrap(),
            ChangedComponent::new(ChangeKind::Modified, "src/slugify.py").unwrap(),
            ChangedComponent::new(ChangeKind::Modified, "src/counter.js").unwrap(),
        ];

        let plan = VerificationPlanner::plan(directory.path(), "repo", "commit", &components, &[])
            .unwrap();

        assert!(plan.checks.iter().any(|check| {
            check.kind == VerificationCheckKind::RepositoryDefined
                && check.program.as_deref() == Some("python")
                && check.args.len() == 1
                && check.args[0] == "verify.py"
        }));
        assert!(plan.checks.iter().any(|check| {
            check.kind == VerificationCheckKind::Unit
                && check.program.as_deref() == Some("npm")
                && check.args.len() == 2
                && check.args[0] == "run"
                && check.args[1] == "test"
        }));
    }

    #[test]
    fn corrupt_nonempty_json_fails() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("artifact.json");
        fs::write(&path, "{not-json}").unwrap();
        let result = validate_artifact_semantics(&path).unwrap();
        assert_eq!(result.class, ArtifactSemanticClass::Json);
        assert!(!result.passed);
    }

    #[test]
    fn receipt_rejects_missing_required_check() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("Cargo.toml"),
            "[package]\nname='x'\nversion='0.1.0'\n",
        )
        .unwrap();
        fs::write(directory.path().join("lib.rs"), "pub fn x(){}\n").unwrap();
        let components = vec![ChangedComponent::new(ChangeKind::Modified, "lib.rs").unwrap()];
        let plan = VerificationPlanner::plan(directory.path(), "repo", "commit", &components, &[])
            .unwrap();
        let bundle = EvidenceBundle::new("repo", "commit");
        assert!(VerificationReceipt::new(plan, Vec::new(), bundle).is_err());
    }
}
