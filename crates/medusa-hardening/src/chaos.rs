use std::{fs, path::Path};

use medusa_core::MedusaResult;

use crate::support::internal;

/// Chaos fixture: repeatedly writes a state file and simulates interrupted temporary files.
pub fn chaos_recovery_cycle(root: &Path, cycles: usize) -> MedusaResult<String> {
    fs::create_dir_all(root)?;
    let state = root.join("state.json");
    for cycle in 0..cycles {
        let temporary = state.with_extension("json.tmp");
        fs::write(&temporary, format!("{{\"cycle\":{cycle}}}"))?;
        if cycle % 3 == 1 {
            fs::remove_file(&temporary)?;
            continue;
        }
        fs::rename(&temporary, &state)?;
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&state)?)?;
        if value.get("cycle").and_then(serde_json::Value::as_u64) != Some(cycle as u64) {
            return Err(internal("chaos recovery observed corrupt state"));
        }
    }
    Ok(format!("chaos-recovery-ok:{cycles}"))
}
