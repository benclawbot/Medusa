use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

mod first_run;
mod oauth_preflight;
mod report_command;
mod skill_dependencies;
mod skill_graduation;
mod skill_lifecycle;
mod skill_probation;
#[cfg_attr(not(test), allow(unused_imports))]
mod skills;

mod legacy {
    pub(super) fn entry() {
        main();
    }

    pub(super) fn interactive_entry_requested() -> bool {
        Cli::try_parse().is_ok_and(|cli| cli.command.is_none())
    }

    pub(super) fn config_init_requested() -> bool {
        Cli::try_parse().is_ok_and(|cli| {
            matches!(
                cli.command,
                Some(CommandKind::Config { action: None })
                    | Some(CommandKind::Config {
                        action: Some(ConfigAction::Init),
                    })
            )
        })
    }

    include!("main.rs");
}

fn main() {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let repo = repository_argument(&args).unwrap_or_else(|| PathBuf::from("."));

    if let Some(quickstart_args) = subcommand_arguments(&args, "quickstart") {
        finish(
            run_sibling("medusa-quickstart", &quickstart_args),
            None::<&str>,
        );
        return;
    }
    if let Some(report_args) = subcommand_arguments(&args, "report") {
        let command_args = strip_repository_argument(&report_args);
        finish(
            report_command::run(&repo, &command_args),
            Some("usage: medusa report <session-id> [--format markdown|json] [--output PATH]"),
        );
        return;
    }
    if let Some(skill_args) = subcommand_arguments(&args, "skills") {
        let command_args = strip_repository_argument(&skill_args);
        let dependencies = skill_dependencies::try_run(&repo, &command_args);
        let graduation = dependencies
            .is_none()
            .then(|| skill_graduation::try_run(&repo, &command_args))
            .flatten();
        let lifecycle = (dependencies.is_none() && graduation.is_none())
            .then(|| skill_lifecycle::try_run(&repo, &command_args))
            .flatten();
        let probation = (dependencies.is_none() && graduation.is_none() && lifecycle.is_none())
            .then(|| skill_probation::try_run(&repo, &command_args))
            .flatten();
        let usage = if dependencies.is_some() {
            Some(skill_dependencies::usage_lines())
        } else if graduation.is_some() {
            Some(skill_graduation::usage_line())
        } else if lifecycle.is_some() {
            Some(skill_lifecycle::usage_lines())
        } else if probation.is_some() {
            Some(skill_probation::usage_line())
        } else {
            None
        };
        let result = match (dependencies, graduation, lifecycle, probation) {
            (Some(result), _, _, _)
            | (_, Some(result), _, _)
            | (_, _, Some(result), _)
            | (_, _, _, Some(result)) => result,
            (None, None, None, None) => skills::run(&skill_args),
        };
        finish(result, usage);
        return;
    }
    if let Some(recall_args) = subcommand_arguments(&args, "recall") {
        finish(run_recall(&recall_args), None::<&str>);
        return;
    }
    if legacy::config_init_requested() {
        match first_run::configure_interactive() {
            Ok(first_run::FirstRunDisposition::Continue | first_run::FirstRunDisposition::Cancelled) => {
                return;
            }
            Err(error) => {
                eprintln!(
                    "{}",
                    serde_json::to_string_pretty(&error).unwrap_or_else(|_| error.to_string())
                );
                std::process::exit(1);
            }
        }
    }
    if legacy::interactive_entry_requested() {
        match first_run::ensure_first_run() {
            Ok(first_run::FirstRunDisposition::Continue) => {}
            Ok(first_run::FirstRunDisposition::Cancelled) => return,
            Err(error) => {
                eprintln!(
                    "{}",
                    serde_json::to_string_pretty(&error).unwrap_or_else(|_| error.to_string())
                );
                std::process::exit(1);
            }
        }
    }
    legacy::entry();
}

fn finish<T: AsRef<str>>(result: Result<(), String>, usage: Option<T>) {
    if let Err(error) = result {
        eprintln!("{error}");
        if let Some(usage) = usage {
            eprintln!("{}", usage.as_ref());
        }
        std::process::exit(1);
    }
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
        } else if value.starts_with("--repo=")
            || value.starts_with("--set=")
            || value.starts_with('-')
        {
            index += 1;
        } else {
            return None;
        }
    }
    None
}

fn repository_argument(args: &[String]) -> Option<PathBuf> {
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--repo" {
            return args.get(index + 1).map(PathBuf::from);
        }
        if let Some(path) = args[index].strip_prefix("--repo=") {
            return (!path.is_empty()).then(|| PathBuf::from(path));
        }
        index += 1;
    }
    None
}

fn strip_repository_argument(args: &[String]) -> Vec<String> {
    let mut stripped = Vec::new();
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--repo" {
            index += 2;
        } else if args[index].starts_with("--repo=") {
            index += 1;
        } else {
            stripped.push(args[index].clone());
            index += 1;
        }
    }
    stripped
}

fn takes_value(value: &str) -> bool {
    matches!(
        value,
        "--repo" | "--set" | "--prompt" | "--resume" | "--format" | "--output"
    )
}

fn run_recall(args: &[String]) -> Result<(), String> {
    run_sibling("medusa-recall", args)
}

fn run_sibling(name: &str, args: &[String]) -> Result<(), String> {
    let executable = sibling_executable(name).map_err(|error| error.to_string())?;
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

fn sibling_executable(name: &str) -> std::io::Result<PathBuf> {
    let current = env::current_exe()?;
    let name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    let sibling = current.with_file_name(&name);
    Ok(if sibling.is_file() {
        sibling
    } else {
        Path::new(&name).to_path_buf()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn quickstart_is_delegated_with_global_repository() {
        assert_eq!(
            subcommand_arguments(
                &strings(&["--repo", "/workspace/project", "quickstart", "--json"]),
                "quickstart"
            ),
            Some(strings(&["--repo", "/workspace/project", "--json"]))
        );
    }

    #[test]
    fn report_router_preserves_global_repository() {
        let args = strings(&[
            "--repo",
            "/workspace/project",
            "report",
            "session-1",
            "--format",
            "json",
        ]);
        assert_eq!(
            repository_argument(&args),
            Some(PathBuf::from("/workspace/project"))
        );
        assert_eq!(
            subcommand_arguments(&args, "report"),
            Some(strings(&[
                "--repo",
                "/workspace/project",
                "session-1",
                "--format",
                "json",
            ]))
        );
    }

    #[test]
    fn report_operands_do_not_trigger_report_dispatch() {
        assert_eq!(
            subcommand_arguments(&strings(&["search", "report"]), "report"),
            None
        );
        assert_eq!(
            subcommand_arguments(&strings(&["--prompt", "report"]), "report"),
            None
        );
    }

    #[test]
    fn ordinary_commands_remain_with_legacy_cli() {
        assert_eq!(
            subcommand_arguments(&strings(&["run", "fix tests"]), "recall"),
            None
        );
        assert_eq!(
            subcommand_arguments(&strings(&["run", "skills"]), "skills"),
            None
        );
        assert_eq!(
            subcommand_arguments(&strings(&["run", "quickstart"]), "quickstart"),
            None
        );
    }
}
