use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const REQUIRED_LINUX_EVIDENCE: &[(&str, &str)] = &[
    ("bounded-coding-task", "production-orchestration"),
    (
        "repository-write-boundary",
        "filesystem-network-process-boundary",
    ),
    (
        "external-filesystem-denial",
        "filesystem-network-process-boundary",
    ),
    ("network-denial", "filesystem-network-process-boundary"),
    (
        "process-tree-cleanup",
        "filesystem-network-process-boundary",
    ),
    ("interrupt-and-resume", "interruption-resume"),
    ("durable-checkpoint-restore", "checkpoint-restore"),
    ("failed-change-rollback", "verification-rollback"),
    ("final-repository-verification", "headless-entrypoint"),
    ("corrupted-state-recovery", "corrupted-state-recovery"),
];

#[derive(Debug, Deserialize)]
struct AcceptanceSummary {
    schema_version: u32,
    platform: String,
    passed: usize,
    failed: usize,
    total: usize,
    scenarios: Vec<AcceptanceScenario>,
}

#[derive(Debug, Deserialize)]
struct AcceptanceScenario {
    id: String,
    guarantee: String,
    command: Vec<String>,
    status: String,
    duration_ms: u128,
    log: String,
    detail: Option<String>,
}

#[derive(Debug, Serialize)]
struct ProofArtifact {
    schema_version: u32,
    generated_unix_seconds: u64,
    platform: String,
    status: String,
    command: Vec<String>,
    source_acceptance_schema_version: u32,
    source_acceptance_summary: String,
    fixture: FixtureEvidence,
    guarantees: Vec<GuaranteeEvidence>,
    acceptance_totals: AcceptanceTotals,
}

#[derive(Debug, Serialize)]
struct FixtureEvidence {
    path: String,
    task: String,
    expected_final_state: String,
}

#[derive(Debug, Serialize)]
struct GuaranteeEvidence {
    guarantee: String,
    acceptance_scenario: String,
    status: String,
    claim: String,
    command: Vec<String>,
    log: String,
    duration_ms: u128,
    detail: Option<String>,
}

#[derive(Debug, Serialize)]
struct AcceptanceTotals {
    passed: usize,
    failed: usize,
    total: usize,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("medusa proof failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let output_dir = parse_output_dir()?;
    fs::create_dir_all(&output_dir)?;

    if env::consts::OS != "linux" {
        return Err(
            "the public proof currently requires Linux because the production Bubblewrap boundary is the authoritative evidence for filesystem, network, and process containment; run cargo product-acceptance for this platform's supported contract"
                .into(),
        );
    }

    println!("MEDUSA SAFETY + RECOVERY PROOF");
    println!("Plan -> Execute Safely -> Recover");
    println!();
    println!("[plan] fixture: examples/medusa-proof/reference-repository");
    println!("[plan] bounded task: repair the deterministic calculator fixture");
    println!("[execute safely] invoking the authoritative product acceptance contract");

    let acceptance_dir = output_dir.join("acceptance");
    let command = vec![
        cargo_program().to_string(),
        "product-acceptance".to_string(),
        "--output".to_string(),
        acceptance_dir.display().to_string(),
    ];
    let status = Command::new(cargo_program())
        .arg("product-acceptance")
        .arg("--output")
        .arg(&acceptance_dir)
        .env("MEDUSA_PROOF", "1")
        .status()?;

    let summary_path = acceptance_dir.join("summary.json");
    let summary: AcceptanceSummary = serde_json::from_slice(&fs::read(&summary_path)?)?;
    let scenarios = summary
        .scenarios
        .iter()
        .map(|scenario| (scenario.id.as_str(), scenario))
        .collect::<BTreeMap<_, _>>();

    let mut guarantees = Vec::with_capacity(REQUIRED_LINUX_EVIDENCE.len());
    let mut proof_passed = status.success() && summary.failed == 0;
    for (guarantee, scenario_id) in REQUIRED_LINUX_EVIDENCE {
        let scenario = scenarios.get(scenario_id).ok_or_else(|| {
            format!("acceptance contract drifted: required scenario `{scenario_id}` is missing")
        })?;
        let passed = scenario.status == "passed";
        proof_passed &= passed;
        println!(
            "[evidence] {:<31} {} ({})",
            guarantee,
            if passed { "PASS" } else { "FAIL" },
            scenario_id
        );
        guarantees.push(GuaranteeEvidence {
            guarantee: (*guarantee).to_string(),
            acceptance_scenario: (*scenario_id).to_string(),
            status: scenario.status.clone(),
            claim: scenario.guarantee.clone(),
            command: scenario.command.clone(),
            log: relative_or_original(&output_dir, &scenario.log),
            duration_ms: scenario.duration_ms,
            detail: scenario.detail.clone(),
        });
    }

    println!("[recover] checkpoint, resume, rollback, and corrupted-state evidence captured");
    println!("[verify] {} authoritative scenarios passed", summary.passed);

    let artifact = ProofArtifact {
        schema_version: 1,
        generated_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        platform: summary.platform,
        status: if proof_passed { "passed" } else { "failed" }.to_string(),
        command,
        source_acceptance_schema_version: summary.schema_version,
        source_acceptance_summary: path_relative_to(&output_dir, &summary_path),
        fixture: FixtureEvidence {
            path: "examples/medusa-proof/reference-repository".to_string(),
            task: "Repair the deterministic calculator fixture while preserving its tests."
                .to_string(),
            expected_final_state: "The production acceptance suite verifies bounded execution and recovery without private provider credentials."
                .to_string(),
        },
        guarantees,
        acceptance_totals: AcceptanceTotals {
            passed: summary.passed,
            failed: summary.failed,
            total: summary.total,
        },
    };

    let artifact_path = output_dir.join("medusa-proof.json");
    fs::write(&artifact_path, serde_json::to_vec_pretty(&artifact)?)?;
    println!("[audit] {}", artifact_path.display());

    if !proof_passed {
        return Err("one or more proof guarantees failed".into());
    }
    Ok(())
}

fn parse_output_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let mut output = PathBuf::from("target/medusa-proof");
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output" => output = PathBuf::from(args.next().ok_or("--output requires a path")?),
            "--help" | "-h" => {
                println!(
                    "Usage: cargo medusa-proof [--output PATH]\n\nRuns the reproducible Linux safety and recovery proof through the authoritative product acceptance contract and writes medusa-proof.json."
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}").into()),
        }
    }
    Ok(output)
}

fn cargo_program() -> &'static str {
    if cfg!(windows) { "cargo.exe" } else { "cargo" }
}

fn relative_or_original(output_dir: &Path, value: &str) -> String {
    let path = Path::new(value);
    if path.is_absolute() {
        path.strip_prefix(output_dir).map_or_else(
            |_| value.to_string(),
            |relative| relative.display().to_string(),
        )
    } else {
        value.to_string()
    }
}

fn path_relative_to(output_dir: &Path, path: &Path) -> String {
    path.strip_prefix(output_dir).map_or_else(
        |_| path.display().to_string(),
        |relative| relative.display().to_string(),
    )
}
