from pathlib import Path

path = Path("crates/medusa-agent/src/session.rs")
source = path.read_text()
old_doc = "/// Creates the on-disk Medusa layout and repository map."
new_doc = "/// Creates the on-disk Medusa layout and runtime-owned repository map."
old_path = '    let map = repo.join("REPOSITORY_MAP.md");'
new_path = '    let map = repo.join(".medusa/REPOSITORY_MAP.md");'
assert source.count(old_doc) == 1
assert source.count(old_path) == 1
source = source.replace(old_doc, new_doc).replace(old_path, new_path)
assert "bootstrap_keeps_generated_repository_map_in_runtime_state" not in source
source = source.rstrip() + '''

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_keeps_generated_repository_map_in_runtime_state() {
        let directory = tempfile::tempdir().expect("tempdir");
        let repo = directory.path();

        bootstrap(repo).expect("bootstrap");
        assert!(repo.join(".medusa/REPOSITORY_MAP.md").is_file());
        assert!(!repo.join("REPOSITORY_MAP.md").exists());

        fs::write(repo.join("REPOSITORY_MAP.md"), "# User-owned map\\n").expect("user map");
        bootstrap(repo).expect("second bootstrap");
        assert_eq!(
            fs::read_to_string(repo.join("REPOSITORY_MAP.md")).expect("read user map"),
            "# User-owned map\\n"
        );
    }
}
'''
path.write_text(source)
