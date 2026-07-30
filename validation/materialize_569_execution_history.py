from pathlib import Path

lib_path = Path("crates/medusa-runtime/src/lib.rs")
lib = lib_path.read_text()
if "pub mod execution_history;" not in lib:
    old = "mod error;\nmod learning_retrieval;\n"
    new = "mod error;\npub mod execution_history;\nmod learning_retrieval;\n"
    if lib.count(old) != 1:
        raise SystemExit("runtime module insertion target changed")
    lib = lib.replace(old, new, 1)
if "RuntimeExecutionHealth" not in lib:
    old = "pub use error::RuntimeError;\n"
    new = "pub use error::RuntimeError;\npub use execution_history::{\n    RuntimeContinuityHealth, RuntimeExecutionHealth, RuntimeHistoricalState,\n};\n"
    if lib.count(old) != 1:
        raise SystemExit("runtime history export target changed")
    lib = lib.replace(old, new, 1)
lib_path.write_text(lib)

error_path = Path("crates/medusa-runtime/src/error.rs")
error = error_path.read_text()
old = """        let mut session = load_session(&repo, session_id).map_err(RuntimeError::agent)?;
        validate_resumed_session(&repo, &session)?;
"""
new = """        let mut session = load_session(&repo, session_id).map_err(RuntimeError::agent)?;
        crate::execution_history::verify_resumed_session(&repo, &session)?;
        validate_resumed_session(&repo, &session)?;
"""
if new not in error:
    if error.count(old) != 1:
        raise SystemExit("resume verification insertion target changed")
    error = error.replace(old, new, 1)
error_path.write_text(error)
