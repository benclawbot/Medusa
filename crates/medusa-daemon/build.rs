mod build_fixups;
mod build_native;
mod build_support;

fn main() {
    build_fixups::run();
    build_support::run();
    build_native::run();
}
