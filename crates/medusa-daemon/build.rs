mod build_fixups;
#[rustfmt::skip]
mod build_mini_app_integration;
#[rustfmt::skip]
mod build_native;
mod build_support;
#[rustfmt::skip]
mod build_voice_integration;
#[rustfmt::skip]
mod build_webhook_integration;

fn main() {
    build_fixups::run();
    build_support::run();
    build_native::run();
    build_voice_integration::run();
    build_mini_app_integration::run();
    build_webhook_integration::run();
}
