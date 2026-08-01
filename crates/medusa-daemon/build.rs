use std::{fs, path::PathBuf};

mod build_fixups;
#[rustfmt::skip]
mod build_compile_fixups;
#[rustfmt::skip]
mod build_mini_app_client;
#[rustfmt::skip]
mod build_mini_app_integration;
#[rustfmt::skip]
mod build_native;
#[rustfmt::skip]
mod build_security;
mod build_support;
#[rustfmt::skip]
mod build_voice_integration;
#[rustfmt::skip]
mod build_webhook_integration;

fn main() {
    let root = std::env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| fail("CARGO_MANIFEST_DIR is unavailable"));
    let marker = root.join(".issue-568-materialized");
    if marker.is_file() {
        return;
    }

    build_fixups::run();
    build_mini_app_client::run();
    build_security::run();
    build_support::run();
    build_native::run();
    build_voice_integration::run();
    build_mini_app_integration::run();
    build_webhook_integration::run();
    build_compile_fixups::run();

    if let Err(error) = fs::write(&marker, b"materialized\n") {
        fail(&format!(
            "cannot write one-time materialization marker {}: {error}",
            marker.display()
        ));
    }
}

fn fail(message: &str) -> ! {
    eprintln!("cargo:warning={message}");
    std::process::exit(1)
}
