use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use clap::Parser;
use serde::Serialize;

#[derive(Parser, Debug)]
#[command(
    name = "medusa quickstart",
    about = "Verify a safe Medusa installation with one deterministic bounded task"
)]
struct Args {
    /// Emit only the versioned machine-readable report.
    #[arg(long)]
    json: bool,
    /// Use an existing harmless repository instead of creating a temporary sample.
    #[arg(long, value_name = "PATH")]
    repo: Option<PathBuf>,
    /// Keep an automatically-created sample repository after verification.
    #[arg(long)]
    keep_sample: bool,
}

#[derive(Debug, Serialize)]
struct Check {
    id: &'static str,
    ok: bool,
    detail: String,
    remediation: Option<String>,
}

#[derive(Debug, Serialize)]
struct ProviderRoute {
    kind: &'static str,
    provider: String,
    authentication: &'static str,
    credential_source: Option<String>,
    capabilities: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct QuickstartReport {
    schema_version: u32,
    success: bool,
    repository: String,
    sample_created: bool,
    selected_route: Option<ProviderRoute>,
    checks: Vec<Check>,
    task: TaskReport,
}

#[derive(Debug, Serialize)]
struct TaskReport {
    objective: &'static str,
    bounded: bool,
    changed_file: Option<String>,
    verified: bool,
    detail: String,
}

fn main() {
    let args = Args::parse();
    let report = run(&args);
    if args.json {
        match serde_json::to_string_pretty(&report) {
            Ok(json) => println!("{json}"),
            Err(error) => {
                eprintln!("failed to serialize quickstart report: {error}");
                std::process::exit(1);
            }
        }
    } else {
        print_human(&report);
    }
    if !report.success {
        std::process::exit(1);
    }
}

fn run(args: &Args) -> QuickstartReport {
    let (repo, sample_created) = match &args.repo {
        Some(path) => (absolute(path), false),
        None => (sample_path(), true),
    };

    let mut checks = vec![
        command_check(
            "git",
            "git",
            &["--version"],
            "Install Git and ensure `git` is available on PATH.",
        ),
        command_check(
            "node",
            "node",
            &["--version"],
            "Install a supported Node.js release and ensure `node` is available on PATH; browser and MCP sidecars require it.",
        ),
        containment_check(),
    ];

    let route = detect_route();
    checks.push(route_check(route.as_ref()));
    checks.push(capability_check(route.as_ref()));

    let repository_check = prepare_repository(&repo);
    checks.push(repository_check);

    let preflight_ok = checks.iter().all(|check| check.ok);
    let task = if preflight_ok {
        execute_bounded_task(&repo)
    } else {
        TaskReport {
            objective: "create and verify one harmless repository-local proof file",
            bounded: true,
            changed_file: None,
            verified: false,
            detail: "not run because a prerequisite failed".to_owned(),
        }
    };

    let success = preflight_ok && task.verified;
    if sample_created && !args.keep_sample {
        let _ = fs::remove_dir_all(&repo);
    }

    QuickstartReport {
        schema_version: 1,
        success,
        repository: repo.display().to_string(),
        sample_created,
        selected_route: route,
        checks,
        task,
    }
}

fn detect_route() -> Option<ProviderRoute> {
    const DIRECT: [(&str, &str); 4] = [
        ("ANTHROPIC_API_KEY", "anthropic"),
        ("OPENAI_API_KEY", "openai"),
        ("MINIMAX_API_KEY", "minimax"),
        ("MEDUSA_API_KEY", "configured-gateway"),
    ];
    for (variable, provider) in DIRECT {
        if env::var_os(variable).is_some_and(|value| !value.is_empty()) {
            return Some(ProviderRoute {
                kind: "direct",
                provider: provider.to_owned(),
                authentication: "environment",
                credential_source: Some(variable.to_owned()),
                capabilities: vec!["text-generation", "tool-calling"],
            });
        }
    }

    if let Some(endpoint) = env::var_os("MEDUSA_BASE_URL").filter(|value| !value.is_empty()) {
        return Some(ProviderRoute {
            kind: "local-or-custom",
            provider: endpoint.to_string_lossy().into_owned(),
            authentication: if env::var_os("MEDUSA_API_KEY").is_some() {
                "environment"
            } else {
                "none"
            },
            credential_source: env::var_os("MEDUSA_API_KEY").map(|_| "MEDUSA_API_KEY".to_owned()),
            capabilities: vec!["text-generation", "tool-calling"],
        });
    }

    None
}

fn route_check(route: Option<&ProviderRoute>) -> Check {
    match route {
        Some(route) => Check {
            id: "provider-route",
            ok: true,
            detail: format!("selected {} route `{}`", route.kind, route.provider),
            remediation: None,
        },
        None => Check {
            id: "provider-route",
            ok: false,
            detail: "no authenticated direct provider or local/custom route was detected".to_owned(),
            remediation: Some(
                "Export ANTHROPIC_API_KEY, OPENAI_API_KEY, MINIMAX_API_KEY, or configure MEDUSA_BASE_URL (and MEDUSA_API_KEY when required), then rerun `medusa quickstart`. Credentials are read from the environment and are never persisted."
                    .to_owned(),
            ),
        },
    }
}

fn capability_check(route: Option<&ProviderRoute>) -> Check {
    match route {
        Some(route) if route.capabilities.contains(&"tool-calling") => Check {
            id: "model-capabilities",
            ok: true,
            detail: "route declares text generation and tool calling required by the bounded verification flow"
                .to_owned(),
            remediation: None,
        },
        Some(_) => Check {
            id: "model-capabilities",
            ok: false,
            detail: "the selected route does not provide tool calling".to_owned(),
            remediation: Some(
                "Select a model/route with tool-calling support and rerun `medusa quickstart`."
                    .to_owned(),
            ),
        },
        None => Check {
            id: "model-capabilities",
            ok: false,
            detail: "capabilities cannot be validated until a provider route is available".to_owned(),
            remediation: Some("Configure a provider route first.".to_owned()),
        },
    }
}

fn containment_check() -> Check {
    if cfg!(target_os = "linux") || cfg!(target_os = "macos") || cfg!(target_os = "windows") {
        Check {
            id: "containment-backend",
            ok: true,
            detail: format!("supported {} containment platform", env::consts::OS),
            remediation: None,
        }
    } else {
        Check {
            id: "containment-backend",
            ok: false,
            detail: format!("no packaged containment backend for {}", env::consts::OS),
            remediation: Some(
                "Run Medusa on Linux, macOS, or Windows, or install a custom containment backend before executing agent tasks."
                    .to_owned(),
            ),
        }
    }
}

fn prepare_repository(repo: &Path) -> Check {
    let result = (|| -> std::io::Result<()> {
        fs::create_dir_all(repo)?;
        if !repo.join(".git").is_dir() {
            let status = Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(repo)
                .status()?;
            if !status.success() {
                return Err(std::io::Error::other("git init failed"));
            }
        }
        let probe = repo.join(".medusa-quickstart-write-probe");
        fs::write(&probe, b"probe\n")?;
        fs::remove_file(probe)?;
        Ok(())
    })();

    match result {
        Ok(()) => Check {
            id: "repository",
            ok: true,
            detail: format!("writable Git repository at {}", repo.display()),
            remediation: None,
        },
        Err(error) => Check {
            id: "repository",
            ok: false,
            detail: error.to_string(),
            remediation: Some(
                "Choose a writable directory with `medusa quickstart --repo <path>` and verify Git can initialize it."
                    .to_owned(),
            ),
        },
    }
}

fn execute_bounded_task(repo: &Path) -> TaskReport {
    let proof = repo.join("MEDUSA_QUICKSTART.md");
    let content = "# Medusa quickstart proof\n\nThis repository-local file was created by the deterministic bounded quickstart verification.\n";
    let result = fs::write(&proof, content).and_then(|_| fs::read_to_string(&proof));
    match result {
        Ok(actual) if actual == content => TaskReport {
            objective: "create and verify one harmless repository-local proof file",
            bounded: true,
            changed_file: Some(proof.display().to_string()),
            verified: true,
            detail: "proof file content matched the expected deterministic result".to_owned(),
        },
        Ok(_) => TaskReport {
            objective: "create and verify one harmless repository-local proof file",
            bounded: true,
            changed_file: Some(proof.display().to_string()),
            verified: false,
            detail: "proof file did not match the expected content; inspect filesystem transformations and permissions"
                .to_owned(),
        },
        Err(error) => TaskReport {
            objective: "create and verify one harmless repository-local proof file",
            bounded: true,
            changed_file: Some(proof.display().to_string()),
            verified: false,
            detail: format!("bounded task failed: {error}; verify repository write permissions"),
        },
    }
}

fn command_check(
    id: &'static str,
    program: &str,
    args: &[&str],
    remediation: &'static str,
) -> Check {
    match Command::new(program).args(args).output() {
        Ok(output) if output.status.success() => Check {
            id,
            ok: true,
            detail: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
            remediation: None,
        },
        Ok(output) => Check {
            id,
            ok: false,
            detail: format!("{program} exited with {}", output.status),
            remediation: Some(remediation.to_owned()),
        },
        Err(error) => Check {
            id,
            ok: false,
            detail: error.to_string(),
            remediation: Some(remediation.to_owned()),
        },
    }
}

fn sample_path() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    env::temp_dir().join(format!("medusa-quickstart-{}-{nonce}", std::process::id()))
}

fn absolute(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| {
        env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    })
}

fn print_human(report: &QuickstartReport) {
    println!("Medusa quickstart");
    for check in &report.checks {
        println!(
            "[{}] {}: {}",
            if check.ok { "ok" } else { "failed" },
            check.id,
            check.detail
        );
        if let Some(remediation) = &check.remediation {
            println!("  next: {remediation}");
        }
    }
    println!(
        "[{}] bounded-task: {}",
        if report.task.verified { "ok" } else { "failed" },
        report.task.detail
    );
    if report.success {
        println!("SUCCESS: Medusa prerequisites and the deterministic bounded task verified.");
    } else {
        println!("FAILURE: Fix the prerequisite shown above and rerun `medusa quickstart`.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_task_is_repository_local_and_deterministic() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = execute_bounded_task(temp.path());
        let second = execute_bounded_task(temp.path());
        assert!(first.verified);
        assert!(second.verified);
        assert_eq!(first.changed_file, second.changed_file);
    }

    #[test]
    fn unsupported_route_has_actionable_remediation() {
        let check = route_check(None);
        assert!(!check.ok);
        assert!(
            check
                .remediation
                .as_deref()
                .is_some_and(|text| text.contains("rerun"))
        );
    }
}
