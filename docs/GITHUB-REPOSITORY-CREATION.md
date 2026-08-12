# GitHub repository creation

Medusa can create and bootstrap a GitHub repository through the typed `medusa-github` service and the approval-gated `medusa-capabilities create-repository` entrypoint.

## Safety model

Repository creation is classified as `RepositoryMutation`. Capability discovery and read authorization run first, the complete request is validated and printed as a preview, and no external command runs until the caller supplies `--approve`.

Authentication is delegated to GitHub CLI. Medusa never accepts a token argument, does not read GitHub CLI credential files, and does not persist credentials in receipts or diagnostics.

## Creation modes

### Initialized remote

Use `--add-readme`, `--gitignore`, `--license`, or `--template` to let GitHub initialize the repository. Medusa then applies the requested issue/wiki settings and renames the initial default branch when necessary.

### Existing local project

Use `--source PATH`. Medusa verifies or initializes the Git repository, rejects an unrelated `origin`, optionally commits current content, renames the current branch, creates the remote, configures `origin`, and pushes with upstream tracking.

### New local project

Use `--source PATH --initialize-git --initial-commit-message MESSAGE`. The path is created when absent. The initial commit is made only after the full request passes validation and approval.

### Empty remote

When no initialization or source option is supplied, Medusa creates a temporary local repository with an empty initial commit, pushes the requested default branch, records the remote receipt, and removes the temporary directory.

## Idempotent recovery

Creation fails if the target already exists. `--reuse-existing` is an explicit retry mode for a previous partial creation. It can attach and push a corrected local project to the existing remote, or reconcile settings and the default branch without creating a second repository.

A failure after remote creation includes the repository URL and a bounded recovery instruction. Medusa never deletes the created remote automatically because that could remove externally visible work or race with another client.

## Structured receipt

The JSON result includes:

- canonical `owner/name`;
- web URL and HTTPS clone URL;
- visibility;
- actual default branch;
- whether the remote was created or reused;
- local source path when used;
- initial commit SHA when available;
- durable authorization events and the audit-ledger path.

## Examples

```bash
# Private initialized repository
medusa-capabilities create-repository \
  --owner acme \
  --name service \
  --visibility private \
  --add-readme \
  --gitignore Rust \
  --license mit \
  --disable-wiki \
  --approve

# Organization template, including all template branches
medusa-capabilities create-repository \
  --owner acme \
  --name service \
  --template acme/rust-service-template \
  --include-all-template-branches \
  --approve

# GitHub Enterprise and local bootstrap
medusa-capabilities create-repository \
  --hostname github.example.com \
  --owner platform \
  --name service \
  --source ./service \
  --initialize-git \
  --initial-commit-message "Initial commit" \
  --approve
```
