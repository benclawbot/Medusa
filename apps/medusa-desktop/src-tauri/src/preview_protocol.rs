use std::{fs, path::{Path, PathBuf}};

use tauri::{Manager, http::{Request, Response, StatusCode, header::CONTENT_TYPE}};

const PREVIEW_PROTOCOL: &str = "asset";

pub fn preview_protocol_response(
    context: tauri::UriSchemeContext<'_, tauri::Wry>,
    request: Request<Vec<u8>>,
) -> Response<Vec<u8>> {
    let registry = context.app_handle().state::<RuntimeRegistry>();
    match resolve_preview_request(&registry, request.uri().path()) {
        Ok(path) => match fs::read(&path) {
            Ok(bytes) => response(StatusCode::OK, preview_content_type(&path), bytes),
            Err(error) => response(
                StatusCode::NOT_FOUND,
                "text/plain; charset=utf-8",
                format!("preview asset is unavailable: {error}").into_bytes(),
            ),
        },
        Err(error) => response(
            StatusCode::FORBIDDEN,
            "text/plain; charset=utf-8",
            error.into_bytes(),
        ),
    }
}

fn resolve_preview_request(registry: &RuntimeRegistry, uri_path: &str) -> Result<PathBuf, String> {
    let requested = decode_uri_path(uri_path)?;
    let requested = fs::canonicalize(&requested)
        .map_err(|_| "preview asset does not exist".to_owned())?;
    if !requested.is_file() {
        return Err("preview request is not a file".to_owned());
    }

    let entries = registry
        .entries
        .lock()
        .map_err(|_| "desktop runtime registry is poisoned".to_owned())?;
    for entry in entries.values() {
        let entry = entry
            .lock()
            .map_err(|_| "desktop runtime entry is poisoned".to_owned())?;
        let Some(index) = latest_preview_index(&entry.repo) else {
            continue;
        };
        let Some(root) = index.parent() else {
            continue;
        };
        let root = fs::canonicalize(root)
            .map_err(|_| "active preview root is unavailable".to_owned())?;
        if requested.starts_with(&root) {
            return Ok(requested);
        }
    }
    Err("preview asset is outside the active runtime artifact tree".to_owned())
}

fn latest_preview_index(repo: &Path) -> Option<PathBuf> {
    web_artifact_snapshot(repo)
        .into_iter()
        .max_by_key(|(path, modified)| (*modified, path.clone()))
        .map(|(path, _)| path)
}

fn decode_uri_path(uri_path: &str) -> Result<PathBuf, String> {
    let decoded = percent_encoding::percent_decode_str(uri_path)
        .decode_utf8()
        .map_err(|_| "preview path is not valid UTF-8".to_owned())?;
    let decoded = decoded.as_ref();
    #[cfg(windows)]
    let decoded = decoded.trim_start_matches('/').replace('/', "\\");
    #[cfg(not(windows))]
    let decoded = decoded.to_owned();
    let path = PathBuf::from(decoded);
    if !path.is_absolute() {
        return Err("preview path must be absolute".to_owned());
    }
    Ok(path)
}

fn preview_content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|value| value.to_str()).map(str::to_ascii_lowercase).as_deref() {
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
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("wasm") => "application/wasm",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn response(status: StatusCode, content_type: &str, body: Vec<u8>) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, content_type)
        .header("X-Content-Type-Options", "nosniff")
        .body(body)
        .expect("static preview response is valid")
}

#[cfg(test)]
mod preview_protocol_tests {
    use super::*;

    #[test]
    fn latest_preview_root_rejects_other_execution_assets() {
        let directory = crate::tempdir().expect("tempdir");
        let old = directory.path().join(".medusa/executions/old");
        let active = directory.path().join(".medusa/executions/active");
        fs::create_dir_all(&old).expect("old execution");
        fs::write(old.join("index.html"), "old").expect("old index");
        std::thread::sleep(std::time::Duration::from_millis(5));
        fs::create_dir_all(&active).expect("active execution");
        fs::write(active.join("index.html"), "active").expect("active index");
        fs::write(active.join("app.css"), "body {} ").expect("asset");

        let index = latest_preview_index(directory.path()).expect("latest preview");
        assert_eq!(index, active.join("index.html"));
        assert!(active.join("app.css").starts_with(index.parent().expect("root")));
        assert!(!old.join("index.html").starts_with(index.parent().expect("root")));
    }

    #[test]
    fn preview_content_types_cover_relative_web_assets() {
        assert_eq!(preview_content_type(Path::new("index.html")), "text/html; charset=utf-8");
        assert_eq!(preview_content_type(Path::new("app.css")), "text/css; charset=utf-8");
        assert_eq!(preview_content_type(Path::new("app.js")), "text/javascript; charset=utf-8");
        assert_eq!(preview_content_type(Path::new("photo.png")), "image/png");
    }
}
