use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::*;

/// Visibility accepted when creating a repository.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryVisibility {
    Public,
    Private,
    Internal,
}

impl RepositoryVisibility {
    fn create_flag(self) -> &'static str {
        match self {
            Self::Public => "--public",
            Self::Private => "--private",
            Self::Internal => "--internal",
        }
    }
}

/// Optional local project bootstrap performed as part of repository creation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RepositoryBootstrap {
    pub path: PathBuf,
    pub initialize_git: bool,
    pub initial_commit_message: Option<String>,
    pub push: bool,
}

/// Complete, reviewable request to create a new GitHub repository.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RepositoryCreateRequest {
    pub owner: String,
    pub name: String,
    pub visibility: RepositoryVisibility,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub default_branch: String,
    pub add_readme: bool,
    pub gitignore_template: Option<String>,
    pub license_template: Option<String>,
    pub issues_enabled: bool,
    pub wiki_enabled: bool,
    pub template_repository: Option<String>,
    pub include_all_template_branches: bool,
    pub reuse_existing: bool,
    pub bootstrap: Option<RepositoryBootstrap>,
}

impl RepositoryCreateRequest {
    /// Returns the canonical `owner/name` identity after validation.
    pub fn full_name(&self) -> MedusaResult<String> {
        self.validate()?;
        Ok(format!("{}/{}", self.owner.trim(), self.name.trim()))
    }

    /// Rejects malformed, ambiguous, or mutually incompatible creation requests.
    pub fn validate(&self) -> MedusaResult<()> {
        validate_owner(self.owner.trim())?;
        validate_repository_name(self.name.trim())?;
        validate_branch(self.default_branch.trim())?;
        validate_optional_text("description", self.description.as_deref(), 350)?;
        validate_optional_url(self.homepage.as_deref())?;
        validate_optional_template("gitignore template", self.gitignore_template.as_deref())?;
        validate_optional_template("license template", self.license_template.as_deref())?;
        if let Some(template) = self.template_repository.as_deref() {
            validate_repository_identity(template)?;
            if self.add_readme
                || self.gitignore_template.is_some()
                || self.license_template.is_some()
                || self.bootstrap.is_some()
            {
                return Err(invalid_input(
                    "template_repository cannot be combined with README, license, gitignore, or local bootstrap options",
                ));
            }
        } else if self.include_all_template_branches {
            return Err(invalid_input(
                "include_all_template_branches requires template_repository",
            ));
        }
        if let Some(bootstrap) = &self.bootstrap {
            if self.add_readme
                || self.gitignore_template.is_some()
                || self.license_template.is_some()
            {
                return Err(invalid_input(
                    "local bootstrap cannot be combined with remote README, license, or gitignore initialization",
                ));
            }
            if bootstrap.path.as_os_str().is_empty() {
                return Err(invalid_input("bootstrap path is required"));
            }
            validate_optional_text(
                "initial commit message",
                bootstrap.initial_commit_message.as_deref(),
                512,
            )?;
        }
        Ok(())
    }
}

/// Structured evidence returned after repository creation or idempotent reuse.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RepositoryCreationReceipt {
    pub repository: String,
    pub web_url: String,
    pub clone_url: String,
    pub visibility: RepositoryVisibility,
    pub default_branch: String,
    pub created: bool,
    pub local_path: Option<PathBuf>,
    pub initial_commit: Option<String>,
}

mod operations;
mod service;
mod validation;

pub use operations::*;
pub(crate) use validation::*;

#[cfg(test)]
mod tests;
