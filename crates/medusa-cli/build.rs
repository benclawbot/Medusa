use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

fn git_output(repository: &Path, arguments: &[&str]) -> Option<String> {
    Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|output| output.trim().to_owned())
        .filter(|output| !output.is_empty())
}

fn main() {
    let repository =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory")).join("../..");
    println!("cargo:rerun-if-env-changed=MEDUSA_BUILD_COMMIT");
    if let Some(git_dir) = git_output(&repository, &["rev-parse", "--absolute-git-dir"]) {
        let git_dir = PathBuf::from(git_dir);
        println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
        println!(
            "cargo:rerun-if-changed={}",
            git_dir.join("packed-refs").display()
        );
        if let Some(reference) = git_output(&repository, &["symbolic-ref", "-q", "HEAD"]) {
            println!(
                "cargo:rerun-if-changed={}",
                git_dir.join(reference).display()
            );
        }
    }
    let revision = env::var("MEDUSA_BUILD_COMMIT")
        .ok()
        .and_then(|value| valid_revision(&value))
        .or_else(|| git_output(&repository, &["rev-parse", "HEAD"]))
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=MEDUSA_BUILD_COMMIT={revision}");
    let short_revision = revision.chars().take(12).collect::<String>();
    println!("cargo:rustc-env=MEDUSA_BUILD_COMMIT_SHORT={short_revision}");
}

fn valid_revision(value: &str) -> Option<String> {
    let revision = value.trim();
    (!revision.is_empty() && !revision.chars().any(char::is_control)).then(|| revision.to_owned())
}

#[cfg(test)]
mod tests {
    use super::valid_revision;

    #[test]
    fn revision_metadata_is_single_line_and_unicode_safe() {
        assert_eq!(valid_revision("  abc123  "), Some("abc123".to_owned()));
        assert_eq!(valid_revision("abc\ndef"), None);
        assert_eq!(valid_revision("éééééééé"), Some("éééééééé".to_owned()));
    }

}
