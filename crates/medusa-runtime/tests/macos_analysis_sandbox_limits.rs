#![cfg(target_os = "macos")]

use std::{fs, sync::atomic::AtomicBool};

use medusa_agent::run_contained_analysis_command;
use tempfile::TempDir;

fn run_python(root: &std::path::Path, script: &str) -> std::process::Output {
    let args = vec![
        "-I".to_owned(),
        "-B".to_owned(),
        "-c".to_owned(),
        script.to_owned(),
    ];
    run_contained_analysis_command(root, "python3", &args, &AtomicBool::new(false))
        .expect("macOS contained Python command must launch")
}

#[test]
fn contained_python_starts_and_accepts_analysis_resource_limits() {
    let temp = TempDir::new().expect("tempdir");
    let root = temp
        .path()
        .join(".medusa")
        .join("analysis-workspace-v1")
        .join("diagnostic-session");
    fs::create_dir_all(&root).expect("analysis root");

    let bootstrap = run_python(&root, "print('bootstrap-ok')");
    assert!(
        bootstrap.status.success(),
        "contained Python bootstrap failed: status={:?} stdout={} stderr={}",
        bootstrap.status,
        String::from_utf8_lossy(&bootstrap.stdout),
        String::from_utf8_lossy(&bootstrap.stderr),
    );

    let constrained = run_python(
        &root,
        r#"
import resource, sys
resource.setrlimit(resource.RLIMIT_CPU, (10, 10))
resource.setrlimit(resource.RLIMIT_FSIZE, (16777216, 16777216))
if hasattr(resource, "RLIMIT_NPROC"):
    resource.setrlimit(resource.RLIMIT_NPROC, (1, 1))
print("limits-ok")
"#,
    );
    assert!(
        constrained.status.success(),
        "contained Python resource-limit setup failed: status={:?} stdout={} stderr={}",
        constrained.status,
        String::from_utf8_lossy(&constrained.stdout),
        String::from_utf8_lossy(&constrained.stderr),
    );
}
