use std::{env, path::PathBuf, process::Command};

fn main() {
    let repository = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"))
        .join("../../..");
    println!("cargo:rerun-if-env-changed=MEDUSA_BUILD_COMMIT");
    println!("cargo:rerun-if-changed=../../../.git/HEAD");
    let revision = env::var("MEDUSA_BUILD_COMMIT").unwrap_or_else(|_| {
        Command::new("git")
            .arg("-C")
            .arg(&repository)
            .args(["rev-parse", "HEAD"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|output| output.trim().to_owned())
            .filter(|output| !output.is_empty())
            .unwrap_or_else(|| "unknown".to_owned())
    });
    let short_revision = revision.get(..12).unwrap_or(&revision);
    println!("cargo:rustc-env=MEDUSA_BUILD_COMMIT={revision}");
    println!("cargo:rustc-env=MEDUSA_BUILD_COMMIT_SHORT={short_revision}");
    tauri_build::build();
}
