use std::{env, path::PathBuf, process::Command};

mod skills;

mod legacy {
    pub(super) fn entry() {
        main();
    }

    include!("main.rs");
}

fn main() {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if let Some(skill_args) = subcommand_arguments(&args, "skills") {
        if let Err(error) = skills::run(&skill_args) {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }
    if let Some(recall_args) = subcommand_arguments(&args, "recall") {
        if let Err(error) = run_recall(&recall_args) {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }
    legacy::entry();
}

fn subcommand_arguments(args: &[String], command: &str) -> Option<Vec<String>> {
    let mut index = 0;
    while index < args.len() {
        let value = &args[index];
        if value == command {
            let mut forwarded = args.to_vec();
            forwarded.remove(index);
            return Some(forwarded);
        }
        if value == "--" {
            return None;
        }
        if takes_value(value) {
            index += 2;
            continue;
        }
        if value.starts_with("--repo=") || value.starts_with("--set=") {
            index += 1;
            continue;
        }
        if value.starts_with('-') {
            index += 1;
            continue;
        }
        return None;
    }
    None
}

fn takes_value(value: &str) -> bool {
    matches!(value, "--repo" | "--set" | "--prompt" | "--resume")
}

fn run_recall(args: &[String]) -> Result<(), String> {
    let executable = recall_executable().map_err(|error| error.to_string())?;
    let status = Command::new(&executable)
        .args(args)
        .status()
        .map_err(|error| format!("launch {}: {error}", executable.display()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{} exited with {status}", executable.display()))
    }
}

fn recall_executable() -> std::io::Result<PathBuf> {
    let current = env::current_exe()?;
    let name = if cfg!(windows) {
        "medusa-recall.exe"
    } else {
        "medusa-recall"
    };
    let sibling = current.with_file_name(name);
    if sibling.is_file() {
        Ok(sibling)
    } else {
        Ok(PathBuf::from(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn bare_recall_is_delegated() {
        assert_eq!(
            subcommand_arguments(&strings(&["recall", "search", "parser"]), "recall"),
            Some(strings(&["search", "parser"]))
        );
    }

    #[test]
    fn bare_skills_is_handled_in_process() {
        assert_eq!(
            subcommand_arguments(&strings(&["skills", "list"]), "skills"),
            Some(strings(&["list"]))
        );
    }

    #[test]
    fn global_repository_is_forwarded() {
        assert_eq!(
            subcommand_arguments(
                &strings(&[
                    "--repo",
                    "/workspace/project",
                    "recall",
                    "open",
                    "session-1"
                ]),
                "recall"
            ),
            Some(strings(&[
                "--repo",
                "/workspace/project",
                "open",
                "session-1"
            ]))
        );
        assert_eq!(
            subcommand_arguments(
                &strings(&[
                    "--repo",
                    "/workspace/project",
                    "skills",
                    "approve",
                    "verify-package"
                ]),
                "skills"
            ),
            Some(strings(&[
                "--repo",
                "/workspace/project",
                "approve",
                "verify-package"
            ]))
        );
    }

    #[test]
    fn ordinary_commands_remain_with_the_existing_cli() {
        assert_eq!(
            subcommand_arguments(&strings(&["run", "fix tests"]), "recall"),
            None
        );
        assert_eq!(
            subcommand_arguments(&strings(&["search", "recall"]), "recall"),
            None
        );
        assert_eq!(
            subcommand_arguments(&strings(&["run", "skills"]), "skills"),
            None
        );
    }

    #[test]
    fn option_values_named_like_commands_are_not_subcommands() {
        assert_eq!(
            subcommand_arguments(
                &strings(&["--prompt", "recall", "run", "tests"]),
                "recall"
            ),
            None
        );
        assert_eq!(
            subcommand_arguments(
                &strings(&["--prompt", "skills", "run", "tests"]),
                "skills"
            ),
            None
        );
    }
}
