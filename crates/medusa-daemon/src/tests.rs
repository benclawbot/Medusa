// Compatibility shim for `server_base.rs`, which is included from `server.rs`.
// Keep the original server test suite in its canonical location.
include!("server/tests.rs");
