use std::{
    fs,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};

use medusa_daemon::{ConfinedDir, ConfinedReadError as PreviewReadError};
use tauri::{AppHandle, State, http};

use crate::{
    dto::DesktopWebArtifact,
    runtime::{self, RuntimeRegistry},
};

#[derive(Clone, Debug)]
struct AuthorizedPreview {
    runtime_id: String,
    root: PathBuf,
    directory: Arc<ConfinedDir>,
}

#[derive(Default)]
pub struct PreviewRegistry {
    active: Mutex<Option<AuthorizedPreview>>,
}

impl PreviewRegistry {
    fn authorize(&self, runtime_id: &str, artifact: &DesktopWebArtifact) -> Result<(), String> {
        let root = authorize_artifact_tree(Path::new(&artifact.path))?;
        let directory = ConfinedDir::open(&root)
            .map_err(|error| format!("cannot open confined preview root: {error:?}"))?;
        *self
            .active
            .lock()
            .map_err(|_| "desktop preview registry is poisoned".to_owned())? =
            Some(AuthorizedPreview {
                runtime_id: runtime_id.to_owned(),
                root,
                directory: Arc::new(directory),
            });
        Ok(())
    }

    fn revoke(&self, runtime_id: &str) {
        if let Ok(mut active) = self.active.lock()
            && active
                .as_ref()
                .is_some_and(|preview| preview.runtime_id == runtime_id)
        {
            *active = None;
        }
    }

    fn access(&self) -> Result<Option<(PathBuf, Arc<ConfinedDir>)>, String> {
        Ok(self
            .active
            .lock()
            .map_err(|_| "desktop preview registry is poisoned".to_owned())?
            .as_ref()
            .map(|preview| (preview.root.clone(), Arc::clone(&preview.directory))))
    }
}

/// Find the newest runtime-produced index and atomically make only its containing tree readable
/// through Medusa's custom `asset` protocol. The original physical path is retained in the DTO so
/// the existing `convertFileSrc` frontend adapter can remain a pure URL conversion.
#[tauri::command(rename = "runtime_find_web_artifact")]
pub fn preview_runtime_find_web_artifact(
    runtime_id: String,
    runtimes: State<'_, RuntimeRegistry>,
    previews: State<'_, PreviewRegistry>,
) -> Result<Option<DesktopWebArtifact>, String> {
    let Some(artifact) = runtime::runtime_find_web_artifact(runtime_id.clone(), runtimes)? else {
        return Ok(None);
    };
    previews.authorize(&runtime_id, &artifact)?;
    Ok(Some(artifact))
}

/// Close the runtime and revoke its preview authority in the same registered IPC command.
#[tauri::command(rename = "runtime_close")]
pub fn preview_runtime_close(
    runtime_id: String,
    runtimes: State<'_, RuntimeRegistry>,
    previews: State<'_, PreviewRegistry>,
) -> Result<(), String> {
    let result = runtime::runtime_close(runtime_id.clone(), runtimes);
    previews.revoke(&runtime_id);
    result
}

/// Backend for Medusa's registered `asset` scheme. Registration happens before Tauri installs its
/// optional built-in handler, so every renderer request reaches this authorization boundary.
pub fn handle_protocol_request(
    _app: &AppHandle,
    webview_label: &str,
    request_path: &str,
    previews: State<'_, PreviewRegistry>,
) -> http::Response<Vec<u8>> {
    if webview_label != "main" {
        return response(
            http::StatusCode::FORBIDDEN,
            "text/plain; charset=utf-8",
            b"preview denied".to_vec(),
        );
    }

    let (root, directory) = match previews.access() {
        Ok(Some(access)) => access,
        Ok(None) => {
            return response(
                http::StatusCode::NOT_FOUND,
                "text/plain; charset=utf-8",
                b"preview unavailable".to_vec(),
            );
        }
        Err(_) => {
            return response(
                http::StatusCode::INTERNAL_SERVER_ERROR,
                "text/plain; charset=utf-8",
                b"preview unavailable".to_vec(),
            );
        }
    };

    let decoded = match percent_decode_path(request_path) {
        Ok(path) => path,
        Err(()) => {
            return response(
                http::StatusCode::BAD_REQUEST,
                "text/plain; charset=utf-8",
                b"invalid preview path".to_vec(),
            );
        }
    };

    // convertFileSrc encodes the complete absolute path as one URI segment. Relative resources
    // subsequently requested by the rendered document do not contain an encoded slash/backslash,
    // so they are resolved from the one authorized artifact root instead of from the filesystem.
    let encoded = request_path.trim_start_matches('/');
    let absolute_request = contains_encoded_separator(encoded);
    let requested = if absolute_request {
        decoded_absolute_path(&decoded)
    } else {
        PathBuf::from(decoded.trim_start_matches('/'))
    };

    let read = if absolute_request {
        read_absolute_preview_file(&root, &directory, &requested)
    } else {
        read_preview_file(&directory, &requested)
    };
    match read {
        Ok(bytes) => response(
            http::StatusCode::OK,
            content_type(requested.to_string_lossy().as_ref()),
            bytes,
        ),
        Err(PreviewReadError::Invalid | PreviewReadError::Symlink) => response(
            http::StatusCode::FORBIDDEN,
            "text/plain; charset=utf-8",
            b"preview denied".to_vec(),
        ),
        Err(PreviewReadError::Missing) => response(
            http::StatusCode::NOT_FOUND,
            "text/plain; charset=utf-8",
            b"preview resource not found".to_vec(),
        ),
        Err(PreviewReadError::Io) => response(
            http::StatusCode::INTERNAL_SERVER_ERROR,
            "text/plain; charset=utf-8",
            b"preview unavailable".to_vec(),
        ),
    }
}

fn contains_encoded_separator(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("%2f") || lower.contains("%5c")
}

fn decoded_absolute_path(decoded: &str) -> PathBuf {
    #[cfg(windows)]
    {
        let without_url_slash = decoded.trim_start_matches('/');
        PathBuf::from(without_url_slash)
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(decoded)
    }
}

fn authorize_artifact_tree(index: &Path) -> Result<PathBuf, String> {
    if index.file_name().and_then(|name| name.to_str()) != Some("index.html") {
        return Err("rendered webpage must be an index.html artifact".to_owned());
    }

    let execution_root = index
        .ancestors()
        .find(|ancestor| {
            ancestor.file_name().and_then(|value| value.to_str()) == Some("executions")
                && ancestor
                    .parent()
                    .and_then(Path::file_name)
                    .and_then(|value| value.to_str())
                    == Some(".medusa")
        })
        .ok_or_else(|| "rendered webpage is outside .medusa/executions".to_owned())?;

    reject_symlink(execution_root.parent().unwrap_or(execution_root))?;
    reject_symlink(execution_root)?;
    reject_symlinks_below(execution_root, index)?;

    let canonical_execution_root = fs::canonicalize(execution_root)
        .map_err(|error| format!("cannot resolve preview execution root: {error}"))?;
    let canonical_index = fs::canonicalize(index)
        .map_err(|error| format!("cannot resolve rendered webpage: {error}"))?;
    if !canonical_index.starts_with(&canonical_execution_root) || !canonical_index.is_file() {
        return Err("rendered webpage is outside the active execution artifacts".to_owned());
    }
    canonical_index
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "rendered webpage has no artifact directory".to_owned())
}

fn reject_symlink(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect preview path: {error}"))?;
    if metadata.file_type().is_symlink() {
        return Err("rendered webpage path contains a symlink".to_owned());
    }
    Ok(())
}

fn reject_symlinks_below(root: &Path, target: &Path) -> Result<(), String> {
    let relative = target
        .strip_prefix(root)
        .map_err(|_| "rendered webpage is outside the execution root".to_owned())?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err("rendered webpage path contains an invalid component".to_owned());
        }
        current.push(component.as_os_str());
        reject_symlink(&current)?;
    }
    Ok(())
}

fn read_absolute_preview_file(
    root: &Path,
    directory: &ConfinedDir,
    requested: &Path,
) -> Result<Vec<u8>, PreviewReadError> {
    let relative = requested
        .strip_prefix(root)
        .map_err(|_| PreviewReadError::Invalid)?;
    read_preview_file(directory, relative)
}

fn read_preview_file(
    directory: &ConfinedDir,
    relative: &Path,
) -> Result<Vec<u8>, PreviewReadError> {
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(PreviewReadError::Invalid);
    }
    directory.read(relative)
}

fn percent_decode_path(input: &str) -> Result<String, ()> {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(());
            }
            let high = hex_value(bytes[index + 1]).ok_or(())?;
            let low = hex_value(bytes[index + 2]).ok_or(())?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| ())
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn content_type(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("wasm") => "application/wasm",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("txt") => "text/plain; charset=utf-8",
        Some("xml") => "application/xml; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn response(
    status: http::StatusCode,
    content_type: &str,
    body: Vec<u8>,
) -> http::Response<Vec<u8>> {
    http::Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, content_type)
        .header(http::header::CACHE_CONTROL, "no-store")
        .header("x-content-type-options", "nosniff")
        .body(body)
        .unwrap_or_else(|_| http::Response::new(Vec::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preview_tree() -> (crate::test_tempfile::TempDir, PathBuf, PathBuf) {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory
            .path()
            .join("project-outside-home")
            .join(".medusa")
            .join("executions")
            .join("run-1")
            .join("output");
        fs::create_dir_all(root.join("assets")).expect("create preview tree");
        let index = root.join("index.html");
        fs::write(
            &index,
            b"<link rel=\"stylesheet\" href=\"assets/site.css\">",
        )
        .expect("write index");
        fs::write(root.join("assets/site.css"), b"body {}").expect("write css");
        (directory, root, index)
    }

    fn authorized_reader(index: &Path) -> (PathBuf, ConfinedDir) {
        let root = authorize_artifact_tree(index).expect("authorize");
        let directory = ConfinedDir::open(&root).expect("confined root");
        (root, directory)
    }

    #[test]
    fn authorization_is_limited_to_the_selected_execution_index_tree() {
        let (_directory, root, index) = preview_tree();
        assert_eq!(
            authorize_artifact_tree(&index).expect("authorize"),
            fs::canonicalize(root).expect("canonical root")
        );
    }

    #[test]
    fn nested_and_root_relative_preview_resources_use_the_authorized_tree() {
        let (_directory, _root, index) = preview_tree();
        let (_authorized, directory) = authorized_reader(&index);
        assert_eq!(
            read_preview_file(&directory, Path::new("assets/site.css")).expect("read"),
            b"body {}"
        );
    }

    #[test]
    fn encoded_absolute_index_is_confined_to_authorized_root() {
        let (_directory, _root, index) = preview_tree();
        let (authorized, directory) = authorized_reader(&index);
        assert!(read_absolute_preview_file(&authorized, &directory, &index).is_ok());
        let outside = authorized.parent().expect("parent").join("secret.txt");
        fs::write(&outside, b"secret").expect("outside");
        assert_eq!(
            read_absolute_preview_file(&authorized, &directory, &outside),
            Err(PreviewReadError::Invalid)
        );
    }

    #[test]
    fn traversal_and_absolute_relative_paths_are_rejected() {
        let (_directory, _root, index) = preview_tree();
        let (_authorized, directory) = authorized_reader(&index);
        assert_eq!(
            read_preview_file(&directory, Path::new("../secret.txt")),
            Err(PreviewReadError::Invalid)
        );
        assert_eq!(
            read_preview_file(&directory, Path::new("/etc/passwd")),
            Err(PreviewReadError::Invalid)
        );
    }

    #[test]
    fn non_execution_index_is_not_authorized() {
        let directory = tempfile::tempdir().expect("tempdir");
        let index = directory.path().join("index.html");
        fs::write(&index, b"preview").expect("write index");
        assert!(authorize_artifact_tree(&index).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_preview_resources_are_rejected() {
        use std::os::unix::fs::symlink;

        let (directory, _root, index) = preview_tree();
        let outside = directory.path().join("outside.css");
        fs::write(&outside, b"secret").expect("write outside");
        let (authorized, confined) = authorized_reader(&index);
        let link = authorized.join("assets/linked.css");
        symlink(&outside, &link).expect("symlink");
        assert_eq!(
            read_preview_file(&confined, Path::new("assets/linked.css")),
            Err(PreviewReadError::Symlink)
        );
    }

    #[cfg(unix)]
    #[test]
    fn swapped_intermediate_directory_cannot_escape_authorized_root() {
        use std::os::unix::fs::symlink;

        let (directory, _root, index) = preview_tree();
        let outside = directory.path().join("outside");
        fs::create_dir_all(&outside).expect("outside dir");
        fs::write(outside.join("site.css"), b"secret").expect("outside css");
        let (authorized, confined) = authorized_reader(&index);
        fs::rename(authorized.join("assets"), authorized.join("assets-old")).expect("rename");
        symlink(&outside, authorized.join("assets")).expect("swap symlink");
        assert_eq!(
            read_preview_file(&confined, Path::new("assets/site.css")),
            Err(PreviewReadError::Symlink)
        );
    }

    #[test]
    fn convert_file_src_encoded_paths_are_detected_without_confusing_relative_assets() {
        assert!(contains_encoded_separator("%2Ftmp%2Fproject%2Findex.html"));
        assert!(contains_encoded_separator("C%3A%5Cproject%5Cindex.html"));
        assert!(!contains_encoded_separator("assets/site.css"));
    }

    #[test]
    fn malformed_percent_encoding_is_rejected() {
        assert_eq!(percent_decode_path("/%zz"), Err(()));
    }
}
