use std::{env, process::Command};

fn main() {
    println!("cargo:rerun-if-env-changed=MEDUSA_BUILD_COMMIT");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    let revision = env::var("MEDUSA_BUILD_COMMIT").unwrap_or_else(|_| {
        Command::new("git")
            .args(["-C", "../..", "rev-parse", "HEAD"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|output| output.trim().to_owned())
            .filter(|output| !output.is_empty())
            .unwrap_or_else(|| "unknown".to_owned())
    });
    println!("cargo:rustc-env=MEDUSA_BUILD_COMMIT={revision}");
}
