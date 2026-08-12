use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::*;

use super::*;

impl<E: CommandExecutor> GitHubService<E> {
    /// Creates a GitHub repository and optionally initializes/pushes a local project.
    ///
    /// The caller is responsible for satisfying Medusa's explicit approval boundary before
    /// invoking this mutating operation. The method remains idempotent only when
    /// `reuse_existing` is explicitly selected.
    pub fn create_repository(
        &self,
        request: &RepositoryCreateRequest,
    ) -> MedusaResult<RepositoryCreationReceipt> {
        request.validate()?;
        let auth = self.executor.run(
            "gh",
            &strings(["auth", "status", "--hostname", &self.hostname]),
            None,
        )?;
        if !auth.success {
            return Err(policy_denied(format!(
                "GitHub authentication is required for {}",
                self.hostname
            )));
        }

        let full_name = request.full_name()?;
        let qualified = self.qualified_repository(&full_name);
        if let Some(mut existing) = self.inspect_repository(&qualified, false)? {
            if !request.reuse_existing {
                return Err(invalid_input(format!(
                    "GitHub repository {full_name} already exists; select reuse_existing for an idempotent retry"
                )));
            }
            if let Some(bootstrap) = &request.bootstrap {
                let prepared = self.prepare_local_repository(request, bootstrap, false)?;
                let push_result = self.attach_and_push_existing(&full_name, request, &prepared);
                if let Err(error) = push_result {
                    return Err(partial_failure(&existing.web_url, error));
                }
                existing.local_path = Some(prepared.path.clone());
                existing.initial_commit = self.current_commit(&prepared.path).ok();
                if prepared.push {
                    existing.default_branch = request.default_branch.trim().to_owned();
                }
            } else {
                self.rename_default_branch_if_needed(
                    &full_name,
                    request,
                    &existing.default_branch,
                )?;
                existing = self
                    .inspect_repository(&qualified, false)?
                    .ok_or_else(|| internal_error("reused repository could not be inspected"))?;
            }
            existing.created = false;
            return Ok(existing);
        }

        if let Some(bootstrap) = &request.bootstrap {
            let prepared = self.prepare_local_repository(request, bootstrap, false)?;
            self.create_from_local(&qualified, request, &prepared)?;
            let recovery_url = self.web_url_for(&full_name);
            return (|| {
                let mut receipt = self
                    .inspect_repository(&qualified, true)?
                    .ok_or_else(|| internal_error("created repository could not be inspected"))?;
                receipt.local_path = Some(prepared.path.clone());
                receipt.initial_commit = self.current_commit(&prepared.path).ok();
                if prepared.push {
                    receipt.default_branch = request.default_branch.trim().to_owned();
                }
                Ok(receipt)
            })()
            .map_err(|error| partial_failure(&recovery_url, error));
        }

        if request.template_repository.is_some()
            || request.add_readme
            || request.gitignore_template.is_some()
            || request.license_template.is_some()
        {
            self.create_remote_initialized(&qualified, request)?;
            let recovery_url = self.web_url_for(&full_name);
            return (|| {
                let mut receipt = self
                    .inspect_repository(&qualified, true)?
                    .ok_or_else(|| internal_error("created repository could not be inspected"))?;
                self.rename_default_branch_if_needed(&full_name, request, &receipt.default_branch)?;
                receipt = self
                    .inspect_repository(&qualified, true)?
                    .ok_or_else(|| internal_error("created repository could not be inspected"))?;
                Ok(receipt)
            })()
            .map_err(|error| partial_failure(&recovery_url, error));
        }

        let temporary = TemporaryRepository::new()?;
        let bootstrap = RepositoryBootstrap {
            path: temporary.path.clone(),
            initialize_git: true,
            initial_commit_message: Some("Initial commit".to_owned()),
            push: true,
        };
        let prepared = self.prepare_local_repository(request, &bootstrap, true)?;
        self.create_from_local(&qualified, request, &prepared)?;
        let recovery_url = self.web_url_for(&full_name);
        (|| {
            self.inspect_repository(&qualified, true)?
                .ok_or_else(|| internal_error("created repository could not be inspected"))
        })()
        .map_err(|error| partial_failure(&recovery_url, error))
    }

    fn create_remote_initialized(
        &self,
        qualified: &str,
        request: &RepositoryCreateRequest,
    ) -> MedusaResult<String> {
        let mut args = strings([
            "repo",
            "create",
            qualified,
            request.visibility.create_flag(),
        ]);
        self.append_repository_options(&mut args, request);
        self.run("gh", args, None)
    }

    fn create_from_local(
        &self,
        qualified: &str,
        request: &RepositoryCreateRequest,
        prepared: &PreparedRepository,
    ) -> MedusaResult<String> {
        let mut args = strings([
            "repo",
            "create",
            qualified,
            request.visibility.create_flag(),
        ]);
        self.append_repository_options(&mut args, request);
        let output = self.run("gh", args, None)?;
        let full_name = request.full_name()?;
        let recovery_url = self.web_url_for(&full_name);
        self.attach_and_push_existing(&full_name, request, prepared)
            .map_err(|error| partial_failure(&recovery_url, error))?;
        Ok(output)
    }

    fn append_repository_options(&self, args: &mut Vec<String>, request: &RepositoryCreateRequest) {
        if let Some(description) = request.description.as_deref() {
            args.extend(strings(["--description", description.trim()]));
        }
        if let Some(homepage) = request.homepage.as_deref() {
            args.extend(strings(["--homepage", homepage.trim()]));
        }
        if request.add_readme {
            args.push("--add-readme".to_owned());
        }
        if let Some(template) = request.gitignore_template.as_deref() {
            args.extend(strings(["--gitignore", template.trim()]));
        }
        if let Some(license) = request.license_template.as_deref() {
            args.extend(strings(["--license", license.trim()]));
        }
        if let Some(template) = request.template_repository.as_deref() {
            args.extend(strings(["--template", template.trim()]));
        }
        if request.include_all_template_branches {
            args.push("--include-all-branches".to_owned());
        }
        if !request.issues_enabled {
            args.push("--disable-issues".to_owned());
        }
        if !request.wiki_enabled {
            args.push("--disable-wiki".to_owned());
        }
    }

    fn prepare_local_repository(
        &self,
        request: &RepositoryCreateRequest,
        bootstrap: &RepositoryBootstrap,
        use_medusa_identity: bool,
    ) -> MedusaResult<PreparedRepository> {
        let path = bootstrap.path.clone();
        if path.exists() && !path.is_dir() {
            return Err(invalid_input(
                "bootstrap path exists and is not a directory",
            ));
        }
        if !path.exists() {
            if !bootstrap.initialize_git {
                return Err(invalid_input(
                    "bootstrap path does not exist and initialize_git is false",
                ));
            }
            fs::create_dir_all(&path).map_err(environment_error)?;
        }

        let repository_probe = self.executor.run(
            "git",
            &strings(["rev-parse", "--is-inside-work-tree"]),
            Some(&path),
        )?;
        if !repository_probe.success {
            if !bootstrap.initialize_git {
                return Err(invalid_input(
                    "bootstrap path is not a Git repository and initialize_git is false",
                ));
            }
            self.run("git", strings(["init"]), Some(&path))?;
        }

        let requested_root = fs::canonicalize(&path).map_err(environment_error)?;
        let discovered_root = self.run(
            "git",
            strings(["rev-parse", "--show-toplevel"]),
            Some(&path),
        )?;
        let discovered_root =
            fs::canonicalize(discovered_root.trim()).map_err(environment_error)?;
        if requested_root != discovered_root {
            return Err(policy_denied(format!(
                "bootstrap path {} is nested inside Git worktree {}; select the exact worktree root",
                requested_root.display(),
                discovered_root.display()
            )));
        }

        self.preflight_remote(
            &path,
            &format!("{}/{}", request.owner.trim(), request.name.trim()),
        )?;

        let head = self.executor.run(
            "git",
            &strings(["rev-parse", "--verify", "HEAD"]),
            Some(&path),
        )?;
        if !head.success {
            let symbolic_ref = format!("refs/heads/{}", request.default_branch.trim());
            self.run(
                "git",
                strings(["symbolic-ref", "HEAD", &symbolic_ref]),
                Some(&path),
            )?;
            let message = bootstrap.initial_commit_message.as_deref().ok_or_else(|| {
                invalid_input("a repository without commits requires initial_commit_message")
            })?;
            self.run("git", strings(["add", "--all"]), Some(&path))?;
            let commit_arguments = if use_medusa_identity {
                strings([
                    "-c",
                    "user.name=Medusa",
                    "-c",
                    "user.email=medusa@localhost",
                    "commit",
                    "--allow-empty",
                    "-m",
                    message.trim(),
                ])
            } else {
                strings(["commit", "--allow-empty", "-m", message.trim()])
            };
            self.run("git", commit_arguments, Some(&path))?;
        } else {
            self.run(
                "git",
                strings(["branch", "-M", request.default_branch.trim()]),
                Some(&path),
            )?;
            if let Some(message) = bootstrap.initial_commit_message.as_deref() {
                self.run("git", strings(["add", "--all"]), Some(&path))?;
                let staged = self.executor.run(
                    "git",
                    &strings(["diff", "--cached", "--quiet"]),
                    Some(&path),
                )?;
                if !staged.success {
                    self.run(
                        "git",
                        strings(["commit", "-m", message.trim()]),
                        Some(&path),
                    )?;
                }
            }
        }

        Ok(PreparedRepository {
            path,
            push: bootstrap.push,
        })
    }

    fn preflight_remote(&self, path: &Path, full_name: &str) -> MedusaResult<()> {
        let output =
            self.executor
                .run("git", &strings(["remote", "get-url", "origin"]), Some(path))?;
        if !output.success {
            return Ok(());
        }
        let expected = self.accepted_remote_urls(full_name);
        if expected
            .iter()
            .any(|candidate| candidate == output.stdout.trim())
        {
            Ok(())
        } else {
            Err(policy_denied(format!(
                "bootstrap repository already has an unrelated origin remote: {}",
                output.stdout.trim()
            )))
        }
    }

    fn attach_and_push_existing(
        &self,
        full_name: &str,
        request: &RepositoryCreateRequest,
        prepared: &PreparedRepository,
    ) -> MedusaResult<()> {
        let remote = self.clone_url_for(full_name);
        let current = self.executor.run(
            "git",
            &strings(["remote", "get-url", "origin"]),
            Some(&prepared.path),
        )?;
        if !current.success {
            self.run(
                "git",
                strings(["remote", "add", "origin", &remote]),
                Some(&prepared.path),
            )?;
        }
        if prepared.push {
            self.run(
                "git",
                strings(["push", "-u", "origin", request.default_branch.trim()]),
                Some(&prepared.path),
            )?;
        }
        self.edit_repository_settings(full_name, request, prepared.push)
    }

    fn edit_repository_settings(
        &self,
        full_name: &str,
        request: &RepositoryCreateRequest,
        branch_exists: bool,
    ) -> MedusaResult<()> {
        let qualified = self.qualified_repository(full_name);
        let issues = format!("--enable-issues={}", request.issues_enabled);
        let wiki = format!("--enable-wiki={}", request.wiki_enabled);
        let mut args = strings(["repo", "edit", &qualified, &issues, &wiki]);
        if let Some(description) = request.description.as_deref() {
            args.extend(strings(["--description", description.trim()]));
        }
        if let Some(homepage) = request.homepage.as_deref() {
            args.extend(strings(["--homepage", homepage.trim()]));
        }
        if branch_exists {
            args.extend(strings(["--default-branch", request.default_branch.trim()]));
        }
        self.run("gh", args, None)?;
        Ok(())
    }

    fn rename_default_branch_if_needed(
        &self,
        full_name: &str,
        request: &RepositoryCreateRequest,
        current: &str,
    ) -> MedusaResult<()> {
        let desired = request.default_branch.trim();
        if current.is_empty() || current == desired {
            return self.edit_repository_settings(full_name, request, !current.is_empty());
        }
        let endpoint = format!(
            "repos/{full_name}/branches/{}/rename",
            percent_encode_path_segment(current)
        );
        let new_name = format!("new_name={desired}");
        self.run(
            "gh",
            strings([
                "api",
                "--hostname",
                &self.hostname,
                "-X",
                "POST",
                &endpoint,
                "-f",
                &new_name,
            ]),
            None,
        )?;
        self.edit_repository_settings(full_name, request, true)
    }

    fn inspect_repository(
        &self,
        qualified: &str,
        created: bool,
    ) -> MedusaResult<Option<RepositoryCreationReceipt>> {
        let output = self.executor.run(
            "gh",
            &strings([
                "repo",
                "view",
                qualified,
                "--json",
                "nameWithOwner,url,visibility,defaultBranchRef",
                "--template",
                "{{.nameWithOwner}}\t{{.url}}\t{{.visibility}}\t{{if .defaultBranchRef}}{{.defaultBranchRef.name}}{{end}}",
            ]),
            None,
        )?;
        if !output.success {
            if repository_missing(&output.stderr) {
                return Ok(None);
            }
            return Err(execution_error(
                "gh repo view",
                sanitize_external_error(&output.stderr),
            ));
        }
        let mut fields = output.stdout.splitn(4, '\t');
        let repository = fields.next().unwrap_or_default().trim().to_owned();
        let web_url = fields.next().unwrap_or_default().trim().to_owned();
        let visibility = parse_visibility(fields.next().unwrap_or_default())?;
        let default_branch = fields.next().unwrap_or_default().trim().to_owned();
        if repository.is_empty() || web_url.is_empty() {
            return Err(internal_error(
                "GitHub repository inspection returned incomplete data",
            ));
        }
        Ok(Some(RepositoryCreationReceipt {
            clone_url: self.clone_url_for(&repository),
            repository,
            web_url,
            visibility,
            default_branch,
            created,
            local_path: None,
            initial_commit: None,
        }))
    }

    fn current_commit(&self, path: &Path) -> MedusaResult<String> {
        self.run("git", strings(["rev-parse", "HEAD"]), Some(path))
    }

    fn accepted_remote_urls(&self, full_name: &str) -> Vec<String> {
        vec![
            self.clone_url_for(full_name),
            format!("git@{}:{full_name}.git", self.hostname),
            format!("ssh://git@{}/{full_name}.git", self.hostname),
        ]
    }

    fn qualified_repository(&self, full_name: &str) -> String {
        if self.hostname == "github.com" {
            full_name.to_owned()
        } else {
            format!("{}/{full_name}", self.hostname)
        }
    }

    fn web_url_for(&self, full_name: &str) -> String {
        format!("https://{}/{full_name}", self.hostname)
    }

    pub(crate) fn clone_url_for(&self, full_name: &str) -> String {
        format!("https://{}/{full_name}.git", self.hostname)
    }
}

#[derive(Debug)]
struct PreparedRepository {
    path: PathBuf,
    push: bool,
}

#[derive(Debug)]
struct TemporaryRepository {
    path: PathBuf,
}

impl TemporaryRepository {
    fn new() -> MedusaResult<Self> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| internal_error(error.to_string()))?
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "medusa-github-create-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).map_err(environment_error)?;
        Ok(Self { path })
    }
}

impl Drop for TemporaryRepository {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
