mod dto;
mod runtime;

use runtime::{
    RuntimeRegistry, runtime_cancel, runtime_close, runtime_command, runtime_configure_model,
    runtime_poll, runtime_start, runtime_submit,
};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(RuntimeRegistry::default())
        .invoke_handler(tauri::generate_handler![
            runtime_start,
            runtime_close,
            runtime_submit,
            runtime_command,
            runtime_cancel,
            runtime_poll,
            runtime_configure_model,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Medusa Desktop");
}
