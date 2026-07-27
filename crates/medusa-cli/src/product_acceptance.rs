use serde::Serialize;
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Mode {
    Smoke,
    Full,
}

impl Mode {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "smoke" => Ok(Self::Smoke),
            "full" => Ok(Self::Full),
            other => Err(format!("unknown acceptance mode: {other}")),
        }
    }
}

#[derive(Debug, Clone)]
struct Scenario {
    id: &'static str,
    guarantee: &'static str,
    package: &'static str,
    filter: Option<&'static str>,
    marker: Option<&'static str>,
    smoke: bool,
}

#[derive(Debug, Serialize)]
struct ScenarioResult {
    id: String,
    guarantee: String,
    command: Vec<String>,
    status: String,
    duration_ms: u128,
    log: String,
    detail: Option<String>,
}

#[derive(Debug, Serialize)]
struct Summary {
    schema_version: u32,
    generated_unix_seconds: u64,
    platform: String,
    commit: Option<String>,
    mode: Mode,
    build_duration_ms: u128,
    scenario_duration_ms: u128,
    total_job_duration_ms: u128,
    build_reuse: bool,
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
    let total_started = Instant::now();
    let (output_dir, mode) = parse_args()?;
    fs::create_dir_all(&output_dir)?;
    let scenarios = scenarios(mode);
    if scenarios.is_empty() {
        return Err("no product acceptance scenarios selected".into());
    }

    let target_dir = output_dir.join("cargo-target");
    let build_started = Instant::now();
    prebuild(&scenarios, &target_dir, &output_dir)?;
    let build_duration = build_started.elapsed();

    let scenario_started = Instant::now();
    let mut results = Vec::with_capacity(scenarios.len());
    for scenario in scenarios {
        println!("==> {}: {}", scenario.id, scenario.guarantee);
        let result = execute(&scenario, &output_dir, &target_dir)?;
        println!("    {} ({} ms)", result.status, result.duration_ms);
        results.push(result);
    }
    let scenario_duration = scenario_started.elapsed();

    let passed = results
        .iter()
        .filter(|result| result.status == "passed")
        .count();
    let failed = results.len().saturating_sub(passed);
    let summary = Summary {
        schema_version: 2,
        generated_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        platform: env::consts::OS.to_string(),
        commit: env::var("GITHUB_SHA").ok(),
        mode,
        build_duration_ms: build_duration.as_millis(),
        scenario_duration_ms: scenario_duration.as_millis(),
        total_job_duration_ms: total_started.elapsed().as_millis(),
        build_reuse: true,
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

fn parse_args() -> Result<(PathBuf, Mode), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let mut output = PathBuf::from("target/product-acceptance");
    let mut mode = Mode::Full;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--output" => output = PathBuf::from(args.next().ok_or("--output requires a path")?),
            "--mode" => mode = Mode::parse(&args.next().ok_or("--mode requires smoke or full")?)?,
            "--help" | "-h" => {
                println!("Usage: cargo product-acceptance [--mode smoke|full] [--output PATH]");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}").into()),
        }
    }
    Ok((output, mode))
}

fn scenario(
    id: &'static str,
    guarantee: &'static str,
    package: &'static str,
    filter: Option<&'static str>,
    smoke: bool,
) -> Scenario {
    Scenario {
        id,
        guarantee,
        package,
        filter,
        marker: filter,
        smoke,
    }
}

fn scenarios(mode: Mode) -> Vec<Scenario> {
    let mut all = vec![
        scenario(
            "production-orchestration",
            "Production orchestration passes its authoritative integration suite.",
            "medusa-execution-orchestrator",
            None,
            true,
        ),
        scenario(
            "headless-entrypoint",
            "The shipped CLI retains the supported headless run entrypoint.",
            "medusa-cli",
            Some("headless_run_remains_available"),
            true,
        ),
        scenario(
            "checkpoint-restore",
            "Execution checkpoints persist and restore deterministically.",
            "medusa-execution-checkpoint",
            None,
            false,
        ),
        scenario(
            "verification-rollback",
            "Repository changes roll back after failed integration.",
            "medusa-repository-rollback",
            None,
            false,
        ),
        scenario(
            "escalation",
            "Policy-sensitive states route through bounded escalation.",
            "medusa-escalation",
            None,
            false,
        ),
        scenario(
            "corrupted-state-recovery",
            "Recovery handles invalid durable state without unsafe continuation.",
            "medusa-recovery-coordinator",
            None,
            false,
        ),
        scenario(
            "upgrade-rollback-evidence",
            "Install, upgrade, and rollback remain byte-exact and auditable.",
            "medusa-hardening",
            Some("clean_install_upgrade_and_rollback_are_byte_exact"),
            false,
        ),
    ];

    match env::consts::OS {
        "windows" => {
            all.push(scenario(
                "containment-fail-closed",
                "Windows containment fails closed when the backend cannot launch.",
                "medusa-process-containment",
                Some("unresolvable_programs_fail_closed"),
                true,
            ));
            all.push(scenario(
                "interruption-replay",
                "Interrupted execution replays from durable evidence.",
                "medusa-execution-replay",
                None,
                false,
            ));
        }
        "linux" => {
            all.push(scenario(
                "filesystem-network-process-boundary",
                "The real Bubblewrap backend allows repository writes and denies external writes and network access.",
                "medusa-agent",
                Some(
                    "linux_product_boundary_exercises_allowed_write_external_denial_and_network_denial",
                ),
                true,
            ));
            all.push(scenario(
                "interruption-resume",
                "Interrupted repair resumes with exact durable evidence.",
                "medusa-agent",
                Some("fixture_bug_fix_survives_restart_with_exact_evidence"),
                false,
            ));
        }
        "macos" => {
            all.push(scenario(
                "policy-boundary",
                "The macOS runtime enforces hard shell-command denials.",
                "medusa-agent",
                Some("dangerous_shell_commands_are_denied"),
                true,
            ));
            all.push(scenario(
                "interruption-resume",
                "Interrupted repair resumes with exact durable evidence.",
                "medusa-agent",
                Some("fixture_bug_fix_survives_restart_with_exact_evidence"),
                false,
            ));
        }
        _ => {}
    }

    if mode == Mode::Smoke {
        all.retain(|item| item.smoke);
    }
    all
}

fn prebuild(
    scenarios: &[Scenario],
    target_dir: &Path,
    output_dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let packages: BTreeSet<_> = scenarios.iter().map(|item| item.package).collect();
    let mut args = vec![
        "test".to_string(),
        "--no-run".to_string(),
        "--locked".to_string(),
    ];
    for package in packages {
        args.extend(["-p".to_string(), package.to_string()]);
    }
    let output = Command::new(cargo())
        .args(&args)
        .env("CARGO_TARGET_DIR", target_dir)
        .env("MEDUSA_PRODUCT_ACCEPTANCE", "1")
        .output()?;
    fs::write(output_dir.join("build.log"), combine(&output))?;
    if !output.status.success() {
        return Err(format!("acceptance prebuild failed with {}", status(&output)).into());
    }
    Ok(())
}

fn execute(
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
    args.extend(["--".to_string(), "--nocapture".to_string()]);

    let started = Instant::now();
    let output = Command::new(cargo())
        .args(&args)
        .env("CARGO_TARGET_DIR", target_dir)
        .env("MEDUSA_PRODUCT_ACCEPTANCE", "1")
        .output()?;
    let duration_ms = started.elapsed().as_millis();
    let combined = combine(&output);
    let log_path = output_dir.join(format!("{}.log", scenario.id));
    fs::write(&log_path, &combined)?;

    let marker_present = scenario
        .marker
        .is_none_or(|marker| combined.contains(marker));
    let passed = output.status.success() && marker_present;
    let detail = if !output.status.success() {
        Some(format!("cargo exited with status {}", status(&output)))
    } else if !marker_present {
        Some(
            "required test marker was not present; the filter may have matched zero tests"
                .to_string(),
        )
    } else {
        None
    };

    Ok(ScenarioResult {
        id: scenario.id.to_string(),
        guarantee: scenario.guarantee.to_string(),
        command: std::iter::once(cargo().to_string()).chain(args).collect(),
        status: if passed { "passed" } else { "failed" }.to_string(),
        duration_ms,
        log: log_path.display().to_string(),
        detail,
    })
}

fn cargo() -> &'static str {
    if cfg!(windows) {
        "cargo.exe"
    } else {
        "cargo"
    }
}

fn status(output: &Output) -> String {
    output
        .status
        .code()
        .map_or_else(|| "terminated".to_string(), |code| code.to_string())
}

fn combine(output: &Output) -> String {
    format!(
        "--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
