use serde::Serialize;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
struct Scenario {
    id: &'static str,
    guarantee: &'static str,
    package: &'static str,
    filter: Option<&'static str>,
    required_marker: Option<&'static str>,
}

#[derive(Debug, Serialize)]
struct ScenarioMetrics {
    false_completes: usize,
    safety_regressions: usize,
}

#[derive(Debug, Serialize)]
struct ScenarioResult {
    id: String,
    guarantee: String,
    command: Vec<String>,
    status: String,
    verification_status: String,
    metrics: ScenarioMetrics,
    duration_ms: u128,
    log: String,
    detail: Option<String>,
}

#[derive(Debug, Serialize)]
struct AcceptanceSummary {
    schema_version: u32,
    generated_unix_seconds: u64,
    platform: String,
    passed: usize,
    failed: usize,
    total: usize,
    scenarios: Vec<ScenarioResult>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("product acceptance failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let output_dir = parse_output_dir()?;
    fs::create_dir_all(&output_dir)?;
    let target_dir = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target"));
    fs::create_dir_all(&target_dir)?;
    let scenarios = scenarios_for_platform();
    let mut results = Vec::with_capacity(scenarios.len());
    for scenario in scenarios {
        println!("==> {}: {}", scenario.id, scenario.guarantee);
        let result = execute_scenario(&scenario, &output_dir, &target_dir)?;
        println!("    {} ({} ms)", result.status, result.duration_ms);
        results.push(result);
    }
    let passed = results
        .iter()
        .filter(|result| result.status == "passed")
        .count();
    let failed = results.len().saturating_sub(passed);
    let summary = AcceptanceSummary {
        schema_version: 1,
        generated_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        platform: env::consts::OS.to_string(),
        passed,
        failed,
        total: results.len(),
        scenarios: results,
    };
    let summary_path = output_dir.join("summary.json");
    fs::write(&summary_path, serde_json::to_vec_pretty(&summary)?)?;
    println!("summary: {}", summary_path.display());
    if failed > 0 {
        return Err(format!("{failed} product acceptance scenario(s) failed").into());
    }
    Ok(())
}

fn parse_output_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let mut output = PathBuf::from("target/product-acceptance");
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output" => output = PathBuf::from(args.next().ok_or("--output requires a path")?),
            "--help" | "-h" => {
                println!(
                    "Usage: cargo product-acceptance [--output PATH]\n\nRuns the authoritative product-level safety and recovery acceptance suite."
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}").into()),
        }
    }
    Ok(output)
}

fn scenarios_for_platform() -> Vec<Scenario> {
    let mut scenarios = vec![
        Scenario {
            id: "production-orchestration",
            guarantee: "The production orchestration layer passes its authoritative integration suite.",
            package: "medusa-runtime",
            filter: None,
            required_marker: None,
        },
        Scenario {
            id: "headless-entrypoint",
            guarantee: "The shipped CLI retains the supported headless run entrypoint.",
            package: "medusa-cli",
            filter: Some("headless_run_remains_available"),
            required_marker: Some("headless_run_remains_available"),
        },
        Scenario {
            id: "checkpoint-restore",
            guarantee: "Execution checkpoints can be persisted and restored deterministically.",
            package: "medusa-execution-checkpoint",
            filter: None,
            required_marker: None,
        },
        Scenario {
            id: "verification-rollback",
            guarantee: "Repository changes can be rolled back after failed or rejected integration.",
            package: "medusa-workers",
            filter: None,
            required_marker: None,
        },
        Scenario {
            id: "escalation",
            guarantee: "Unresolved or policy-sensitive execution states route through bounded escalation.",
            package: "medusa-escalation",
            filter: None,
            required_marker: None,
        },
        Scenario {
            id: "corrupted-state-recovery",
            guarantee: "Recovery coordination handles invalid or incomplete durable state without unsafe continuation.",
            package: "medusa-recovery-coordinator",
            filter: None,
            required_marker: None,
        },
        Scenario {
            id: "upgrade-rollback-evidence",
            guarantee: "Install, upgrade, and rollback state transitions remain byte-exact and auditable.",
            package: "medusa-hardening",
            filter: Some("clean_install_upgrade_and_rollback_are_byte_exact"),
            required_marker: Some("clean_install_upgrade_and_rollback_are_byte_exact"),
        },
    ];

    match env::consts::OS {
        "windows" => {
            scenarios.push(Scenario {
                id: "containment-fail-closed",
                guarantee: "Windows containment fails closed when a requested program cannot be resolved or the platform backend is unavailable.",
                package: "medusa-process-containment",
                filter: Some("unresolvable_programs_fail_closed"),
                required_marker: Some("unresolvable_programs_fail_closed"),
            });
            scenarios.push(Scenario {
                id: "interruption-replay",
                guarantee: "Interrupted execution can be replayed from durable evidence on Windows.",
                package: "medusa-execution-replay",
                filter: None,
                required_marker: None,
            });
        }
        "linux" => {
            scenarios.push(Scenario {
                id: "filesystem-network-process-boundary",
                guarantee: "The production Linux sandbox starts successfully, allows repository-bounded writes, denies external writes and network access, and uses the real Bubblewrap backend.",
                package: "medusa-agent",
                filter: Some(
                    "linux_product_boundary_exercises_allowed_write_external_denial_and_network_denial",
                ),
                required_marker: Some(
                    "linux_product_boundary_exercises_allowed_write_external_denial_and_network_denial",
                ),
            });
            scenarios.push(Scenario {
                id: "interruption-resume",
                guarantee: "An interrupted repository repair resumes through durable runtime state with exact evidence.",
                package: "medusa-agent",
                filter: Some("fixture_bug_fix_survives_restart_with_exact_evidence"),
                required_marker: Some("fixture_bug_fix_survives_restart_with_exact_evidence"),
            });
        }
        "macos" => {
            scenarios.push(Scenario {
                id: "policy-boundary",
                guarantee: "The shipped macOS runtime enforces hard shell-command denials while no native process-containment backend is advertised.",
                package: "medusa-agent",
                filter: Some("dangerous_shell_commands_are_denied"),
                required_marker: Some("dangerous_shell_commands_are_denied"),
            });
            scenarios.push(Scenario {
                id: "interruption-resume",
                guarantee: "An interrupted repository repair resumes through durable runtime state with exact evidence.",
                package: "medusa-agent",
                filter: Some("fixture_bug_fix_survives_restart_with_exact_evidence"),
                required_marker: Some("fixture_bug_fix_survives_restart_with_exact_evidence"),
            });
        }
        _ => {}
    }
    scenarios
}

fn execute_scenario(
    scenario: &Scenario,
    output_dir: &Path,
    target_dir: &Path,
) -> Result<ScenarioResult, Box<dyn std::error::Error>> {
    let mut args = vec![
        "test".to_string(),
        "-p".to_string(),
        scenario.package.to_string(),
        "--locked".to_string(),
    ];
    if let Some(filter) = scenario.filter {
        args.push(filter.to_string());
    }
    args.push("--".to_string());
    args.push("--nocapture".to_string());

    let started = Instant::now();
    let output = Command::new(cargo_program())
        .args(&args)
        .env("CARGO_TARGET_DIR", &target_dir)
        .env("MEDUSA_PRODUCT_ACCEPTANCE", "1")
        .output()?;
    let duration = started.elapsed();
    let combined = combine_output(&output);
    let log_path = output_dir.join(format!("{}.log", scenario.id));
    fs::write(&log_path, combined.as_bytes())?;
    let marker_present = scenario
        .required_marker
        .is_none_or(|marker| combined.contains(marker));
    let passed = output.status.success() && marker_present;
    let detail = if !output.status.success() {
        Some(format!(
            "cargo exited with status {}",
            output
                .status
                .code()
                .map_or_else(|| "terminated".to_string(), |code| code.to_string())
        ))
    } else if !marker_present {
        Some(
            "required test marker was not present; the filter may have matched zero tests"
                .to_string(),
        )
    } else {
        None
    };
    let evidence_failure = usize::from(!passed);
    Ok(ScenarioResult {
        id: scenario.id.to_string(),
        guarantee: scenario.guarantee.to_string(),
        command: std::iter::once(cargo_program().to_string())
            .chain(args)
            .collect(),
        status: if passed { "passed" } else { "failed" }.to_string(),
        verification_status: if passed { "satisfied" } else { "unsatisfied" }.to_string(),
        // Product acceptance is a binary, authoritative contract. A failed scenario is
        // therefore recorded as one failed verification for both safety guardrails; passing
        // scenarios carry explicit zeroes instead of relying on benchmark-side defaults.
        metrics: ScenarioMetrics {
            false_completes: evidence_failure,
            safety_regressions: evidence_failure,
        },
        duration_ms: duration.as_millis(),
        log: log_path.display().to_string(),
        detail,
    })
}

fn cargo_program() -> &'static str {
    if cfg!(windows) { "cargo.exe" } else { "cargo" }
}

fn combine_output(output: &Output) -> String {
    let mut combined = String::new();
    combined.push_str("--- stdout ---\n");
    combined.push_str(&String::from_utf8_lossy(&output.stdout));
    combined.push_str("\n--- stderr ---\n");
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    combined
}
