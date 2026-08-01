mod build_fixups;
#[rustfmt::skip]
mod build_native;
mod build_support;
#[rustfmt::skip]
mod build_voice_integration;

fn main() {
    build_fixups::run();
    build_support::run();
    build_native::run();
    build_voice_integration::run();
}
