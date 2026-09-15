use std::{fs, path::Path};

use serde::Serialize;

const MAX_REPORT_ITEMS: usize = 128;
const MAX_SCAN_ENTRIES: usize = 4096;
const MAX_SCAN_DEPTH: usize = 64;

/// The uninstall manifest is deliberately closed. Anything not named here is unknown to this
/// command and is retained, even when it lives below `.medusa`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Ownership {
    MedusaEphemeral,
    UserDurable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Scope {
    Runtime,
    Cache,
    Sessions,
    Configuration,
    Skills,
    All,
}

impl Scope {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "runtime" => Ok(Self::Runtime),
            "cache" => Ok(Self::Cache),
            "sessions" | "session" => Ok(Self::Sessions),
            "configuration" | "config" => Ok(Self::Configuration),
            "skills" | "skill" => Ok(Self::Skills),
            "all" => Ok(Self::All),
            _ => Err(format!(
                "unknown uninstall scope `{value}`; expected runtime, cache, sessions, configuration, skills, or all"
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Runtime => "runtime",
            Self::Cache => "cache",
            Self::Sessions => "sessions",
            Self::Configuration => "configuration",
            Self::Skills => "skills",
            Self::All => "all",
        }
    }

    fn includes(self, entry: &ManifestEntry) -> bool {
        match self {
            Self::Runtime => entry.ownership == Ownership::MedusaEphemeral,
            Self::Cache => entry.ownership == Ownership::MedusaEphemeral && entry.cache,
            Self::Sessions => entry.group == "sessions",
            Self::Configuration => entry.group == "configuration",
            Self::Skills => entry.group == "skills",
            Self::All => true,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Action {
    Default,
    Preview,
    Purge,
}

impl Action {
    fn as_str(self) -> &'static str {
        match self {
            Self::Default => "preserve_durable_state",
            Self::Preview => "preview",
            Self::Purge => "purge",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ManifestEntry {
    relative: &'static str,
    group: &'static str,
    ownership: Ownership,
    cache: bool,
}

const UNINSTALL_MANIFEST: &[ManifestEntry] = &[
    ManifestEntry {
        relative: ".medusa/cache",
        group: "runtime",
        ownership: Ownership::MedusaEphemeral,
        cache: true,
    },
    ManifestEntry {
        relative: ".medusa/tool-cache",
        group: "runtime",
        ownership: Ownership::MedusaEphemeral,
        cache: true,
    },
    ManifestEntry {
        relative: ".medusa/update-cache",
        group: "runtime",
        ownership: Ownership::MedusaEphemeral,
        cache: true,
    },
    ManifestEntry {
        relative: ".medusa/output-expansions",
        group: "runtime",
        ownership: Ownership::MedusaEphemeral,
        cache: false,
    },
    ManifestEntry {
        relative: ".medusa/analysis-workspace-v1",
        group: "runtime",
        ownership: Ownership::MedusaEphemeral,
        cache: false,
    },
    ManifestEntry {
        relative: ".medusa/frontend-artifacts",
        group: "runtime",
        ownership: Ownership::MedusaEphemeral,
        cache: false,
    },
    ManifestEntry {
        relative: ".medusa/sessions",
        group: "sessions",
        ownership: Ownership::UserDurable,
        cache: false,
    },
    ManifestEntry {
        relative: ".medusa/journals",
        group: "sessions",
        ownership: Ownership::UserDurable,
        cache: false,
    },
    ManifestEntry {
        relative: ".medusa/session-recall-inbox",
        group: "sessions",
        ownership: Ownership::UserDurable,
        cache: false,
    },
    ManifestEntry {
        relative: ".medusa/config.toml",
        group: "configuration",
        ownership: Ownership::UserDurable,
        cache: false,
    },
    ManifestEntry {
        relative: ".medusa/runtime.toml",
        group: "configuration",
        ownership: Ownership::UserDurable,
        cache: false,
    },
    ManifestEntry {
        relative: ".medusa/skills",
        group: "skills",
        ownership: Ownership::UserDurable,
        cache: false,
    },
];

#[derive(Debug)]
struct ParsedArgs {
    action: Action,
    scope: Option<Scope>,
    json: bool,
}

#[derive(Debug, Serialize)]
struct UninstallReport {
    schema_version: u8,
    action: &'static str,
    scope: &'static str,
    removed: Vec<String>,
    preserved: Vec<String>,
    protected: Vec<ProtectedPath>,
    failures: Vec<Failure>,
    truncated: bool,
    success: bool,
    #[serde(skip)]
    blocking_protection: bool,
}

#[derive(Debug, Serialize)]
struct ProtectedPath {
    path: String,
    reason: String,
}

#[derive(Debug, Serialize)]
struct Failure {
    path: String,
    reason: String,
}

impl UninstallReport {
    fn new(action: Action, scope: Scope) -> Self {
        Self {
            schema_version: 1,
            action: action.as_str(),
            scope: scope.as_str(),
            removed: Vec::new(),
            preserved: Vec::new(),
            protected: Vec::new(),
            failures: Vec::new(),
            truncated: false,
            success: true,
            blocking_protection: false,
        }
    }

    fn item_count(&self) -> usize {
        self.removed.len() + self.preserved.len() + self.protected.len() + self.failures.len()
    }

    fn push_path(paths: &mut Vec<String>, path: String, truncated: &mut bool, count: usize) {
        if count >= MAX_REPORT_ITEMS {
            *truncated = true;
        } else {
            paths.push(path);
        }
    }

    fn removed(&mut self, path: String) {
        let count = self.item_count();
        Self::push_path(&mut self.removed, path, &mut self.truncated, count);
    }

    fn preserved(&mut self, path: String) {
        let count = self.item_count();
        Self::push_path(&mut self.preserved, path, &mut self.truncated, count);
    }

    fn protected(&mut self, path: String, reason: impl Into<String>, blocking: bool) {
        let count = self.item_count();
        if count >= MAX_REPORT_ITEMS {
            self.truncated = true;
        } else {
            self.protected.push(ProtectedPath {
                path,
                reason: reason.into(),
            });
        }
        self.blocking_protection |= blocking;
    }

    fn failure(&mut self, path: String, reason: impl Into<String>) {
        let count = self.item_count();
        if count >= MAX_REPORT_ITEMS {
            self.truncated = true;
        } else {
            self.failures.push(Failure {
                path,
                reason: reason.into(),
            });
        }
        self.success = false;
    }

    fn finish(&mut self) {
        self.success = self.failures.is_empty() && !self.blocking_protection;
    }
}

pub(super) fn run(root: &Path, args: &[String]) -> Result<(), String> {
    let parsed = parse_args(args)?;
    let scope = parsed.scope.unwrap_or(Scope::Runtime);
    let mut report = UninstallReport::new(parsed.action, scope);

    let root_type = fs::symlink_metadata(root)
        .map_err(|error| format!("inspect uninstall workspace {}: {error}", root.display()))?;
    if root_type.file_type().is_symlink() || !root_type.is_dir() {
        return Err(format!(
            "uninstall workspace must be a real directory, not a symlink: {}",
            root.display()
        ));
    }

    let state_root = root.join(".medusa");
    match fs::symlink_metadata(&state_root) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            report.protected(
                ".medusa".to_owned(),
                "the Medusa state root is a symlink; refusing path-confusing cleanup",
                parsed.action == Action::Purge,
            );
            report.finish();
            print_report(&report, parsed.json)?;
            return report_result(&report);
        }
        Ok(metadata) if !metadata.is_dir() => {
            report.protected(
                ".medusa".to_owned(),
                "the Medusa state root is not a directory; ownership cannot be established",
                parsed.action == Action::Purge,
            );
            report.finish();
            print_report(&report, parsed.json)?;
            return report_result(&report);
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            report.failure(".medusa".to_owned(), format!("inspect state root: {error}"));
        }
    }

    let linked_worktree = is_linked_worktree(root);
    record_unmanaged_state(&state_root, &mut report);

    for entry in UNINSTALL_MANIFEST {
        let entry_path = root.join(entry.relative);
        if !entry_path.exists() && !is_symlink(&entry_path) {
            continue;
        }
        if !scope.includes(entry) {
            report.preserved(entry.relative.to_owned());
            continue;
        }
        if linked_worktree {
            report.protected(
                entry.relative.to_owned(),
                "linked Git worktrees are protected from uninstall cleanup",
                parsed.action == Action::Purge,
            );
            continue;
        }

        walk_owned_path(&entry_path, entry.relative, parsed.action, &mut report, 0);
    }

    if linked_worktree && report.item_count() == 0 {
        report.protected(
            ".git".to_owned(),
            "linked Git worktrees are protected from uninstall cleanup",
            parsed.action == Action::Purge,
        );
    }

    report.finish();
    print_report(&report, parsed.json)?;
    report_result(&report)
}

fn parse_args(args: &[String]) -> Result<ParsedArgs, String> {
    let mut purge = false;
    let mut preview = false;
    let mut scope = None;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--purge" | "--purge-data" => {
                if purge {
                    return Err("uninstall accepts --purge or --purge-data only once".to_owned());
                }
                purge = true;
                index += 1;
            }
            "--preview" | "--dry-run" => {
                if preview {
                    return Err("uninstall accepts --preview or --dry-run only once".to_owned());
                }
                preview = true;
                index += 1;
            }
            "--json" => {
                if json {
                    return Err("uninstall accepts --json only once".to_owned());
                }
                json = true;
                index += 1;
            }
            "--scope" => {
                if scope.is_some() {
                    return Err("uninstall accepts --scope only once".to_owned());
                }
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "uninstall --scope requires a value".to_owned())?;
                scope = Some(Scope::parse(value)?);
                index += 2;
            }
            value if value.starts_with("--scope=") => {
                if scope.is_some() {
                    return Err("uninstall accepts --scope only once".to_owned());
                }
                let value = value.trim_start_matches("--scope=");
                if value.is_empty() {
                    return Err("uninstall --scope requires a value".to_owned());
                }
                scope = Some(Scope::parse(value)?);
                index += 1;
            }
            value => return Err(format!("unknown uninstall option `{value}`\n\n{}", usage())),
        }
    }

    if purge && scope.is_none() {
        return Err(
            "destructive uninstall requires an exact scope; use `--purge --scope runtime` (or preview it first)"
                .to_owned(),
        );
    }
    if !purge && !preview && scope.is_some() {
        return Err(
            "--scope requires --preview/--dry-run or --purge/--purge-data; no scoped deletion is implicit"
                .to_owned(),
        );
    }

    let action = if purge {
        if preview {
            Action::Preview
        } else {
            Action::Purge
        }
    } else if preview {
        Action::Preview
    } else {
        Action::Default
    };
    Ok(ParsedArgs {
        action,
        scope,
        json,
    })
}

fn walk_owned_path(
    path: &Path,
    relative: &str,
    action: Action,
    report: &mut UninstallReport,
    depth: usize,
) {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            report.failure(relative.to_owned(), format!("inspect path: {error}"));
            return;
        }
    };
    if metadata.file_type().is_symlink() {
        report.protected(
            relative.to_owned(),
            "symbolic links are never followed or removed by uninstall",
            action == Action::Purge,
        );
        return;
    }
    if metadata.is_file() {
        match action {
            Action::Preview => report.preserved(format!("would remove {relative}")),
            Action::Default | Action::Purge => match fs::remove_file(path) {
                Ok(()) => report.removed(relative.to_owned()),
                Err(error) => report.failure(relative.to_owned(), format!("remove file: {error}")),
            },
        }
        return;
    }
    if !metadata.is_dir() {
        report.protected(
            relative.to_owned(),
            "non-regular filesystem entries have unknown ownership",
            action == Action::Purge,
        );
        return;
    }
    if depth >= MAX_SCAN_DEPTH {
        report.failure(
            relative.to_owned(),
            format!("owned-state traversal exceeded the {MAX_SCAN_DEPTH}-level bound"),
        );
        return;
    }

    let mut children = Vec::new();
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) => {
            report.failure(relative.to_owned(), format!("read directory: {error}"));
            return;
        }
    };
    for entry in entries {
        match entry {
            Ok(entry) => {
                if children.len() >= MAX_SCAN_ENTRIES {
                    report.failure(
                        relative.to_owned(),
                        format!(
                            "owned-state traversal exceeded the {MAX_SCAN_ENTRIES}-entry bound"
                        ),
                    );
                    break;
                }
                let child = entry.path();
                let child_relative = format!("{relative}/{}", entry.file_name().to_string_lossy());
                children.push((child, child_relative));
            }
            Err(error) => report.failure(
                relative.to_owned(),
                format!("read directory entry: {error}"),
            ),
        }
    }
    children.sort_by(|left, right| left.1.cmp(&right.1));

    if children.is_empty() {
        match action {
            Action::Preview => report.preserved(format!("would remove {relative}")),
            Action::Default | Action::Purge => match fs::remove_dir(path) {
                Ok(()) => report.removed(relative.to_owned()),
                Err(error) => {
                    report.failure(relative.to_owned(), format!("remove directory: {error}"))
                }
            },
        }
        return;
    }

    for (child, child_relative) in children {
        walk_owned_path(&child, &child_relative, action, report, depth + 1);
    }

    if matches!(action, Action::Default | Action::Purge) {
        match fs::remove_dir(path) {
            Ok(()) => report.removed(relative.to_owned()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {}
            Err(error) => report.failure(relative.to_owned(), format!("remove directory: {error}")),
        }
    }
}

fn record_unmanaged_state(state_root: &Path, report: &mut UninstallReport) {
    let entries = match fs::read_dir(state_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            report.failure(
                ".medusa".to_owned(),
                format!("read state for ownership report: {error}"),
            );
            return;
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        if paths.len() >= MAX_SCAN_ENTRIES {
            report.failure(
                ".medusa".to_owned(),
                format!("ownership scan exceeded the {MAX_SCAN_ENTRIES}-entry bound"),
            );
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                report.failure(
                    ".medusa".to_owned(),
                    format!("read state entry for ownership report: {error}"),
                );
                continue;
            }
        };
        paths.push(entry.file_name().to_string_lossy().into_owned());
    }
    paths.sort();
    for name in paths {
        let relative = format!(".medusa/{name}");
        if UNINSTALL_MANIFEST
            .iter()
            .any(|entry| entry.relative == relative)
        {
            continue;
        }
        report.protected(
            relative,
            "ownership is not present in the uninstall manifest; preserving it",
            false,
        );
    }
}

fn is_linked_worktree(root: &Path) -> bool {
    fs::symlink_metadata(root.join(".git"))
        .map(|metadata| metadata.file_type().is_symlink() || metadata.is_file())
        .unwrap_or(false)
}

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
}

fn print_report(report: &UninstallReport, json: bool) -> Result<(), String> {
    if json {
        let mut value = serde_json::to_value(report)
            .map_err(|error| format!("serialize uninstall report: {error}"))?;
        if let Some(object) = value.as_object_mut() {
            object.insert(
                "preserved_durable_state".to_owned(),
                serde_json::json!(
                    report
                        .preserved
                        .iter()
                        .filter(|path| !path.starts_with("would remove "))
                        .collect::<Vec<_>>()
                ),
            );
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&value)
                .map_err(|error| format!("format uninstall report: {error}"))?
        );
        return Ok(());
    }

    println!("Medusa uninstall report");
    println!("action: {}", report.action);
    println!("scope: {}", report.scope);
    print_paths("removed", &report.removed);
    print_paths("preserved", &report.preserved);
    for item in &report.protected {
        println!("protected: {} ({})", item.path, item.reason);
    }
    for failure in &report.failures {
        println!("failure: {} ({})", failure.path, failure.reason);
    }
    if report.truncated {
        println!("report: output was bounded; additional entries were omitted");
    }
    println!(
        "result: {}",
        if report.success {
            "success"
        } else {
            "incomplete"
        }
    );
    Ok(())
}

fn print_paths(label: &str, paths: &[String]) {
    if paths.is_empty() {
        return;
    }
    println!("{label}:");
    for path in paths {
        println!("  {path}");
    }
}

fn report_result(report: &UninstallReport) -> Result<(), String> {
    if report.success {
        Ok(())
    } else {
        Err("uninstall was incomplete; inspect the bounded report before retrying".to_owned())
    }
}

fn usage() -> &'static str {
    "Usage:\n  medusa [--repo PATH] uninstall [--preview|--dry-run] [--json]\n  medusa [--repo PATH] uninstall --purge|--purge-data --scope <runtime|cache|sessions|configuration|skills|all> [--json]"
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn default_action_only_selects_ephemeral_state() {
        let runtime = ManifestEntry {
            relative: ".medusa/cache",
            group: "runtime",
            ownership: Ownership::MedusaEphemeral,
            cache: true,
        };
        let sessions = ManifestEntry {
            relative: ".medusa/sessions",
            group: "sessions",
            ownership: Ownership::UserDurable,
            cache: false,
        };
        assert!(Scope::Runtime.includes(&runtime));
        assert!(!Scope::Runtime.includes(&sessions));
    }

    #[test]
    fn purge_requires_an_exact_scope() {
        assert!(parse_args(&["--purge".to_owned()]).is_err());
        assert!(parse_args(&["--purge-data".to_owned()]).is_err());
        assert!(
            parse_args(&[
                "--purge".to_owned(),
                "--scope".to_owned(),
                "runtime".to_owned()
            ])
            .is_ok()
        );
    }

    #[test]
    fn scope_without_an_action_is_not_implicit_deletion() {
        let error = parse_args(&["--scope".to_owned(), "skills".to_owned()])
            .expect_err("scope must select preview or purge");
        assert!(error.contains("requires"));
    }

    #[test]
    fn preview_does_not_delete_and_reports_unknown_state() {
        let directory = tempfile::tempdir().expect("workspace");
        fs::create_dir_all(directory.path().join(".medusa/cache")).expect("cache");
        fs::write(directory.path().join(".medusa/cache/value"), "ephemeral").expect("cache file");
        fs::write(directory.path().join(".medusa/user-owned.json"), "keep").expect("unknown");

        run(
            directory.path(),
            &["--preview".to_owned(), "--json".to_owned()],
        )
        .expect("preview");

        assert!(directory.path().join(".medusa/cache/value").exists());
        assert!(directory.path().join(".medusa/user-owned.json").exists());
    }

    #[test]
    fn default_cleanup_preserves_durable_configuration_and_skills() {
        let directory = tempfile::tempdir().expect("workspace");
        fs::create_dir_all(directory.path().join(".medusa/cache")).expect("cache");
        fs::write(directory.path().join(".medusa/cache/value"), "ephemeral").expect("cache file");
        fs::create_dir_all(directory.path().join(".medusa/sessions")).expect("sessions");
        fs::write(
            directory.path().join(".medusa/sessions/session.json"),
            "session",
        )
        .expect("session");
        fs::write(
            directory.path().join(".medusa/config.toml"),
            "version = 1\n",
        )
        .expect("config");
        fs::create_dir_all(directory.path().join(".medusa/skills/custom")).expect("skills");
        fs::write(
            directory.path().join(".medusa/skills/custom/SKILL.md"),
            "user skill",
        )
        .expect("skill");

        run(directory.path(), &[]).expect("default uninstall");

        assert!(!directory.path().join(".medusa/cache").exists());
        assert!(
            directory
                .path()
                .join(".medusa/sessions/session.json")
                .exists()
        );
        assert!(directory.path().join(".medusa/config.toml").exists());
        assert!(
            directory
                .path()
                .join(".medusa/skills/custom/SKILL.md")
                .exists()
        );
    }

    #[test]
    fn explicit_scope_can_preview_and_purge_durable_state() {
        let directory = tempfile::tempdir().expect("workspace");
        fs::create_dir_all(directory.path().join(".medusa/skills/custom")).expect("skills");
        fs::write(
            directory.path().join(".medusa/skills/custom/SKILL.md"),
            "user skill",
        )
        .expect("skill");

        run(
            directory.path(),
            &[
                "--preview".to_owned(),
                "--scope".to_owned(),
                "skills".to_owned(),
            ],
        )
        .expect("preview");
        assert!(
            directory
                .path()
                .join(".medusa/skills/custom/SKILL.md")
                .exists()
        );

        run(
            directory.path(),
            &[
                "--purge-data".to_owned(),
                "--scope".to_owned(),
                "skills".to_owned(),
            ],
        )
        .expect("purge");
        assert!(!directory.path().join(".medusa/skills").exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_owned_state_is_protected_without_following_it() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("workspace");
        let outside = tempfile::tempdir().expect("outside");
        let outside_file = outside.path().join("must-survive");
        fs::write(&outside_file, "outside").expect("outside file");
        fs::create_dir_all(directory.path().join(".medusa")).expect("state");
        symlink(&outside_file, directory.path().join(".medusa/cache")).expect("symlink");

        assert!(
            run(
                directory.path(),
                &[
                    "--purge".to_owned(),
                    "--scope".to_owned(),
                    "runtime".to_owned(),
                ],
            )
            .is_err()
        );
        assert!(outside_file.exists());
        assert!(directory.path().join(".medusa/cache").exists());
    }

    #[test]
    fn linked_worktree_is_protected_and_safe_siblings_are_not_touched() {
        let directory = tempfile::tempdir().expect("workspace");
        fs::write(
            directory.path().join(".git"),
            "gitdir: ../main/.git/worktrees/child\n",
        )
        .expect("linked worktree marker");
        fs::create_dir_all(directory.path().join(".medusa/cache")).expect("cache");
        fs::write(directory.path().join(".medusa/cache/value"), "ephemeral").expect("cache");

        assert!(
            run(
                directory.path(),
                &[
                    "--purge".to_owned(),
                    "--scope".to_owned(),
                    "runtime".to_owned(),
                ],
            )
            .is_err()
        );
        assert!(directory.path().join(".medusa/cache/value").exists());
    }

    #[test]
    fn report_is_bounded_for_large_owned_trees() {
        let directory = tempfile::tempdir().expect("workspace");
        let cache = directory.path().join(".medusa/cache");
        fs::create_dir_all(&cache).expect("cache");
        for index in 0..(MAX_REPORT_ITEMS + 20) {
            fs::write(cache.join(format!("entry-{index}")), "value").expect("entry");
        }

        run(
            directory.path(),
            &["--preview".to_owned(), "--json".to_owned()],
        )
        .expect("bounded preview");
    }

    #[test]
    fn scope_aliases_have_stable_canonical_names() {
        assert_eq!(
            Scope::parse("session").expect("session scope").as_str(),
            "sessions"
        );
        assert_eq!(
            Scope::parse("config")
                .expect("configuration scope")
                .as_str(),
            "configuration"
        );
    }
}
