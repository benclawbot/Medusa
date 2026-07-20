mod credentials;
mod dto;
mod runtime;
mod sessions;
#[cfg(test)]
mod test_tempfile;
#[cfg(test)]
extern crate self as tempfile;
#[cfg(test)]
pub(crate) use test_tempfile::tempdir;

use runtime::{
    RuntimeRegistry, runtime_cancel, runtime_close, runtime_command, runtime_command_suggestions,
    runtime_configure_model, runtime_poll, runtime_start, runtime_submit,
};
use sessions::runtime_list_sessions;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() -> tauri::Result<()> {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(RuntimeRegistry::default())
        .invoke_handler(tauri::generate_handler![
            runtime_start,
            runtime_close,
            runtime_submit,
            runtime_command,
            runtime_command_suggestions,
            runtime_cancel,
            runtime_poll,
            runtime_configure_model,
            runtime_list_sessions,
        ])
        .run(tauri::generate_context!())
}
