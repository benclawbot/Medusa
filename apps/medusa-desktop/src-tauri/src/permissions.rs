use medusa_config::{PermissionMode, PermissionStore};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopPermissionMode {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub active: bool,
}

fn mode_dto(mode: PermissionMode, active: PermissionMode) -> DesktopPermissionMode {
    DesktopPermissionMode {
        id: mode.id(),
        label: mode.label(),
        description: mode.description(),
        active: mode == active,
    }
}

#[tauri::command]
pub fn desktop_permission_modes() -> Result<Vec<DesktopPermissionMode>, String> {
    let active = PermissionStore::user()
        .and_then(|store| store.load())
        .map_err(|error| error.to_string())?;
    Ok(PermissionMode::ALL
        .into_iter()
        .map(|mode| mode_dto(mode, active))
        .collect())
}

#[tauri::command]
pub fn desktop_permission_mode() -> Result<DesktopPermissionMode, String> {
    let active = PermissionStore::user()
        .and_then(|store| store.load())
        .map_err(|error| error.to_string())?;
    Ok(mode_dto(active, active))
}

#[tauri::command]
pub fn desktop_set_permission_mode(mode: String) -> Result<DesktopPermissionMode, String> {
    let mode = PermissionMode::parse(&mode).map_err(|error| error.to_string())?;
    PermissionStore::user()
        .and_then(|store| store.save(mode))
        .map_err(|error| error.to_string())?;
    Ok(mode_dto(mode, mode))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_labels_match_codex_permission_menu() {
        let labels = PermissionMode::ALL
            .into_iter()
            .map(PermissionMode::label)
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            vec![
                "Ask for approval",
                "Approve for me",
                "Full Access",
                "Read Only"
            ]
        );
    }
}
