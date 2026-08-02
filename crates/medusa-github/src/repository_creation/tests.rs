use super::*;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug)]
struct RecordedCall {
    program: String,
    arguments: Vec<String>,
    directory: Option<PathBuf>,
}

#[derive(Clone, Debug)]
struct ScriptedExecutor {
    calls: Arc<Mutex<Vec<RecordedCall>>>,
    outputs: Arc<Mutex<VecDeque<CommandOutput>>>,
}

impl ScriptedExecutor {
    fn new(outputs: impl IntoIterator<Item = CommandOutput>) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            outputs: Arc::new(Mutex::new(outputs.into_iter().collect())),
        }
    }

    fn successful() -> CommandOutput {
        CommandOutput {
            success: true,
            stdout: "ok".into(),
            stderr: String::new(),
        }
    }

    fn stdout(value: impl Into<String>) -> CommandOutput {
        CommandOutput {
            success: true,
            stdout: value.into(),
            stderr: String::new(),
        }
    }

    fn failed(stderr: &str) -> CommandOutput {
        CommandOutput {
            success: false,
            stdout: String::new(),
            stderr: stderr.into(),
        }
    }
}

impl CommandExecutor for ScriptedExecutor {
    fn run(
        &self,
        program: &str,
        arguments: &[String],
        directory: Option<&Path>,
    ) -> MedusaResult<CommandOutput> {
        self.calls.lock().expect("calls").push(RecordedCall {
            program: program.into(),
            arguments: arguments.into(),
            directory: directory.map(Path::to_path_buf),
        });
        self.outputs
            .lock()
            .expect("outputs")
            .pop_front()
            .ok_or_else(|| internal_error("test command script exhausted"))
    }
}

fn request() -> RepositoryCreateRequest {
    RepositoryCreateRequest {
        owner: "acme".into(),
        name: "project".into(),
        visibility: RepositoryVisibility::Private,
        description: Some("A project".into()),
        homepage: Some("https://example.test/project".into()),
        default_branch: "main".into(),
        add_readme: true,
        gitignore_template: Some("Rust".into()),
        license_template: Some("mit".into()),
        issues_enabled: true,
        wiki_enabled: false,
        template_repository: None,
        include_all_template_branches: false,
        reuse_existing: false,
        bootstrap: None,
    }
}

fn service(executor: ScriptedExecutor) -> GitHubService<ScriptedExecutor> {
    GitHubService::enterprise("acme/project", "github.example", None, executor)
}

#[test]
fn validates_repository_creation_inputs_and_incompatible_options() {
    let mut value = request();
    value.owner = "bad owner".into();
    assert_eq!(
        value.validate().expect_err("owner").code,
        ErrorCode::InvalidInput
    );
    value.owner = "acme".into();
    for branch in ["../main", "main.lock", "/main", "foo//bar", "foo/.bar", "@"] {
        value.default_branch = branch.into();
        assert!(
            value.validate().is_err(),
            "accepted invalid branch {branch}"
        );
    }
    value.default_branch = "main".into();
    value.template_repository = Some("acme/template".into());
    assert!(value.validate().is_err());
}

#[test]
fn creates_initialized_enterprise_repository_with_typed_arguments() {
    let executor = ScriptedExecutor::new([
        ScriptedExecutor::successful(),
        ScriptedExecutor::failed("HTTP 404: Not Found"),
        ScriptedExecutor::successful(),
        ScriptedExecutor::stdout(
            "acme/project\thttps://github.example/acme/project\tPRIVATE\tmain",
        ),
        ScriptedExecutor::successful(),
        ScriptedExecutor::stdout(
            "acme/project\thttps://github.example/acme/project\tPRIVATE\tmain",
        ),
    ]);
    let github = service(executor.clone());
    let receipt = github.create_repository(&request()).expect("create");
    assert!(receipt.created);
    assert_eq!(receipt.repository, "acme/project");
    assert_eq!(receipt.clone_url, "https://github.example/acme/project.git");
    let calls = executor.calls.lock().expect("calls");
    let create = calls
        .iter()
        .find(|call| {
            call.arguments
                .starts_with(&["repo".into(), "create".into()])
        })
        .expect("create call");
    assert_eq!(create.program, "gh");
    assert!(
        create
            .arguments
            .contains(&"github.example/acme/project".into())
    );
    assert!(create.arguments.contains(&"--private".into()));
    assert!(create.arguments.contains(&"--add-readme".into()));
    assert!(create.arguments.contains(&"--disable-wiki".into()));
    assert!(
        calls
            .iter()
            .all(|call| call.program != "sh" && call.program != "cmd")
    );
}

#[test]
fn initialized_repository_post_create_failure_reports_recovery_url() {
    let executor = ScriptedExecutor::new([
        ScriptedExecutor::successful(),
        ScriptedExecutor::failed("HTTP 404: Not Found"),
        ScriptedExecutor::successful(),
        ScriptedExecutor::failed("temporary API timeout"),
    ]);
    let error = service(executor)
        .create_repository(&request())
        .expect_err("post-create inspection must retain recovery details");
    assert!(
        error
            .message
            .contains("https://github.example/acme/project")
    );
    assert!(error.message.contains("reuse_existing=true"));
    assert!(error.retryable);
}

#[test]
fn local_creation_preserves_an_expected_origin_and_pushes_after_remote_creation() {
    let directory = tempfile::tempdir().expect("tempdir");
    let root = directory.path().canonicalize().expect("canonical root");
    let remote = "https://github.example/acme/project.git";
    let executor = ScriptedExecutor::new([
        ScriptedExecutor::successful(),
        ScriptedExecutor::failed("HTTP 404: Not Found"),
        ScriptedExecutor::successful(),
        ScriptedExecutor::stdout(root.to_string_lossy()),
        ScriptedExecutor::stdout(remote),
        ScriptedExecutor::successful(),
        ScriptedExecutor::successful(),
        ScriptedExecutor::successful(),
        ScriptedExecutor::stdout(remote),
        ScriptedExecutor::successful(),
        ScriptedExecutor::successful(),
        ScriptedExecutor::stdout(
            "acme/project\thttps://github.example/acme/project\tPRIVATE\tmain",
        ),
        ScriptedExecutor::stdout("abc123"),
    ]);
    let mut value = request();
    value.add_readme = false;
    value.gitignore_template = None;
    value.license_template = None;
    value.bootstrap = Some(RepositoryBootstrap {
        path: directory.path().to_path_buf(),
        initialize_git: false,
        initial_commit_message: None,
        push: true,
    });
    let receipt = service(executor.clone())
        .create_repository(&value)
        .expect("create from local repository");
    assert_eq!(receipt.initial_commit.as_deref(), Some("abc123"));
    let calls = executor.calls.lock().expect("calls");
    let create = calls
        .iter()
        .find(|call| {
            call.arguments
                .starts_with(&["repo".into(), "create".into()])
        })
        .expect("create call");
    assert!(!create.arguments.contains(&"--source".into()));
    assert!(!create.arguments.contains(&"--remote".into()));
    assert!(!create.arguments.contains(&"--push".into()));
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.arguments.first().is_some_and(|arg| arg == "push"))
            .count(),
        1
    );
}

#[test]
fn nested_worktree_source_is_rejected_before_branch_or_remote_mutation() {
    let parent = tempfile::tempdir().expect("parent");
    let child = parent.path().join("child");
    std::fs::create_dir(&child).expect("child");
    let parent_root = parent.path().canonicalize().expect("parent root");
    let executor = ScriptedExecutor::new([
        ScriptedExecutor::successful(),
        ScriptedExecutor::failed("HTTP 404: Not Found"),
        ScriptedExecutor::successful(),
        ScriptedExecutor::stdout(parent_root.to_string_lossy()),
    ]);
    let mut value = request();
    value.add_readme = false;
    value.gitignore_template = None;
    value.license_template = None;
    value.bootstrap = Some(RepositoryBootstrap {
        path: child,
        initialize_git: false,
        initial_commit_message: None,
        push: true,
    });
    let error = service(executor.clone())
        .create_repository(&value)
        .expect_err("nested source");
    assert_eq!(error.code, ErrorCode::PolicyDenied);
    let calls = executor.calls.lock().expect("calls");
    assert!(!calls.iter().any(|call| {
        call.arguments.starts_with(&["branch".into(), "-M".into()])
            || call
                .arguments
                .starts_with(&["repo".into(), "create".into()])
    }));
}

#[test]
fn existing_repository_requires_explicit_reuse() {
    let executor = ScriptedExecutor::new([
        ScriptedExecutor::successful(),
        ScriptedExecutor::stdout(
            "acme/project\thttps://github.example/acme/project\tPRIVATE\tmain",
        ),
    ]);
    let error = service(executor)
        .create_repository(&request())
        .expect_err("must reject existing");
    assert_eq!(error.code, ErrorCode::InvalidInput);
    assert!(error.message.contains("reuse_existing"));
}

#[test]
fn unauthenticated_creation_fails_before_repository_probe() {
    let executor = ScriptedExecutor::new([ScriptedExecutor::failed("not logged in")]);
    let error = service(executor.clone())
        .create_repository(&request())
        .expect_err("authentication");
    assert_eq!(error.code, ErrorCode::PolicyDenied);
    assert_eq!(executor.calls.lock().expect("calls").len(), 1);
}

#[test]
fn repository_clone_uses_enterprise_url_without_shell_interpolation() {
    let executor = ScriptedExecutor::new([ScriptedExecutor::successful()]);
    service(executor.clone())
        .clone(Path::new("checkout"))
        .expect("clone");
    let calls = executor.calls.lock().expect("calls");
    assert_eq!(calls[0].program, "git");
    assert_eq!(
        calls[0].arguments[1],
        "https://github.example/acme/project.git"
    );
}

#[test]
fn existing_operations_remain_typed_and_available() {
    let executor = ScriptedExecutor::new(std::iter::repeat_n(ScriptedExecutor::successful(), 18));
    let github = service(executor.clone());
    github.fetch().expect("fetch");
    github.pull().expect("pull");
    github.push().expect("push");
    github.checkout("main").expect("checkout");
    github.branches().expect("branches");
    github.tags().expect("tags");
    github
        .create_pr("title", "body", "main", Some("feature"))
        .expect("create pr");
    github
        .update_pr(7, Some("updated"), Some("details"))
        .expect("update pr");
    github.review_pr(7, "looks good", false).expect("review");
    github.merge_pr(7, MergeStrategy::Squash).expect("merge");
    github.close_pr(7).expect("close");
    github.create_issue("bug", "details").expect("issue");
    github.comment_issue(8, "triaged").expect("comment");
    github.assign_issue(8, "octocat").expect("assign");
    github.label_issue(8, "bug").expect("label");
    github.milestone_issue(8, "v1").expect("milestone");
    github.watch_workflow(99).expect("watch");
    github.download_workflow_logs(99).expect("logs");
    let calls = executor.calls.lock().expect("calls");
    assert!(
        calls
            .iter()
            .any(|call| call.arguments.contains(&"--head".into()))
    );
    assert!(
        calls
            .iter()
            .any(|call| call.arguments.contains(&"--squash".into()))
    );
    assert!(
        calls
            .iter()
            .any(|call| call.arguments.contains(&"--exit-status".into()))
    );
}

#[test]
fn credential_like_external_errors_are_redacted() {
    assert!(!sanitize_external_error("authorization: Bearer ghp_secret").contains("ghp_secret"));
}

#[test]
fn recorded_directory_is_preserved_for_git_commands() {
    let executor = ScriptedExecutor::new([ScriptedExecutor::successful()]);
    let directory = PathBuf::from("checkout");
    let github = GitHubService::enterprise(
        "acme/project",
        "github.example",
        Some(directory.clone()),
        executor.clone(),
    );
    github.fetch().expect("fetch");
    assert_eq!(
        executor.calls.lock().expect("calls")[0].directory,
        Some(directory)
    );
}
