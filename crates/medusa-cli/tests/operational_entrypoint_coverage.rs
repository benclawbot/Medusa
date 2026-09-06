#[cfg(unix)]
mod unix {
    use std::{
        ffi::OsString,
        fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        process::{Command, Output},
    };

    fn write_executable(path: &Path, body: &str) {
        fs::write(path, body).expect("write executable fixture");
        let mut permissions = fs::metadata(path).expect("fixture metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("make fixture executable");
    }

    fn output_text(output: &Output) -> String {
        format!(
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    }

    fn assert_success(output: Output) -> Output {
        assert!(output.status.success(), "{}", output_text(&output));
        output
    }

    fn fake_path(bin: &Path) -> OsString {
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![bin.to_path_buf()];
        paths.extend(std::env::split_paths(&inherited));
        std::env::join_paths(paths).expect("compose fixture PATH")
    }

    #[test]
    fn product_acceptance_binary_runs_all_platform_scenarios_with_fake_cargo() {
        let temp = tempfile::tempdir().expect("tempdir");
        let bin = temp.path().join("bin");
        let output_dir = temp.path().join("acceptance");
        let target_dir = temp.path().join("shared-target");
        let target_record = temp.path().join("target-dir-record");
        fs::create_dir_all(&bin).expect("bin directory");
        write_executable(
            &bin.join("cargo"),
            "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$CARGO_TARGET_DIR\" >> \"$MEDUSA_TARGET_DIR_RECORD\"\nprintf 'fake cargo output for %s\\n' \"$*\"\nexit 0\n",
        );

        let output = assert_success(
            Command::new(env!("CARGO_BIN_EXE_medusa-product-acceptance"))
                .args(["--output", output_dir.to_str().expect("output path")])
                .env("PATH", fake_path(&bin))
                .env("CARGO_TARGET_DIR", &target_dir)
                .env("MEDUSA_TARGET_DIR_RECORD", &target_record)
                .output()
                .expect("run product acceptance"),
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("summary:"),
            "{}",
            output_text(&output)
        );

        let summary: serde_json::Value = serde_json::from_slice(
            &fs::read(output_dir.join("summary.json")).expect("acceptance summary"),
        )
        .expect("parse acceptance summary");
        assert_eq!(summary["failed"], 0);
        assert_eq!(summary["passed"], summary["total"]);
        assert!(summary["total"].as_u64().is_some_and(|total| total >= 7));
        let target_record_contents =
            fs::read_to_string(&target_record).expect("target directory record");
        let recorded_target_dirs = target_record_contents.lines().collect::<Vec<_>>();
        assert_eq!(
            recorded_target_dirs.len(),
            summary["total"].as_u64().expect("scenario count") as usize
        );
        assert!(
            recorded_target_dirs
                .iter()
                .all(|recorded| *recorded == target_dir.to_str().expect("target path"))
        );
        for scenario in summary["scenarios"].as_array().expect("scenario array") {
            assert_eq!(scenario["status"], "passed");
            assert_eq!(scenario["verification_status"], "satisfied");
            assert_eq!(scenario["metrics"]["false_completes"], 0);
            assert_eq!(scenario["metrics"]["safety_regressions"], 0);
            let log = PathBuf::from(scenario["log"].as_str().expect("scenario log"));
            assert!(log.is_file(), "missing scenario log: {}", log.display());
            assert!(
                fs::read_to_string(&log)
                    .expect("scenario log contents")
                    .contains("fake cargo output"),
                "scenario log omitted child output: {}",
                log.display()
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proof_binary_builds_auditable_artifact_from_acceptance_summary() {
        let temp = tempfile::tempdir().expect("tempdir");
        let bin = temp.path().join("bin");
        let output_dir = temp.path().join("proof");
        fs::create_dir_all(&bin).expect("bin directory");
        write_executable(
            &bin.join("cargo"),
            r###"#!/bin/sh
set -eu
if [ "$1" != "product-acceptance" ] || [ "$2" != "--output" ]; then
  echo "unexpected fake cargo invocation: $*" >&2
  exit 2
fi
out="$3"
mkdir -p "$out"
cat > "$out/summary.json" <<'JSON'
{
  "schema_version": 1,
  "platform": "linux",
  "passed": 7,
  "failed": 0,
  "total": 7,
  "scenarios": [
    {"id":"production-orchestration","guarantee":"production runtime","command":["cargo","test"],"status":"passed","duration_ms":1,"log":"acceptance/production.log","detail":null},
    {"id":"filesystem-network-process-boundary","guarantee":"sandbox boundary","command":["cargo","test"],"status":"passed","duration_ms":2,"log":"acceptance/boundary.log","detail":null},
    {"id":"interruption-resume","guarantee":"resume","command":["cargo","test"],"status":"passed","duration_ms":3,"log":"acceptance/resume.log","detail":null},
    {"id":"checkpoint-restore","guarantee":"restore","command":["cargo","test"],"status":"passed","duration_ms":4,"log":"acceptance/checkpoint.log","detail":null},
    {"id":"verification-rollback","guarantee":"rollback","command":["cargo","test"],"status":"passed","duration_ms":5,"log":"acceptance/rollback.log","detail":null},
    {"id":"headless-entrypoint","guarantee":"headless","command":["cargo","test"],"status":"passed","duration_ms":6,"log":"acceptance/headless.log","detail":null},
    {"id":"corrupted-state-recovery","guarantee":"recovery","command":["cargo","test"],"status":"passed","duration_ms":7,"log":"acceptance/recovery.log","detail":null}
  ]
}
JSON
exit 0
"###,
        );

        let output = assert_success(
            Command::new(env!("CARGO_BIN_EXE_medusa-proof"))
                .args(["--output", output_dir.to_str().expect("output path")])
                .env("PATH", fake_path(&bin))
                .output()
                .expect("run proof"),
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("MEDUSA SAFETY + RECOVERY PROOF"),
            "{}",
            output_text(&output)
        );

        let proof: serde_json::Value = serde_json::from_slice(
            &fs::read(output_dir.join("medusa-proof.json")).expect("proof artifact"),
        )
        .expect("parse proof artifact");
        assert_eq!(proof["status"], "passed");
        assert_eq!(proof["acceptance_totals"]["failed"], 0);
        assert_eq!(
            proof["guarantees"].as_array().expect("guarantees").len(),
            10
        );
    }

    #[test]
    fn quickstart_binary_verifies_a_bounded_repository_with_fake_prerequisites() {
        let temp = tempfile::tempdir().expect("tempdir");
        let bin = temp.path().join("bin");
        let repo = temp.path().join("repo");
        fs::create_dir_all(&bin).expect("bin directory");
        fs::create_dir_all(&repo).expect("repository directory");
        write_executable(
            &bin.join("git"),
            "#!/bin/sh\nset -eu\nif [ \"${1:-}\" = \"--version\" ]; then\n  echo 'git version fixture'\nelif [ \"${1:-}\" = \"init\" ]; then\n  mkdir -p .git\nelse\n  echo \"unexpected git invocation: $*\" >&2\n  exit 2\nfi\n",
        );
        write_executable(&bin.join("node"), "#!/bin/sh\necho 'v22.0.0-fixture'\n");

        let output = assert_success(
            Command::new(env!("CARGO_BIN_EXE_medusa-quickstart"))
                .args(["--json", "--repo", repo.to_str().expect("repo path")])
                .env("PATH", fake_path(&bin))
                .env_remove("ANTHROPIC_API_KEY")
                .env_remove("MINIMAX_API_KEY")
                .env_remove("MEDUSA_API_KEY")
                .env_remove("MEDUSA_BASE_URL")
                .env("OPENAI_API_KEY", "coverage-fixture-not-a-real-secret")
                .output()
                .expect("run quickstart"),
        );
        let report: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("parse quickstart report");
        assert_eq!(report["success"], true, "{}", output_text(&output));
        assert_eq!(report["sample_created"], false);
        assert_eq!(report["selected_route"]["provider"], "openai");
        assert_eq!(report["task"]["verified"], true);
        assert!(repo.join("MEDUSA_QUICKSTART.md").is_file());

        let missing_provider_repo = temp.path().join("missing-provider-repo");
        fs::create_dir_all(&missing_provider_repo).expect("missing-provider repository");
        let missing_provider = Command::new(env!("CARGO_BIN_EXE_medusa-quickstart"))
            .args([
                "--repo",
                missing_provider_repo
                    .to_str()
                    .expect("missing-provider path"),
            ])
            .env("PATH", fake_path(&bin))
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove("MINIMAX_API_KEY")
            .env_remove("MEDUSA_API_KEY")
            .env_remove("MEDUSA_BASE_URL")
            .output()
            .expect("run missing-provider quickstart");
        assert!(
            !missing_provider.status.success(),
            "missing provider must fail: {}",
            output_text(&missing_provider)
        );
        let human = String::from_utf8_lossy(&missing_provider.stdout);
        assert!(human.contains("Medusa quickstart"));
        assert!(human.contains("[failed] provider-route"));
        assert!(human.contains("[failed] bounded-task"));
        assert!(human.contains("FAILURE:"));
        assert!(!missing_provider_repo.join("MEDUSA_QUICKSTART.md").exists());
    }

    fn write_skill(root: &Path, name: &str, dependencies: &[&str]) {
        let directory = root.join(".medusa/skills").join(name);
        fs::create_dir_all(&directory).expect("skill directory");
        fs::write(directory.join("SKILL.md"), format!("# {name}\n")).expect("skill file");
        if !dependencies.is_empty() {
            fs::write(
                directory.join("dependencies.json"),
                serde_json::to_vec_pretty(&serde_json::json!({
                    "schema_version": 1,
                    "requires": dependencies,
                }))
                .expect("dependency manifest"),
            )
            .expect("write dependency manifest");
        }
    }

    fn run_medusa(repo: &Path, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_medusa"));
        command.arg("--repo").arg(repo);
        command.args(args);
        assert_success(command.output().expect("run medusa command"))
    }

    #[test]
    fn skills_dependency_commands_cover_inspect_validate_lock_and_verify() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_skill(temp.path(), "base", &[]);
        write_skill(temp.path(), "application", &["base"]);

        let inspect = run_medusa(
            temp.path(),
            &["skills", "dependencies", "application", "--json"],
        );
        let inspection: serde_json::Value =
            serde_json::from_slice(&inspect.stdout).expect("inspection json");
        assert_eq!(inspection["skill"], "application");
        assert_eq!(inspection["direct"][0], "base");

        let validate = run_medusa(temp.path(), &["skills", "validate-dependencies", "--json"]);
        let validation: serde_json::Value =
            serde_json::from_slice(&validate.stdout).expect("validation json");
        assert_eq!(validation["valid"], true);

        let lock = run_medusa(
            temp.path(),
            &["skills", "lock-dependencies", "application", "--json"],
        );
        let receipt: serde_json::Value =
            serde_json::from_slice(&lock.stdout).expect("lock receipt");
        assert!(
            receipt["graph_sha256"]
                .as_str()
                .is_some_and(|hash| hash.len() == 64)
        );

        run_medusa(
            temp.path(),
            &["skills", "lock-dependencies", "application", "--check"],
        );
        run_medusa(
            temp.path(),
            &["skills", "verify-dependency-lock", "application", "--json"],
        );
    }
}
