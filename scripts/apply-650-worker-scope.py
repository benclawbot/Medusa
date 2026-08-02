from pathlib import Path
import re

manifest = Path('crates/medusa-workers/Cargo.toml')
text = manifest.read_text()
needle = '[dependencies]\nmedusa-core.workspace = true\n'
if needle not in text:
    raise SystemExit('worker manifest dependency anchor missing')
text = text.replace(needle, '[dependencies]\nmedusa-core.workspace = true\nmedusa-evidence.workspace = true\n', 1)
manifest.write_text(text)

path = Path('crates/medusa-workers/src/lib.rs')
text = path.read_text()
text = text.replace(
    'use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};\n',
    'use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};\nuse medusa_evidence::{ChangeKind, ChangedComponent, normalize_components};\n',
    1,
)
text = text.replace(
    '    pub integrated_head: String,\n    pub changed_paths: Vec<String>,\n}',
    '    pub integrated_head: String,\n    pub changed_paths: Vec<String>,\n    pub changed_components: Vec<ChangedComponent>,\n}',
    1,
)
old_changed = '''    /// Returns every tracked or untracked path changed relative to the worker base commit.
    pub fn changed_paths_since(
        &self,
        worker: &Worker,
        base_commit: &str,
    ) -> MedusaResult<Vec<String>> {
        if base_commit.trim().is_empty() {
            return Err(invalid("worker base commit cannot be empty"));
        }
        let mut paths = git_nul_paths(
            &worker.worktree,
            &[
                "diff",
                "--name-only",
                "--diff-filter=ACDMRTUXB",
                "-z",
                base_commit,
                "--",
            ],
        )?;
        paths.extend(git_nul_paths(
            &worker.worktree,
            &["ls-files", "--others", "--exclude-standard", "-z"],
        )?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
'''
new_changed = '''    /// Returns exact tracked and untracked components changed relative to the worker base commit.
    pub fn changed_components_since(
        &self,
        worker: &Worker,
        base_commit: &str,
    ) -> MedusaResult<Vec<ChangedComponent>> {
        if base_commit.trim().is_empty() {
            return Err(invalid("worker base commit cannot be empty"));
        }
        let mut components = git_changed_components(
            &worker.worktree,
            &[
                "diff",
                "--name-status",
                "-M",
                "-C",
                "-z",
                base_commit,
                "--",
            ],
        )?;
        for path in git_nul_paths(
            &worker.worktree,
            &["ls-files", "--others", "--exclude-standard", "-z"],
        )? {
            components.push(
                ChangedComponent::new(ChangeKind::Added, path)
                    .map_err(|error| invalid(error.to_string()))?,
            );
        }
        normalize_components(&worker.worktree, &components)
            .map_err(|error| invalid(error.to_string()))
    }

    /// Compatibility projection of exact changed-component scope.
    pub fn changed_paths_since(
        &self,
        worker: &Worker,
        base_commit: &str,
    ) -> MedusaResult<Vec<String>> {
        self.changed_components_since(worker, base_commit)
            .map(|components| changed_component_paths(&components))
    }
'''
if old_changed not in text:
    raise SystemExit('changed_paths_since block missing')
text = text.replace(old_changed, new_changed, 1)

text = text.replace(
    '            let paths = changed_paths_for_commit(&self.repo, commit)?;\n            if paths.is_empty() {',
    '            let components = changed_components_for_commit(&self.repo, commit)?;\n            let paths = changed_component_paths(&components);\n            if paths.is_empty() {',
    1,
)
text = text.replace(
    '            prepared.push((worker, commit.to_owned(), paths));',
    '            prepared.push((worker, commit.to_owned(), paths, components));',
    1,
)
text = text.replace(
    '        for (worker, commit, changed_paths) in prepared {',
    '        for (worker, commit, changed_paths, changed_components) in prepared {',
    1,
)
text = text.replace(
    '                integrated_head: self.repository_head()?,\n                changed_paths,\n            });',
    '                integrated_head: self.repository_head()?,\n                changed_paths,\n                changed_components,\n            });',
    1,
)
text = text.replace(
    '''    /// Returns the exact changed paths encoded by a prepared commit.
    pub fn commit_changed_paths(&self, commit: &str) -> MedusaResult<Vec<String>> {
        if commit.trim().is_empty() {
            return Err(invalid("worker commit cannot be empty"));
        }
        changed_paths_for_commit(&self.repo, commit)
    }
''',
    '''    /// Returns the exact changed components encoded by a prepared commit.
    pub fn commit_changed_components(&self, commit: &str) -> MedusaResult<Vec<ChangedComponent>> {
        if commit.trim().is_empty() {
            return Err(invalid("worker commit cannot be empty"));
        }
        changed_components_for_commit(&self.repo, commit)
    }

    /// Compatibility projection of exact prepared-commit scope.
    pub fn commit_changed_paths(&self, commit: &str) -> MedusaResult<Vec<String>> {
        self.commit_changed_components(commit)
            .map(|components| changed_component_paths(&components))
    }
''',
    1,
)
text = text.replace(
    '                integrated_head: self.repository_head()?,\n                changed_paths: self.commit_changed_paths(authorized_commit)?,\n            });',
    '                integrated_head: self.repository_head()?,\n                changed_paths: self.commit_changed_paths(authorized_commit)?,\n                changed_components: self.commit_changed_components(authorized_commit)?,\n            });',
    1,
)
old_helper = '''fn changed_paths_for_commit(repo: &Path, commit: &str) -> MedusaResult<Vec<String>> {
    let mut paths = git_nul_paths(
        repo,
        &[
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "--diff-filter=ACDMRTUXB",
            "-r",
            "-z",
            commit,
        ],
    )?;
    paths.sort();
    paths.dedup();
    Ok(paths)
}
'''
new_helper = '''fn changed_components_for_commit(
    repo: &Path,
    commit: &str,
) -> MedusaResult<Vec<ChangedComponent>> {
    let components = git_changed_components(
        repo,
        &[
            "diff-tree",
            "--root",
            "--no-commit-id",
            "--name-status",
            "-M",
            "-C",
            "-r",
            "-z",
            commit,
        ],
    )?;
    normalize_components(repo, &components).map_err(|error| invalid(error.to_string()))
}

fn changed_component_paths(components: &[ChangedComponent]) -> Vec<String> {
    let mut paths = components
        .iter()
        .flat_map(ChangedComponent::all_paths)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}

fn git_changed_components(repo: &Path, args: &[&str]) -> MedusaResult<Vec<ChangedComponent>> {
    let output = Command::new("git").args(args).current_dir(repo).output()?;
    if !output.status.success() {
        return output_result(&format!("git {}", args.join(" ")), output).map(|_| Vec::new());
    }
    let fields = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .map(|field| {
            String::from_utf8(field.to_vec()).map_err(|error| {
                invalid(format!("Git returned non-UTF-8 change metadata: {error}"))
            })
        })
        .collect::<MedusaResult<Vec<_>>>()?;
    let mut components = Vec::new();
    let mut index = 0;
    while index < fields.len() {
        let status = fields[index].as_str();
        index += 1;
        let code = status.chars().next().unwrap_or('X');
        let component = match code {
            'R' | 'C' => {
                let previous = fields.get(index).ok_or_else(|| invalid("Git rename source missing"))?;
                let path = fields
                    .get(index + 1)
                    .ok_or_else(|| invalid("Git rename target missing"))?;
                index += 2;
                if code == 'R' {
                    ChangedComponent::renamed(previous.clone(), path.clone())
                } else {
                    let mut component = ChangedComponent::new(ChangeKind::Copied, path.clone())?;
                    component.previous_path = Some(previous.clone());
                    Ok(component)
                }
            }
            _ => {
                let path = fields.get(index).ok_or_else(|| invalid("Git change path missing"))?;
                index += 1;
                ChangedComponent::new(
                    match code {
                        'A' => ChangeKind::Added,
                        'M' => ChangeKind::Modified,
                        'D' => ChangeKind::Deleted,
                        'T' => ChangeKind::TypeChanged,
                        'U' => ChangeKind::Unmerged,
                        _ => ChangeKind::Unknown,
                    },
                    path.clone(),
                )
            }
        }
        .map_err(|error| invalid(error.to_string()))?;
        components.push(component);
    }
    Ok(components)
}
'''
if old_helper not in text:
    raise SystemExit('changed_paths_for_commit helper missing')
text = text.replace(old_helper, new_helper, 1)

test_anchor = '    #[test]\n    fn parallel_feature_fixture_merges_and_verifies() {'
test = '''    #[test]
    fn exact_scope_preserves_rename_delete_generated_and_owner() {
        let (_directory, repo, worktrees) = repository();
        fs::create_dir_all(repo.join("apps/web/src")).expect("source directory");
        fs::write(repo.join("apps/web/package.json"), "{}\n").expect("package");
        fs::write(repo.join("apps/web/src/old.tsx"), "old\n").expect("old source");
        fs::write(repo.join("apps/web/src/delete.css"), "delete\n").expect("deleted source");
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-m", "fixture"]);
        let base = git_stdout(&repo, &["rev-parse", "HEAD"]).expect("base");
        let manager = WorkerManager::new(&repo, &worktrees).expect("manager");
        let worker = manager.create_worker("scope").expect("worker");
        git(&worker.worktree, &["mv", "apps/web/src/old.tsx", "apps/web/src/new.tsx"]);
        fs::remove_file(worker.worktree.join("apps/web/src/delete.css")).expect("delete");
        fs::create_dir_all(worker.worktree.join("apps/web/generated")).expect("generated");
        fs::write(worker.worktree.join("apps/web/generated/schema.json"), "{}\n")
            .expect("generated artifact");
        let components = manager
            .changed_components_since(&worker, &base)
            .expect("components");
        assert!(components.iter().any(|component| {
            component.kind == ChangeKind::Renamed
                && component.previous_path.as_deref() == Some("apps/web/src/old.tsx")
                && component.path == "apps/web/src/new.tsx"
        }));
        assert!(components.iter().any(|component| component.kind == ChangeKind::Deleted));
        assert!(components.iter().any(|component| {
            component.generated && component.package_owner.as_deref() == Some("apps/web")
        }));
        manager.cleanup(&[worker]).expect("cleanup");
    }

'''
if test_anchor not in text:
    raise SystemExit('worker test anchor missing')
text = text.replace(test_anchor, test + test_anchor, 1)
path.write_text(text)
