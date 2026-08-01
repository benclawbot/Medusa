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
    build_fixups::run();
    build_mini_app_client::run();
    build_security::run();
    build_support::run();
    build_native::run();
    build_voice_integration::run();
    build_mini_app_integration::run();
    build_webhook_integration::run();
    build_compile_fixups::run();
}
