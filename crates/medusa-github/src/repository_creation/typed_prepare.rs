use std::collections::BTreeMap;

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde_json::{Map, Value, json};

use super::*;

pub(super) fn prepare(document: &GitHubOperationDocument) -> MedusaResult<PreparedGitHubOperation> {
    let digest = document.request_digest()?;
    let prepared = match &document.operation {
        GitHubTypedOperation::Repository(operation) => prepare_repository(document, operation),
        GitHubTypedOperation::Contents(operation) => prepare_contents(document, operation),
        GitHubTypedOperation::GitData(operation) => prepare_git_data(document, operation),
        GitHubTypedOperation::Issues(operation) => prepare_issues(document, operation, &digest),
        GitHubTypedOperation::PullRequests(operation) => {
            prepare_pull_requests(document, operation, &digest)
        }
        GitHubTypedOperation::Actions(operation) => prepare_actions(document, operation),
        GitHubTypedOperation::Releases(operation) => prepare_releases(document, operation),
        GitHubTypedOperation::Collaborators(operation) => {
            prepare_collaborators(document, operation)
        }
        GitHubTypedOperation::Environments(operation) => {
            prepare_environments(document, operation)
        }
        GitHubTypedOperation::Variables(operation) => prepare_variables(document, operation),
        GitHubTypedOperation::Secrets(operation) => prepare_secrets(document, operation),
        GitHubTypedOperation::Webhooks(operation) => prepare_webhooks(document, operation),
        GitHubTypedOperation::BranchProtection(operation) => {
            prepare_branch_protection(document, operation)
        }
        GitHubTypedOperation::Projects(operation) => prepare_projects(document, operation),
        GitHubTypedOperation::Search(operation) => prepare_search(document, operation),
    }?;
    Ok(prepared)
}

fn prepare_repository(
    document: &GitHubOperationDocument,
    operation: &RepositoryOperation,
) -> MedusaResult<PreparedGitHubOperation> {
    match operation {
        RepositoryOperation::Get => api(
            document,
            GitHubResource::Repository,
            "get",
            GitHubHttpMethod::Get,
            "",
            BTreeMap::new(),
            None,
            true,
            None,
        ),
        RepositoryOperation::Update {
            description,
            homepage,
            visibility,
            default_branch,
            has_issues,
            has_wiki,
            archived,
        } => {
            if let Some(visibility) = visibility.as_deref()
                && !matches!(visibility, "public" | "private" | "internal")
            {
                return Err(invalid("repository visibility must be public, private, or internal"));
            }
            let body = object([
                optional("description", description.clone()),
                optional("homepage", homepage.clone()),
                optional("visibility", visibility.clone()),
                optional("default_branch", default_branch.clone()),
                optional("has_issues", *has_issues),
                optional("has_wiki", *has_wiki),
                optional("archived", *archived),
            ]);
            api(
                document,
                GitHubResource::Repository,
                "update",
                GitHubHttpMethod::Patch,
                "",
                BTreeMap::new(),
                Some(body),
                true,
                None,
            )
        }
        RepositoryOperation::Delete => api(
            document,
            GitHubResource::Repository,
            "delete",
            GitHubHttpMethod::Delete,
            "",
            BTreeMap::new(),
            None,
            true,
            Some(GitHubReconciliation::ResourceAbsent {
                endpoint: String::new(),
            }),
        ),
        RepositoryOperation::GetTopics => api(
            document,
            GitHubResource::Repository,
            "get_topics",
            GitHubHttpMethod::Get,
            "topics",
            BTreeMap::new(),
            None,
            true,
            None,
        ),
        RepositoryOperation::ReplaceTopics { names } => {
            validate_string_list("topic", names, 20, 50)?;
            api(
                document,
                GitHubResource::Repository,
                "replace_topics",
                GitHubHttpMethod::Put,
                "topics",
                BTreeMap::new(),
                Some(json!({"names": names})),
                true,
                None,
            )
        }
        RepositoryOperation::Create { request } => {
            if request.full_name()? != document.repository {
                return Err(invalid(
                    "repository create request must match the operation document repository",
                ));
            }
            Ok(PreparedGitHubOperation {
                operation_kind: "repository.create".into(),
                risk: GitHubOperationRisk::Administration,
                idempotent: request.reuse_existing,
                reconciliation: None,
                execution: PreparedExecution::RepositoryCreate(request.clone()),
            })
        }
    }
}

fn prepare_contents(
    document: &GitHubOperationDocument,
    operation: &ContentsOperation,
) -> MedusaResult<PreparedGitHubOperation> {
    match operation {
        ContentsOperation::Get { path, reference } => {
            let path = repository_path(path)?;
            let mut query = BTreeMap::new();
            insert_query(&mut query, "ref", reference.as_deref());
            api(
                document,
                GitHubResource::Contents,
                "get",
                GitHubHttpMethod::Get,
                &format!("contents/{path}"),
                query,
                None,
                true,
                None,
            )
        }
        ContentsOperation::Put {
            path,
            message,
            content_base64,
            branch,
            sha,
        } => {
            let path = repository_path(path)?;
            require_text("commit message", message, 1, 512)?;
            require_text("base64 content", content_base64, 1, 1_048_576)?;
            let body = object([
                required("message", message),
                required("content", content_base64),
                optional("branch", branch.clone()),
                optional("sha", sha.clone()),
            ]);
            api(
                document,
                GitHubResource::Contents,
                "put",
                GitHubHttpMethod::Put,
                &format!("contents/{path}"),
                BTreeMap::new(),
                Some(body),
                true,
                Some(GitHubReconciliation::ContentMatches {
                    path,
                    branch: branch.clone(),
                }),
            )
        }
        ContentsOperation::Delete {
            path,
            message,
            sha,
            branch,
        } => {
            let path = repository_path(path)?;
            require_text("commit message", message, 1, 512)?;
            require_sha(sha)?;
            let body = object([
                required("message", message),
                required("sha", sha),
                optional("branch", branch.clone()),
            ]);
            api(
                document,
                GitHubResource::Contents,
                "delete",
                GitHubHttpMethod::Delete,
                &format!("contents/{path}"),
                BTreeMap::new(),
                Some(body),
                true,
                Some(GitHubReconciliation::ContentAbsent {
                    path,
                    branch: branch.clone(),
                }),
            )
        }
    }
}

fn prepare_git_data(
    document: &GitHubOperationDocument,
    operation: &GitDataOperation,
) -> MedusaResult<PreparedGitHubOperation> {
    match operation {
        GitDataOperation::ListBranches {
            protected,
            per_page,
        } => {
            let mut query = page_query(*per_page)?;
            if let Some(value) = protected {
                query.insert("protected".into(), value.to_string());
            }
            api(
                document,
                GitHubResource::GitData,
                "list_branches",
                GitHubHttpMethod::Get,
                "branches",
                query,
                None,
                true,
                None,
            )
        }
        GitDataOperation::GetBranch { branch } => {
            let branch = path_component("branch", branch)?;
            api(
                document,
                GitHubResource::GitData,
                "get_branch",
                GitHubHttpMethod::Get,
                &format!("branches/{branch}"),
                BTreeMap::new(),
                None,
                true,
                None,
            )
        }
        GitDataOperation::CreateReference { reference, sha } => {
            let reference = git_reference(reference)?;
            require_sha(sha)?;
            api(
                document,
                GitHubResource::GitData,
                "create_reference",
                GitHubHttpMethod::Post,
                "git/refs",
                BTreeMap::new(),
                Some(json!({"ref": reference, "sha": sha})),
                true,
                Some(GitHubReconciliation::BranchExists {
                    reference,
                    sha: sha.clone(),
                }),
            )
        }
        GitDataOperation::UpdateReference {
            reference,
            sha,
            force,
        } => {
            let reference = git_reference(reference)?;
            require_sha(sha)?;
            let endpoint_ref = reference.trim_start_matches("refs/");
            api(
                document,
                GitHubResource::GitData,
                "update_reference",
                GitHubHttpMethod::Patch,
                &format!("git/refs/{endpoint_ref}"),
                BTreeMap::new(),
                Some(json!({"sha": sha, "force": force})),
                true,
                Some(GitHubReconciliation::BranchExists {
                    reference,
                    sha: sha.clone(),
                }),
            )
        }
        GitDataOperation::DeleteReference { reference } => {
            let reference = git_reference(reference)?;
            let endpoint_ref = reference.trim_start_matches("refs/");
            api(
                document,
                GitHubResource::GitData,
                "delete_reference",
                GitHubHttpMethod::Delete,
                &format!("git/refs/{endpoint_ref}"),
                BTreeMap::new(),
                None,
                true,
                Some(GitHubReconciliation::ResourceAbsent {
                    endpoint: format!("git/ref/{endpoint_ref}"),
                }),
            )
        }
        GitDataOperation::GetCommit { sha } => {
            require_sha(sha)?;
            api(
                document,
                GitHubResource::GitData,
                "get_commit",
                GitHubHttpMethod::Get,
                &format!("commits/{sha}"),
                BTreeMap::new(),
                None,
                true,
                None,
            )
        }
        GitDataOperation::ListCommits {
            sha,
            path,
            per_page,
        } => {
            let mut query = page_query(*per_page)?;
            insert_query(&mut query, "sha", sha.as_deref());
            if let Some(path) = path {
                query.insert("path".into(), repository_path(path)?);
            }
            api(
                document,
                GitHubResource::GitData,
                "list_commits",
                GitHubHttpMethod::Get,
                "commits",
                query,
                None,
                true,
                None,
            )
        }
    }
}

fn prepare_issues(
    document: &GitHubOperationDocument,
    operation: &IssuesOperation,
    digest: &str,
) -> MedusaResult<PreparedGitHubOperation> {
    match operation {
        IssuesOperation::List {
            state,
            labels,
            assignee,
            per_page,
        } => {
            let mut query = page_query(*per_page)?;
            insert_query(&mut query, "state", state.as_deref());
            if !labels.is_empty() {
                validate_string_list("label", labels, 100, 100)?;
                query.insert("labels".into(), labels.join(","));
            }
            insert_query(&mut query, "assignee", assignee.as_deref());
            api(
                document,
                GitHubResource::Issues,
                "list",
                GitHubHttpMethod::Get,
                "issues",
                query,
                None,
                true,
                None,
            )
        }
        IssuesOperation::Get { number } => numbered_get(document, GitHubResource::Issues, "get", "issues", *number),
        IssuesOperation::Create {
            title,
            body,
            labels,
            assignees,
            milestone,
        } => {
            require_text("issue title", title, 1, 256)?;
            validate_string_list("label", labels, 100, 100)?;
            validate_string_list("assignee", assignees, 20, 100)?;
            let marker = marker(digest);
            let body = append_marker(body, &marker);
            let payload = object([
                required("title", title),
                required("body", &body),
                optional_nonempty("labels", labels),
                optional_nonempty("assignees", assignees),
                optional("milestone", *milestone),
            ]);
            api(
                document,
                GitHubResource::Issues,
                "create",
                GitHubHttpMethod::Post,
                "issues",
                BTreeMap::new(),
                Some(payload),
                true,
                Some(GitHubReconciliation::IssueMarker { marker }),
            )
        }
        IssuesOperation::Update {
            number,
            title,
            body,
            state,
            state_reason,
        } => {
            require_number(*number)?;
            let payload = object([
                optional("title", title.clone()),
                optional("body", body.clone()),
                optional("state", state.clone()),
                optional("state_reason", state_reason.clone()),
            ]);
            api(
                document,
                GitHubResource::Issues,
                "update",
                GitHubHttpMethod::Patch,
                &format!("issues/{number}"),
                BTreeMap::new(),
                Some(payload),
                true,
                None,
            )
        }
        IssuesOperation::Comment { number, body } => {
            require_number(*number)?;
            require_text("comment body", body, 1, 262_144)?;
            api(
                document,
                GitHubResource::Issues,
                "comment",
                GitHubHttpMethod::Post,
                &format!("issues/{number}/comments"),
                BTreeMap::new(),
                Some(json!({"body": body})),
                false,
                None,
            )
        }
        IssuesOperation::AddLabels { number, labels } => {
            require_number(*number)?;
            validate_string_list("label", labels, 100, 100)?;
            api(
                document,
                GitHubResource::Issues,
                "add_labels",
                GitHubHttpMethod::Post,
                &format!("issues/{number}/labels"),
                BTreeMap::new(),
                Some(json!({"labels": labels})),
                true,
                None,
            )
        }
        IssuesOperation::SetAssignees { number, assignees } => {
            require_number(*number)?;
            validate_string_list("assignee", assignees, 20, 100)?;
            api(
                document,
                GitHubResource::Issues,
                "set_assignees",
                GitHubHttpMethod::Post,
                &format!("issues/{number}/assignees"),
                BTreeMap::new(),
                Some(json!({"assignees": assignees})),
                true,
                None,
            )
        }
        IssuesOperation::Lock { number, reason } => {
            require_number(*number)?;
            if let Some(reason) = reason.as_deref()
                && !matches!(reason, "off-topic" | "too heated" | "resolved" | "spam")
            {
                return Err(invalid("lock reason is unsupported"));
            }
            api(
                document,
                GitHubResource::Issues,
                "lock",
                GitHubHttpMethod::Put,
                &format!("issues/{number}/lock"),
                BTreeMap::new(),
                reason.as_ref().map(|value| json!({"lock_reason": value})),
                true,
                None,
            )
        }
        IssuesOperation::Unlock { number } => {
            require_number(*number)?;
            api(
                document,
                GitHubResource::Issues,
                "unlock",
                GitHubHttpMethod::Delete,
                &format!("issues/{number}/lock"),
                BTreeMap::new(),
                None,
                true,
                None,
            )
        }
        IssuesOperation::DeleteComment { comment_id } => {
            require_number(*comment_id)?;
            api(
                document,
                GitHubResource::Issues,
                "delete_comment",
                GitHubHttpMethod::Delete,
                &format!("issues/comments/{comment_id}"),
                BTreeMap::new(),
                None,
                true,
                Some(GitHubReconciliation::ResourceAbsent {
                    endpoint: format!("issues/comments/{comment_id}"),
                }),
            )
        }
        IssuesOperation::ReactToComment { comment_id, content } => {
            require_number(*comment_id)?;
            validate_reaction(content)?;
            api(
                document,
                GitHubResource::Issues,
                "react_to_comment",
                GitHubHttpMethod::Post,
                &format!("issues/comments/{comment_id}/reactions"),
                BTreeMap::new(),
                Some(json!({"content": content})),
                true,
                None,
            )
        }
    }
}

fn prepare_pull_requests(
    document: &GitHubOperationDocument,
    operation: &PullRequestsOperation,
    digest: &str,
) -> MedusaResult<PreparedGitHubOperation> {
    match operation {
        PullRequestsOperation::List {
            state,
            head,
            base,
            per_page,
        } => {
            let mut query = page_query(*per_page)?;
            insert_query(&mut query, "state", state.as_deref());
            insert_query(&mut query, "head", head.as_deref());
            insert_query(&mut query, "base", base.as_deref());
            api(document, GitHubResource::PullRequests, "list", GitHubHttpMethod::Get, "pulls", query, None, true, None)
        }
        PullRequestsOperation::Get { number } => numbered_get(document, GitHubResource::PullRequests, "get", "pulls", *number),
        PullRequestsOperation::Create { title, body, head, base, draft } => {
            require_text("pull request title", title, 1, 256)?;
            require_text("pull request head", head, 1, 255)?;
            require_text("pull request base", base, 1, 255)?;
            let marker = marker(digest);
            let body = append_marker(body, &marker);
            api(
                document,
                GitHubResource::PullRequests,
                "create",
                GitHubHttpMethod::Post,
                "pulls",
                BTreeMap::new(),
                Some(json!({"title": title, "body": body, "head": head, "base": base, "draft": draft})),
                true,
                Some(GitHubReconciliation::PullRequestMarker { marker }),
            )
        }
        PullRequestsOperation::Update { number, title, body, state, base } => {
            require_number(*number)?;
            api(
                document,
                GitHubResource::PullRequests,
                "update",
                GitHubHttpMethod::Patch,
                &format!("pulls/{number}"),
                BTreeMap::new(),
                Some(object([
                    optional("title", title.clone()),
                    optional("body", body.clone()),
                    optional("state", state.clone()),
                    optional("base", base.clone()),
                ])),
                true,
                None,
            )
        }
        PullRequestsOperation::Review { number, event, body } => {
            require_number(*number)?;
            let event = match event {
                PullRequestReviewEvent::Approve => "APPROVE",
                PullRequestReviewEvent::Comment => "COMMENT",
                PullRequestReviewEvent::RequestChanges => "REQUEST_CHANGES",
            };
            api(
                document,
                GitHubResource::PullRequests,
                "review",
                GitHubHttpMethod::Post,
                &format!("pulls/{number}/reviews"),
                BTreeMap::new(),
                Some(json!({"event": event, "body": body})),
                false,
                None,
            )
        }
        PullRequestsOperation::Merge { number, strategy, commit_title, commit_message, expected_head_sha } => {
            require_number(*number)?;
            let strategy = match strategy {
                MergeStrategy::Merge => "merge",
                MergeStrategy::Squash => "squash",
                MergeStrategy::Rebase => "rebase",
            };
            api(
                document,
                GitHubResource::PullRequests,
                "merge",
                GitHubHttpMethod::Put,
                &format!("pulls/{number}/merge"),
                BTreeMap::new(),
                Some(object([
                    required("merge_method", strategy),
                    optional("commit_title", commit_title.clone()),
                    optional("commit_message", commit_message.clone()),
                    optional("sha", expected_head_sha.clone()),
                ])),
                true,
                None,
            )
        }
        PullRequestsOperation::Close { number } => {
            require_number(*number)?;
            api(document, GitHubResource::PullRequests, "close", GitHubHttpMethod::Patch, &format!("pulls/{number}"), BTreeMap::new(), Some(json!({"state":"closed"})), true, None)
        }
        PullRequestsOperation::RequestReviewers { number, reviewers, team_reviewers } => reviewers(document, *number, reviewers, team_reviewers, false),
        PullRequestsOperation::RemoveReviewers { number, reviewers, team_reviewers } => reviewers(document, *number, reviewers, team_reviewers, true),
    }
}

fn reviewers(
    document: &GitHubOperationDocument,
    number: u64,
    reviewers: &[String],
    team_reviewers: &[String],
    remove: bool,
) -> MedusaResult<PreparedGitHubOperation> {
    require_number(number)?;
    validate_string_list("reviewer", reviewers, 100, 100)?;
    validate_string_list("team reviewer", team_reviewers, 100, 100)?;
    api(
        document,
        GitHubResource::PullRequests,
        if remove { "remove_reviewers" } else { "request_reviewers" },
        if remove { GitHubHttpMethod::Delete } else { GitHubHttpMethod::Post },
        &format!("pulls/{number}/requested_reviewers"),
        BTreeMap::new(),
        Some(json!({"reviewers": reviewers, "team_reviewers": team_reviewers})),
        true,
        None,
    )
}

fn prepare_actions(
    document: &GitHubOperationDocument,
    operation: &ActionsOperation,
) -> MedusaResult<PreparedGitHubOperation> {
    match operation {
        ActionsOperation::ListRuns { workflow_id, branch, status, per_page } => {
            let mut query = page_query(*per_page)?;
            insert_query(&mut query, "branch", branch.as_deref());
            insert_query(&mut query, "status", status.as_deref());
            let endpoint = workflow_id.as_ref().map_or_else(|| "actions/runs".into(), |id| format!("actions/workflows/{}/runs", safe_component("workflow_id", id).unwrap_or_else(|_| id.clone())));
            api(document, GitHubResource::Actions, "list_runs", GitHubHttpMethod::Get, &endpoint, query, None, true, None)
        }
        ActionsOperation::GetRun { run_id } => numbered_get(document, GitHubResource::Actions, "get_run", "actions/runs", *run_id),
        ActionsOperation::ListJobs { run_id } => {
            require_number(*run_id)?;
            api(document, GitHubResource::Actions, "list_jobs", GitHubHttpMethod::Get, &format!("actions/runs/{run_id}/jobs"), BTreeMap::new(), None, true, None)
        }
        ActionsOperation::GetJob { job_id } => numbered_get(document, GitHubResource::Actions, "get_job", "actions/jobs", *job_id),
        ActionsOperation::Rerun { run_id } => action_post(document, "rerun", &format!("actions/runs/{run_id}/rerun"), *run_id),
        ActionsOperation::RerunFailed { run_id } => action_post(document, "rerun_failed", &format!("actions/runs/{run_id}/rerun-failed-jobs"), *run_id),
        ActionsOperation::Cancel { run_id } => action_post(document, "cancel", &format!("actions/runs/{run_id}/cancel"), *run_id),
        ActionsOperation::DownloadArtifact { artifact_id, destination, max_bytes } => {
            require_number(*artifact_id)?;
            Ok(PreparedGitHubOperation {
                operation_kind: "actions.download_artifact".into(),
                risk: GitHubOperationRisk::ReadOnly,
                idempotent: true,
                reconciliation: None,
                execution: PreparedExecution::ArtifactDownload(ArtifactDownload {
                    endpoint: format!("actions/artifacts/{artifact_id}/zip"),
                    destination: destination.clone(),
                    max_bytes: *max_bytes,
                    accept: "application/octet-stream".into(),
                }),
            })
        }
    }
}

fn prepare_releases(
    document: &GitHubOperationDocument,
    operation: &ReleasesOperation,
) -> MedusaResult<PreparedGitHubOperation> {
    match operation {
        ReleasesOperation::List { per_page } => api(document, GitHubResource::Releases, "list", GitHubHttpMethod::Get, "releases", page_query(*per_page)?, None, true, None),
        ReleasesOperation::Get { release_id } => numbered_get(document, GitHubResource::Releases, "get", "releases", *release_id),
        ReleasesOperation::GetByTag { tag } => api(document, GitHubResource::Releases, "get_by_tag", GitHubHttpMethod::Get, &format!("releases/tags/{}", path_component("tag", tag)?), BTreeMap::new(), None, true, None),
        ReleasesOperation::Create { tag, target_commitish, name, body, draft, prerelease, generate_release_notes } => {
            let tag = path_component("tag", tag)?;
            require_text("release name", name, 1, 256)?;
            api(
                document,
                GitHubResource::Releases,
                "create",
                GitHubHttpMethod::Post,
                "releases",
                BTreeMap::new(),
                Some(object([
                    required("tag_name", &tag),
                    optional("target_commitish", target_commitish.clone()),
                    required("name", name),
                    required("body", body),
                    required("draft", *draft),
                    required("prerelease", *prerelease),
                    required("generate_release_notes", *generate_release_notes),
                ])),
                true,
                Some(GitHubReconciliation::ReleaseTag { tag }),
            )
        }
        ReleasesOperation::Update { release_id, tag, name, body, draft, prerelease } => {
            require_number(*release_id)?;
            api(document, GitHubResource::Releases, "update", GitHubHttpMethod::Patch, &format!("releases/{release_id}"), BTreeMap::new(), Some(object([
                optional("tag_name", tag.clone()), optional("name", name.clone()), optional("body", body.clone()), optional("draft", *draft), optional("prerelease", *prerelease)
            ])), true, None)
        }
        ReleasesOperation::Delete { release_id } => {
            require_number(*release_id)?;
            api(document, GitHubResource::Releases, "delete", GitHubHttpMethod::Delete, &format!("releases/{release_id}"), BTreeMap::new(), None, true, Some(GitHubReconciliation::ResourceAbsent { endpoint: format!("releases/{release_id}") }))
        }
        ReleasesOperation::UploadAsset { release_id, path, name, label, content_type, max_bytes } => {
            require_number(*release_id)?;
            Ok(PreparedGitHubOperation {
                operation_kind: "releases.upload_asset".into(),
                risk: GitHubOperationRisk::Mutation,
                idempotent: document.idempotency_key.is_some(),
                reconciliation: None,
                execution: PreparedExecution::ReleaseAssetUpload(ReleaseAssetUpload {
                    release_id: *release_id,
                    path: path.clone(),
                    name: name.clone(),
                    label: label.clone(),
                    content_type: content_type.clone(),
                    max_bytes: *max_bytes,
                }),
            })
        }
        ReleasesOperation::DownloadAsset { asset_id, destination, max_bytes } => {
            require_number(*asset_id)?;
            Ok(PreparedGitHubOperation {
                operation_kind: "releases.download_asset".into(),
                risk: GitHubOperationRisk::ReadOnly,
                idempotent: true,
                reconciliation: None,
                execution: PreparedExecution::ArtifactDownload(ArtifactDownload {
                    endpoint: format!("releases/assets/{asset_id}"),
                    destination: destination.clone(),
                    max_bytes: *max_bytes,
                    accept: "application/octet-stream".into(),
                }),
            })
        }
        ReleasesOperation::DeleteAsset { asset_id } => {
            require_number(*asset_id)?;
            api(document, GitHubResource::Releases, "delete_asset", GitHubHttpMethod::Delete, &format!("releases/assets/{asset_id}"), BTreeMap::new(), None, true, Some(GitHubReconciliation::ResourceAbsent { endpoint: format!("releases/assets/{asset_id}") }))
        }
    }
}

fn prepare_collaborators(document: &GitHubOperationDocument, operation: &CollaboratorsOperation) -> MedusaResult<PreparedGitHubOperation> {
    match operation {
        CollaboratorsOperation::List { per_page } => api(document, GitHubResource::Collaborators, "list", GitHubHttpMethod::Get, "collaborators", page_query(*per_page)?, None, true, None),
        CollaboratorsOperation::GetPermission { username } => api(document, GitHubResource::Collaborators, "get_permission", GitHubHttpMethod::Get, &format!("collaborators/{}/permission", path_component("username", username)?), BTreeMap::new(), None, true, None),
        CollaboratorsOperation::Add { username, permission } => {
            if !matches!(permission.as_str(), "pull" | "triage" | "push" | "maintain" | "admin") { return Err(invalid("collaborator permission is unsupported")); }
            api(document, GitHubResource::Collaborators, "add", GitHubHttpMethod::Put, &format!("collaborators/{}", path_component("username", username)?), BTreeMap::new(), Some(json!({"permission":permission})), true, None)
        }
        CollaboratorsOperation::Remove { username } => api(document, GitHubResource::Collaborators, "remove", GitHubHttpMethod::Delete, &format!("collaborators/{}", path_component("username", username)?), BTreeMap::new(), None, true, None),
    }
}

fn prepare_environments(document: &GitHubOperationDocument, operation: &EnvironmentsOperation) -> MedusaResult<PreparedGitHubOperation> {
    match operation {
        EnvironmentsOperation::List { per_page } => api(document, GitHubResource::Environments, "list", GitHubHttpMethod::Get, "environments", page_query(*per_page)?, None, true, None),
        EnvironmentsOperation::Get { name } => api(document, GitHubResource::Environments, "get", GitHubHttpMethod::Get, &format!("environments/{}", path_component("environment", name)?), BTreeMap::new(), None, true, None),
        EnvironmentsOperation::Put { name, wait_timer, prevent_self_review } => api(document, GitHubResource::Environments, "put", GitHubHttpMethod::Put, &format!("environments/{}", path_component("environment", name)?), BTreeMap::new(), Some(object([
            optional("wait_timer", *wait_timer), optional("prevent_self_review", *prevent_self_review)
        ])), true, None),
        EnvironmentsOperation::Delete { name } => api(document, GitHubResource::Environments, "delete", GitHubHttpMethod::Delete, &format!("environments/{}", path_component("environment", name)?), BTreeMap::new(), None, true, None),
    }
}

fn prepare_variables(document: &GitHubOperationDocument, operation: &VariablesOperation) -> MedusaResult<PreparedGitHubOperation> {
    match operation {
        VariablesOperation::ListActions { per_page } => api(document, GitHubResource::Variables, "list_actions", GitHubHttpMethod::Get, "actions/variables", page_query(*per_page)?, None, true, None),
        VariablesOperation::GetActions { name } => api(document, GitHubResource::Variables, "get_actions", GitHubHttpMethod::Get, &format!("actions/variables/{}", path_component("variable", name)?), BTreeMap::new(), None, true, None),
        VariablesOperation::PutActions { name, value } => api(document, GitHubResource::Variables, "put_actions", GitHubHttpMethod::Patch, &format!("actions/variables/{}", path_component("variable", name)?), BTreeMap::new(), Some(json!({"name":name,"value":value})), true, None),
        VariablesOperation::DeleteActions { name } => api(document, GitHubResource::Variables, "delete_actions", GitHubHttpMethod::Delete, &format!("actions/variables/{}", path_component("variable", name)?), BTreeMap::new(), None, true, None),
        VariablesOperation::ListEnvironment { environment, per_page } => api(document, GitHubResource::Variables, "list_environment", GitHubHttpMethod::Get, &format!("environments/{}/variables", path_component("environment", environment)?), page_query(*per_page)?, None, true, None),
        VariablesOperation::GetEnvironment { environment, name } => api(document, GitHubResource::Variables, "get_environment", GitHubHttpMethod::Get, &format!("environments/{}/variables/{}", path_component("environment", environment)?, path_component("variable", name)?), BTreeMap::new(), None, true, None),
        VariablesOperation::PutEnvironment { environment, name, value } => api(document, GitHubResource::Variables, "put_environment", GitHubHttpMethod::Patch, &format!("environments/{}/variables/{}", path_component("environment", environment)?, path_component("variable", name)?), BTreeMap::new(), Some(json!({"name":name,"value":value})), true, None),
        VariablesOperation::DeleteEnvironment { environment, name } => api(document, GitHubResource::Variables, "delete_environment", GitHubHttpMethod::Delete, &format!("environments/{}/variables/{}", path_component("environment", environment)?, path_component("variable", name)?), BTreeMap::new(), None, true, None),
    }
}

fn prepare_secrets(document: &GitHubOperationDocument, operation: &SecretsOperation) -> MedusaResult<PreparedGitHubOperation> {
    match operation {
        SecretsOperation::ListActions { per_page } => api(document, GitHubResource::Secrets, "list_actions", GitHubHttpMethod::Get, "actions/secrets", page_query(*per_page)?, None, true, None),
        SecretsOperation::GetActionsPublicKey => api(document, GitHubResource::Secrets, "get_actions_public_key", GitHubHttpMethod::Get, "actions/secrets/public-key", BTreeMap::new(), None, true, None),
        SecretsOperation::PutActions { name, encrypted_value, key_id } => api(document, GitHubResource::Secrets, "put_actions", GitHubHttpMethod::Put, &format!("actions/secrets/{}", path_component("secret", name)?), BTreeMap::new(), Some(json!({"encrypted_value":encrypted_value,"key_id":key_id})), true, None),
        SecretsOperation::DeleteActions { name } => api(document, GitHubResource::Secrets, "delete_actions", GitHubHttpMethod::Delete, &format!("actions/secrets/{}", path_component("secret", name)?), BTreeMap::new(), None, true, None),
        SecretsOperation::ListEnvironment { environment, per_page } => api(document, GitHubResource::Secrets, "list_environment", GitHubHttpMethod::Get, &format!("environments/{}/secrets", path_component("environment", environment)?), page_query(*per_page)?, None, true, None),
        SecretsOperation::GetEnvironmentPublicKey { environment } => api(document, GitHubResource::Secrets, "get_environment_public_key", GitHubHttpMethod::Get, &format!("environments/{}/secrets/public-key", path_component("environment", environment)?), BTreeMap::new(), None, true, None),
        SecretsOperation::PutEnvironment { environment, name, encrypted_value, key_id } => api(document, GitHubResource::Secrets, "put_environment", GitHubHttpMethod::Put, &format!("environments/{}/secrets/{}", path_component("environment", environment)?, path_component("secret", name)?), BTreeMap::new(), Some(json!({"encrypted_value":encrypted_value,"key_id":key_id})), true, None),
        SecretsOperation::DeleteEnvironment { environment, name } => api(document, GitHubResource::Secrets, "delete_environment", GitHubHttpMethod::Delete, &format!("environments/{}/secrets/{}", path_component("environment", environment)?, path_component("secret", name)?), BTreeMap::new(), None, true, None),
    }
}

fn prepare_webhooks(document: &GitHubOperationDocument, operation: &WebhooksOperation) -> MedusaResult<PreparedGitHubOperation> {
    match operation {
        WebhooksOperation::List { per_page } => api(document, GitHubResource::Webhooks, "list", GitHubHttpMethod::Get, "hooks", page_query(*per_page)?, None, true, None),
        WebhooksOperation::Get { hook_id } => numbered_get(document, GitHubResource::Webhooks, "get", "hooks", *hook_id),
        WebhooksOperation::Create { name, active, events, config } => {
            validate_string_list("webhook event", events, 100, 100)?;
            api(document, GitHubResource::Webhooks, "create", GitHubHttpMethod::Post, "hooks", BTreeMap::new(), Some(json!({"name":name,"active":active,"events":events,"config":config})), false, None)
        }
        WebhooksOperation::Update { hook_id, active, events, config } => {
            require_number(*hook_id)?;
            validate_string_list("webhook event", events, 100, 100)?;
            api(document, GitHubResource::Webhooks, "update", GitHubHttpMethod::Patch, &format!("hooks/{hook_id}"), BTreeMap::new(), Some(object([
                optional("active", *active), optional_nonempty("events", events), optional_nonempty_map("config", config)
            ])), true, None)
        }
        WebhooksOperation::Delete { hook_id } => {
            require_number(*hook_id)?;
            api(document, GitHubResource::Webhooks, "delete", GitHubHttpMethod::Delete, &format!("hooks/{hook_id}"), BTreeMap::new(), None, true, None)
        }
        WebhooksOperation::Ping { hook_id } => {
            require_number(*hook_id)?;
            api(document, GitHubResource::Webhooks, "ping", GitHubHttpMethod::Post, &format!("hooks/{hook_id}/pings"), BTreeMap::new(), None, true, None)
        }
    }
}

fn prepare_branch_protection(document: &GitHubOperationDocument, operation: &BranchProtectionOperation) -> MedusaResult<PreparedGitHubOperation> {
    match operation {
        BranchProtectionOperation::Get { branch } => api(document, GitHubResource::BranchProtection, "get", GitHubHttpMethod::Get, &format!("branches/{}/protection", path_component("branch", branch)?), BTreeMap::new(), None, true, None),
        BranchProtectionOperation::Put { branch, settings } => {
            if settings.required_approving_review_count > 6 { return Err(invalid("required_approving_review_count must be at most 6")); }
            let required_status_checks = if settings.required_status_checks.is_empty() { Value::Null } else { json!({"strict":settings.strict_status_checks,"contexts":settings.required_status_checks}) };
            let review = json!({
                "dismiss_stale_reviews": settings.dismiss_stale_reviews,
                "require_code_owner_reviews": settings.require_code_owner_reviews,
                "required_approving_review_count": settings.required_approving_review_count,
            });
            api(document, GitHubResource::BranchProtection, "put", GitHubHttpMethod::Put, &format!("branches/{}/protection", path_component("branch", branch)?), BTreeMap::new(), Some(json!({
                "required_status_checks":required_status_checks,
                "enforce_admins":settings.enforce_admins,
                "required_pull_request_reviews":review,
                "restrictions":Value::Null,
                "required_conversation_resolution":settings.require_conversation_resolution,
                "allow_force_pushes":settings.allow_force_pushes,
                "allow_deletions":settings.allow_deletions,
            })), true, None)
        }
        BranchProtectionOperation::Delete { branch } => api(document, GitHubResource::BranchProtection, "delete", GitHubHttpMethod::Delete, &format!("branches/{}/protection", path_component("branch", branch)?), BTreeMap::new(), None, true, None),
        BranchProtectionOperation::ListRulesets { per_page } => api(document, GitHubResource::Repository, "list_rulesets", GitHubHttpMethod::Get, "rulesets", page_query(*per_page)?, None, true, None),
        BranchProtectionOperation::GetRuleset { ruleset_id } => numbered_get(document, GitHubResource::Repository, "get_ruleset", "rulesets", *ruleset_id),
        BranchProtectionOperation::CreateRuleset { ruleset } => api(document, GitHubResource::Repository, "create_ruleset", GitHubHttpMethod::Post, "rulesets", BTreeMap::new(), Some(ruleset_body(ruleset)?), true, None),
        BranchProtectionOperation::UpdateRuleset { ruleset_id, ruleset } => {
            require_number(*ruleset_id)?;
            api(document, GitHubResource::Repository, "update_ruleset", GitHubHttpMethod::Put, &format!("rulesets/{ruleset_id}"), BTreeMap::new(), Some(ruleset_body(ruleset)?), true, None)
        }
        BranchProtectionOperation::DeleteRuleset { ruleset_id } => {
            require_number(*ruleset_id)?;
            api(document, GitHubResource::Repository, "delete_ruleset", GitHubHttpMethod::Delete, &format!("rulesets/{ruleset_id}"), BTreeMap::new(), None, true, None)
        }
    }
}

fn ruleset_body(input: &RulesetInput) -> MedusaResult<Value> {
    require_text("ruleset name", &input.name, 1, 100)?;
    if !matches!(input.enforcement.as_str(), "disabled" | "active" | "evaluate") { return Err(invalid("ruleset enforcement is unsupported")); }
    if input.target != "branch" { return Err(invalid("only branch rulesets are supported")); }
    let rules = input.rules.iter().map(|rule| match rule {
        RulesetRule::RequiredSignatures => json!({"type":"required_signatures"}),
        RulesetRule::NonFastForward => json!({"type":"non_fast_forward"}),
        RulesetRule::Creation => json!({"type":"creation"}),
        RulesetRule::Update => json!({"type":"update"}),
        RulesetRule::Deletion => json!({"type":"deletion"}),
        RulesetRule::PullRequest { required_approving_review_count, require_code_owner_review, dismiss_stale_reviews_on_push, require_last_push_approval, required_review_thread_resolution } => json!({"type":"pull_request","parameters":{
            "required_approving_review_count":required_approving_review_count,
            "require_code_owner_review":require_code_owner_review,
            "dismiss_stale_reviews_on_push":dismiss_stale_reviews_on_push,
            "require_last_push_approval":require_last_push_approval,
            "required_review_thread_resolution":required_review_thread_resolution,
        }}),
        RulesetRule::RequiredStatusChecks { contexts, strict_required_status_checks_policy } => json!({"type":"required_status_checks","parameters":{
            "required_status_checks":contexts.iter().map(|context| json!({"context":context})).collect::<Vec<_>>(),
            "strict_required_status_checks_policy":strict_required_status_checks_policy,
        }}),
    }).collect::<Vec<_>>();
    Ok(json!({
        "name":input.name,
        "target":input.target,
        "enforcement":input.enforcement,
        "conditions":{"ref_name":{"include":input.include,"exclude":input.exclude}},
        "rules":rules,
    }))
}

fn prepare_projects(document: &GitHubOperationDocument, operation: &ProjectsOperation) -> MedusaResult<PreparedGitHubOperation> {
    match operation {
        ProjectsOperation::List { per_page } => api(document, GitHubResource::Projects, "list", GitHubHttpMethod::Get, "projects", page_query(*per_page)?, None, true, None),
        ProjectsOperation::Get { project_id } => numbered_get(document, GitHubResource::Projects, "get", "projects", *project_id),
        ProjectsOperation::Create { name, body } => api(document, GitHubResource::Projects, "create", GitHubHttpMethod::Post, "projects", BTreeMap::new(), Some(json!({"name":name,"body":body})), false, None),
        ProjectsOperation::Update { project_id, name, body, state } => {
            require_number(*project_id)?;
            api(document, GitHubResource::Projects, "update", GitHubHttpMethod::Patch, &format!("projects/{project_id}"), BTreeMap::new(), Some(object([
                optional("name", name.clone()), optional("body", body.clone()), optional("state", state.clone())
            ])), true, None)
        }
        ProjectsOperation::Delete { project_id } => {
            require_number(*project_id)?;
            api(document, GitHubResource::Projects, "delete", GitHubHttpMethod::Delete, &format!("projects/{project_id}"), BTreeMap::new(), None, true, None)
        }
    }
}

fn prepare_search(document: &GitHubOperationDocument, operation: &SearchOperation) -> MedusaResult<PreparedGitHubOperation> {
    let (endpoint, query_text, sort, order, per_page) = match operation {
        SearchOperation::Issues { query, sort, order, per_page } => ("issues", query, sort, order, *per_page),
        SearchOperation::Commits { query, sort, order, per_page } => ("commits", query, sort, order, *per_page),
        SearchOperation::Code { query, sort, order, per_page } => ("code", query, sort, order, *per_page),
    };
    validate_search_text(query_text)?;
    let mut query = page_query(per_page)?;
    query.insert("q".into(), format!("{} repo:{}", query_text.trim(), document.repository));
    insert_query(&mut query, "sort", sort.as_deref());
    insert_query(&mut query, "order", order.as_deref());
    api(document, GitHubResource::Search, &format!("search_{endpoint}"), GitHubHttpMethod::Get, endpoint, query, None, true, None)
}

fn api(
    document: &GitHubOperationDocument,
    resource: GitHubResource,
    action: &str,
    method: GitHubHttpMethod,
    endpoint: &str,
    query: BTreeMap<String, String>,
    body: Option<Value>,
    idempotent: bool,
    reconciliation: Option<GitHubReconciliation>,
) -> MedusaResult<PreparedGitHubOperation> {
    let backend = match document.backend {
        GitHubExecutionBackend::NativeCli => GitHubBackendKind::NativeCli,
        GitHubExecutionBackend::GhApi | GitHubExecutionBackend::DirectOauth => GitHubBackendKind::RestApi,
    };
    let request = GitHubOperationRequest {
        repository: document.repository.clone(),
        hostname: document.hostname.clone(),
        resource,
        action: action.into(),
        method,
        endpoint: endpoint.into(),
        query,
        body,
        backend,
        paginate: false,
        max_response_bytes: document.max_response_bytes,
    };
    request.validate()?;
    Ok(PreparedGitHubOperation {
        operation_kind: format!("{}.{}", resource_name(resource), action),
        risk: request.risk(),
        idempotent,
        reconciliation,
        execution: PreparedExecution::Api(request),
    })
}

fn numbered_get(document: &GitHubOperationDocument, resource: GitHubResource, action: &str, prefix: &str, number: u64) -> MedusaResult<PreparedGitHubOperation> {
    require_number(number)?;
    api(document, resource, action, GitHubHttpMethod::Get, &format!("{prefix}/{number}"), BTreeMap::new(), None, true, None)
}

fn action_post(document: &GitHubOperationDocument, action: &str, endpoint: &str, id: u64) -> MedusaResult<PreparedGitHubOperation> {
    require_number(id)?;
    api(document, GitHubResource::Actions, action, GitHubHttpMethod::Post, endpoint, BTreeMap::new(), None, true, None)
}

fn object<const N: usize>(pairs: [Option<(String, Value)>; N]) -> Value {
    Value::Object(pairs.into_iter().flatten().collect())
}

fn required<T: serde::Serialize>(key: &str, value: T) -> Option<(String, Value)> {
    Some((key.into(), serde_json::to_value(value).unwrap_or(Value::Null)))
}

fn optional<T: serde::Serialize>(key: &str, value: Option<T>) -> Option<(String, Value)> {
    value.map(|value| (key.into(), serde_json::to_value(value).unwrap_or(Value::Null)))
}

fn optional_nonempty<T: serde::Serialize>(key: &str, values: &[T]) -> Option<(String, Value)> {
    (!values.is_empty()).then(|| (key.into(), serde_json::to_value(values).unwrap_or(Value::Null)))
}

fn optional_nonempty_map<T: serde::Serialize>(key: &str, values: &BTreeMap<String, T>) -> Option<(String, Value)> {
    (!values.is_empty()).then(|| (key.into(), serde_json::to_value(values).unwrap_or(Value::Null)))
}

fn page_query(per_page: u16) -> MedusaResult<BTreeMap<String, String>> {
    if !(1..=100).contains(&per_page) { return Err(invalid("per_page must be between 1 and 100")); }
    Ok(BTreeMap::from([("per_page".into(), per_page.to_string())]))
}

fn insert_query(query: &mut BTreeMap<String, String>, key: &str, value: Option<&str>) {
    if let Some(value) = value { query.insert(key.into(), value.into()); }
}

fn repository_path(value: &str) -> MedusaResult<String> {
    if value.is_empty() || value.len() > 1024 || value.starts_with('/') || value.ends_with('/') || value.contains('\\') || value.split('/').any(|part| part.is_empty() || matches!(part, "." | "..")) || value.chars().any(char::is_control) {
        return Err(invalid("repository path must be relative and cannot traverse parents"));
    }
    Ok(value.into())
}

fn path_component(label: &str, value: &str) -> MedusaResult<String> { safe_component(label, value) }

fn safe_component(label: &str, value: &str) -> MedusaResult<String> {
    if value.is_empty() || value.len() > 255 || value.trim() != value || value.contains('/') || value.contains('\\') || matches!(value, "." | "..") || value.chars().any(char::is_control) {
        return Err(invalid(format!("{label} is not a safe GitHub path component")));
    }
    Ok(value.into())
}

fn git_reference(value: &str) -> MedusaResult<String> {
    if !value.starts_with("refs/") || value.len() > 512 || value.contains("..") || value.contains("@{") || value.ends_with('.') || value.ends_with('/') || value.contains('\\') || value.chars().any(char::is_control) {
        return Err(invalid("Git reference must be a valid refs/... name"));
    }
    Ok(value.into())
}

fn require_sha(value: &str) -> MedusaResult<()> {
    if !(7..=128).contains(&value.len()) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) { return Err(invalid("Git SHA is malformed")); }
    Ok(())
}

fn require_number(value: u64) -> MedusaResult<()> {
    if value == 0 { return Err(invalid("GitHub numeric identifiers must be greater than zero")); }
    Ok(())
}

fn require_text(label: &str, value: &str, minimum: usize, maximum: usize) -> MedusaResult<()> {
    if value.len() < minimum || value.len() > maximum || value.chars().any(|character| character == '\0') { return Err(invalid(format!("{label} length is invalid"))); }
    Ok(())
}

fn validate_string_list(label: &str, values: &[String], maximum_items: usize, maximum_length: usize) -> MedusaResult<()> {
    if values.len() > maximum_items { return Err(invalid(format!("too many {label} values"))); }
    for value in values { require_text(label, value, 1, maximum_length)?; }
    Ok(())
}

fn validate_reaction(content: &str) -> MedusaResult<()> {
    if !matches!(content, "+1" | "-1" | "laugh" | "confused" | "heart" | "hooray" | "rocket" | "eyes") { return Err(invalid("reaction content is unsupported")); }
    Ok(())
}

fn validate_search_text(value: &str) -> MedusaResult<()> {
    let lower = value.to_ascii_lowercase();
    if value.trim().is_empty() || value.len() > 1024 || lower.split_whitespace().any(|part| part.starts_with("repo:") || part.starts_with("org:") || part.starts_with("user:")) { return Err(invalid("search query cannot contain repository, organization, or user scopes")); }
    Ok(())
}

fn marker(digest: &str) -> String { format!("<!-- medusa-request:{digest} -->") }

fn append_marker(body: &str, marker: &str) -> String {
    if body.is_empty() { marker.into() } else { format!("{body}\n\n{marker}") }
}

fn resource_name(resource: GitHubResource) -> &'static str {
    match resource {
        GitHubResource::Repository => "repository",
        GitHubResource::Contents => "contents",
        GitHubResource::GitData => "git_data",
        GitHubResource::Issues => "issues",
        GitHubResource::PullRequests => "pull_requests",
        GitHubResource::Actions => "actions",
        GitHubResource::Releases => "releases",
        GitHubResource::Collaborators => "collaborators",
        GitHubResource::Environments => "environments",
        GitHubResource::Variables => "variables",
        GitHubResource::Secrets => "secrets",
        GitHubResource::Webhooks => "webhooks",
        GitHubResource::BranchProtection => "branch_protection",
        GitHubResource::Projects => "projects",
        GitHubResource::Search => "search",
    }
}

fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(ErrorCode::InvalidInput, ErrorCategory::Validation, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(operation: GitHubTypedOperation) -> GitHubOperationDocument {
        GitHubOperationDocument {
            schema_version: GITHUB_OPERATION_SCHEMA_VERSION,
            repository: "acme/project".into(),
            hostname: "github.com".into(),
            api_base_url: None,
            oauth_base_url: None,
            api_version: DEFAULT_GITHUB_API_VERSION.into(),
            backend: GitHubExecutionBackend::DirectOauth,
            idempotency_key: Some("test-key".into()),
            max_response_bytes: 1024,
            operation,
        }
    }

    #[test]
    fn issue_create_adds_reconciliation_marker() {
        let prepared = prepare(&document(GitHubTypedOperation::Issues(IssuesOperation::Create {
            title: "title".into(), body: "body".into(), labels: vec![], assignees: vec![], milestone: None
        }))).expect("prepare");
        assert!(matches!(prepared.reconciliation, Some(GitHubReconciliation::IssueMarker { .. })));
        let PreparedExecution::Api(request) = prepared.execution else { panic!("api") };
        assert!(request.body.expect("body").to_string().contains("medusa-request"));
    }

    #[test]
    fn search_scope_is_added_internally() {
        let prepared = prepare(&document(GitHubTypedOperation::Search(SearchOperation::Issues {
            query: "is:open bug".into(), sort: None, order: None, per_page: 30
        }))).expect("prepare");
        let PreparedExecution::Api(request) = prepared.execution else { panic!("api") };
        assert_eq!(request.query.get("q").expect("q"), "is:open bug repo:acme/project");
    }

    #[test]
    fn arbitrary_search_scope_is_rejected() {
        assert!(prepare(&document(GitHubTypedOperation::Search(SearchOperation::Code {
            query: "repo:other/project token".into(), sort: None, order: None, per_page: 30
        }))).is_err());
    }
}
