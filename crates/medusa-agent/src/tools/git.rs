use std::{collections::BTreeSet, path::Path};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult, hidden_command};

use crate::tools::format_command_output;

pub(crate) fn checkpoint(repo: &Path, message: &str) -> MedusaResult<String> {
    run_git(repo, &["add", "-A"])?;
    run_git(repo, &["commit", "-m", message])?;
    Ok(format!("checkpoint created: {message}"))
}

fn run_git(repo: &Path, args: &[&str]) -> MedusaResult<()> {
    let hooks_path = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let mut config_overrides = vec![
        format!("core.hooksPath={hooks_path}"),
        "core.fsmonitor=false".to_owned(),
    ];
    // A repository's .gitattributes can select arbitrary clean/smudge/process filters. Empty
    // every locally configured filter command before `git add` so a checkpoint never executes
    // repository-controlled code. The config query reads names only and runs with system/global
    // configuration disabled; values are never interpreted by a shell.
    for key in local_filter_keys(repo, hooks_path)? {
        config_overrides.push(format!("{key}="));
    }
    let output = hidden_command("git")
        .args(
            config_overrides
                .iter()
                .flat_map(|value| ["-c", value.as_str()]),
        )
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", hooks_path)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .current_dir(repo)
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(MedusaError::new(
            ErrorCode::ToolExecutionFailed,
            ErrorCategory::Execution,
            format_command_output("git", args, &output.stdout, &output.stderr).join("\n"),
        ))
    }
}

fn local_filter_keys(repo: &Path, global_config: &str) -> MedusaResult<BTreeSet<String>> {
    let output = hidden_command("git")
        .args([
            "config",
            "--local",
            "--name-only",
            "--get-regexp",
            r"^filter\..+\.(clean|smudge|process)$",
        ])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", global_config)
        .current_dir(repo)
        .output()?;
    if !output.status.success() && output.status.code() != Some(1) {
        return Err(MedusaError::new(
            ErrorCode::ToolExecutionFailed,
            ErrorCategory::Execution,
            format!(
                "git local filter configuration could not be inspected: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ));
    }
    let mut keys = BTreeSet::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some(key) = line.split_whitespace().next() else {
            continue;
        };
        let Some(filter) = key
            .strip_suffix(".clean")
            .or_else(|| key.strip_suffix(".smudge"))
            .or_else(|| key.strip_suffix(".process"))
        else {
            continue;
        };
        for suffix in ["clean", "smudge", "process"] {
            keys.insert(format!("{filter}.{suffix}"));
        }
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn checkpoint_does_not_execute_repository_hooks() {
        let repo = tempfile::tempdir().expect("repo");
        let init = hidden_command("git")
            .args(["init", "-q"])
            .current_dir(repo.path())
            .status()
            .expect("git init");
        assert!(init.success());
        for args in [
            ["config", "user.name", "Medusa Test"].as_slice(),
            ["config", "user.email", "medusa@example.invalid"].as_slice(),
        ] {
            assert!(
                hidden_command("git")
                    .args(args)
                    .current_dir(repo.path())
                    .status()
                    .expect("git config")
                    .success()
            );
        }
        let marker = repo.path().join("hook-ran");
        let hooks = repo.path().join(".git/hooks");
        fs::write(
            hooks.join("pre-commit"),
            format!("#!/bin/sh\nprintf ran > '{}'\n", marker.display()),
        )
        .expect("hook");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(hooks.join("pre-commit"), fs::Permissions::from_mode(0o755))
                .expect("hook permissions");
        }
        fs::write(repo.path().join("tracked.txt"), "content\n").expect("tracked");
        checkpoint(repo.path(), "safe checkpoint").expect("checkpoint");
        assert!(!marker.exists(), "repository pre-commit hook executed");
    }

    #[cfg(unix)]
    #[test]
    fn checkpoint_does_not_execute_repository_filters() {
        let repo = tempfile::tempdir().expect("repo");
        let init = hidden_command("git")
            .args(["init", "-q"])
            .current_dir(repo.path())
            .status()
            .expect("git init");
        assert!(init.success());
        for args in [
            ["config", "user.name", "Medusa Test"].as_slice(),
            ["config", "user.email", "medusa@example.invalid"].as_slice(),
        ] {
            assert!(
                hidden_command("git")
                    .args(args)
                    .current_dir(repo.path())
                    .status()
                    .expect("git config")
                    .success()
            );
        }
        let marker = repo.path().join("filter-ran");
        fs::write(repo.path().join(".gitattributes"), "*.txt filter=evil\n").expect("attributes");
        hidden_command("git")
            .args([
                "config",
                "filter.evil.clean",
                &format!("sh -c 'printf ran > {}; cat'", marker.display()),
            ])
            .current_dir(repo.path())
            .status()
            .expect("filter config");
        fs::write(repo.path().join("tracked.txt"), "content\n").expect("tracked");
        checkpoint(repo.path(), "safe checkpoint").expect("checkpoint");
        assert!(!marker.exists(), "repository clean filter executed");
    }
}
