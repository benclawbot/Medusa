use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use medusa_core::{ErrorCode, MedusaResult};
use medusa_github::{CommandExecutor, CommandOutput, GitHubService, MergeStrategy};

type RecordedCommands = Arc<Mutex<Vec<(String, Vec<String>)>>>;

#[derive(Clone, Default, Debug)]
struct FakeExecutor(RecordedCommands);

impl CommandExecutor for FakeExecutor {
    fn run(
        &self,
        program: &str,
        arguments: &[String],
        _: Option<&Path>,
    ) -> MedusaResult<CommandOutput> {
        self.0
            .lock()
            .expect("lock")
            .push((program.into(), arguments.into()));
        Ok(CommandOutput {
            success: true,
            stdout: "ok".into(),
            stderr: String::new(),
        })
    }
}

fn service(fake: FakeExecutor) -> GitHubService<FakeExecutor> {
    GitHubService::enterprise("acme/medusa", "github.example", None, fake)
}

#[test]
fn device_flow_targets_enterprise_host_and_secure_store() {
    let fake = FakeExecutor::default();
    let status = service(fake.clone())
        .authenticate_device_flow()
        .expect("login");
    assert!(status.authenticated);
    assert_eq!(status.hostname, "github.example");
    let calls = fake.0.lock().expect("lock");
    assert_eq!(calls[0].0, "gh");
    assert!(
        calls[0]
            .1
            .windows(2)
            .any(|pair| pair == ["--hostname", "github.example"])
    );
    assert!(calls[0].1.contains(&"--web".into()));
}

#[test]
fn pull_request_and_actions_lifecycle_use_typed_commands() {
    let fake = FakeExecutor::default();
    let github = service(fake.clone());
    github.merge_pr(42, MergeStrategy::Squash).expect("merge");
    github.rerun_failed_jobs(99).expect("rerun");
    github.cancel_workflow(99).expect("cancel");
    let calls = fake.0.lock().expect("lock");
    assert!(calls[0].1.contains(&"--squash".into()));
    assert!(calls[0].1.contains(&"--delete-branch".into()));
    assert!(calls[1].1.contains(&"--failed".into()));
    assert_eq!(calls[2].1[1], "cancel");
}

#[test]
fn repository_clone_uses_enterprise_url_without_shell_interpolation() {
    let fake = FakeExecutor::default();
    service(fake.clone())
        .clone(Path::new("checkout"))
        .expect("clone");
    let calls = fake.0.lock().expect("lock");
    assert_eq!(calls[0].0, "git");
    assert_eq!(calls[0].1[1], "https://github.example/acme/medusa.git");
}

#[test]
fn every_repository_pr_issue_and_actions_operation_routes_through_the_service() {
    let fake = FakeExecutor::default();
    let github = service(fake.clone());
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
    github
        .review_pr(7, "looks good", false)
        .expect("comment review");
    github.close_pr(7).expect("close pr");
    github.create_issue("bug", "details").expect("create issue");
    github.comment_issue(8, "triaged").expect("comment issue");
    github.assign_issue(8, "octocat").expect("assign issue");
    github.label_issue(8, "bug").expect("label issue");
    github.milestone_issue(8, "v1").expect("milestone issue");
    github.watch_workflow(99).expect("watch workflow");
    github.download_workflow_logs(99).expect("logs");
    let calls = fake.0.lock().expect("lock");
    assert!(
        calls
            .iter()
            .any(|(_, args)| args.contains(&"--head".into()))
    );
    assert!(
        calls
            .iter()
            .any(|(_, args)| args.contains(&"--comment".into()))
    );
    assert!(
        calls
            .iter()
            .any(|(_, args)| args.contains(&"--add-label".into()))
    );
    assert!(
        calls
            .iter()
            .any(|(_, args)| args.contains(&"--exit-status".into()))
    );
    assert!(calls.iter().any(|(_, args)| args.contains(&"--log".into())));
}

struct FailingExecutor;

impl CommandExecutor for FailingExecutor {
    fn run(&self, _: &str, _: &[String], _: Option<&Path>) -> MedusaResult<CommandOutput> {
        Ok(CommandOutput {
            success: false,
            stdout: String::new(),
            stderr: "denied".into(),
        })
    }
}

#[test]
fn failed_external_command_is_a_structured_execution_error() {
    let github = GitHubService::enterprise("acme/medusa", "github.example", None, FailingExecutor);
    let error = github.fetch().expect_err("failed git command");
    assert_eq!(error.code, ErrorCode::ToolExecutionFailed);
    assert!(error.message.contains("denied"));
}
