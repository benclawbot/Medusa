use medusa_browser_client::protocol::{BrowserResponse, BrowserRequest, ElementRef, TabInfo};
use serde_json::Value;

#[allow(dead_code)]
pub fn build(method: &str, input: &Value) -> Result<BrowserRequest, String> {
    match method {
        "browser_navigate" => {
            let url = input
                .get("url")
                .and_then(Value::as_str)
                .ok_or("url must be a string")?;
            Ok(BrowserRequest::Navigate { url: url.to_owned() })
        }
        "browser_snapshot" => Ok(BrowserRequest::Snapshot),
        "browser_click" => Ok(BrowserRequest::Click {
            ref_id: input.get("ref").and_then(Value::as_u64).map(|n| n as u32),
            selector: input
                .get("selector")
                .and_then(Value::as_str)
                .map(str::to_owned),
        }),
        "browser_fill" => {
            let value = input
                .get("value")
                .and_then(Value::as_str)
                .ok_or("value must be a string")?;
            Ok(BrowserRequest::Fill {
                ref_id: input.get("ref").and_then(Value::as_u64).map(|n| n as u32),
                selector: input
                    .get("selector")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                value: value.to_owned(),
            })
        }
        "browser_press" => {
            let key = input
                .get("key")
                .and_then(Value::as_str)
                .ok_or("key must be a string")?;
            Ok(BrowserRequest::Press { key: key.to_owned() })
        }
        "browser_screenshot" => Ok(BrowserRequest::Screenshot {
            full_page: input
                .get("full_page")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }),
        "browser_evaluate" => {
            let expression = input
                .get("expression")
                .and_then(Value::as_str)
                .ok_or("expression must be a string")?;
            Ok(BrowserRequest::Evaluate {
                expression: expression.to_owned(),
            })
        }
        "browser_tabs" => Ok(BrowserRequest::Tabs),
        "browser_close" => Ok(BrowserRequest::Close),
        "browser_ping" => Ok(BrowserRequest::Ping),
        other => Err(format!("unknown browser method: {other}")),
    }
}

#[allow(dead_code)]
pub fn format_response(response: BrowserResponse) -> (String, Vec<u8>) {
    match response {
        BrowserResponse::Ok => ("ok".to_owned(), Vec::new()),
        BrowserResponse::Navigate { final_url, status } => (
            format!("navigated to {final_url} (status {status})"),
            Vec::new(),
        ),
        BrowserResponse::Snapshot { text, refs } => {
            let mut s = text;
            s.push_str(&format!("\n[{} refs]", refs.len()));
            (s, Vec::new())
        }
        BrowserResponse::Screenshot {
            format,
            bytes_base64,
        } => {
            let decoded = base64_decode(&bytes_base64);
            (format!("screenshot {format} ({} bytes)", decoded.len()), decoded)
        }
        BrowserResponse::Evaluate { value } => (
            serde_json::to_string_pretty(&value).unwrap_or_default(),
            Vec::new(),
        ),
        BrowserResponse::Tabs { tabs } => (format_tabs(&tabs), Vec::new()),
        BrowserResponse::Error { code, message } => {
            (format!("error: {code}: {message}"), Vec::new())
        }
    }
}

fn format_tabs(tabs: &[TabInfo]) -> String {
    let mut s = String::new();
    for tab in tabs {
        s.push_str(&format!("- [{}] {} ({})\n", tab.id, tab.title, tab.url));
    }
    s
}

#[allow(clippy::missing_const_for_fn)]
fn base64_decode(s: &str) -> Vec<u8> {
    // Tiny RFC 4648 base64 decoder. Avoids pulling in the `base64` crate.
    let table: [u8; 256] = {
        let mut t = [255u8; 256];
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        for (i, &b) in alphabet.iter().enumerate() {
            t[b as usize] = i as u8;
        }
        t
    };
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    let mut buf = [0u8; 4];
    let mut buf_len = 0;
    for &b in bytes {
        if b == b'=' || b == b'\n' || b == b'\r' {
            continue;
        }
        if table[b as usize] == 255 {
            continue;
        }
        buf[buf_len] = table[b as usize];
        buf_len += 1;
        if buf_len == 4 {
            out.push((buf[0] << 2) | (buf[1] >> 4));
            out.push((buf[1] << 4) | (buf[2] >> 2));
            out.push((buf[2] << 6) | buf[3]);
            buf_len = 0;
        }
    }
    match buf_len {
        2 => {
            out.push((buf[0] << 2) | (buf[1] >> 4));
        }
        3 => {
            out.push((buf[0] << 2) | (buf[1] >> 4));
            out.push((buf[1] << 4) | (buf[2] >> 2));
        }
        _ => {}
    }
    out
}

pub fn _force_use(refs: &[ElementRef]) {
    let _ = refs;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn build_navigate_extracts_url() {
        let req = build("browser_navigate", &json!({"url": "https://example.com"})).unwrap();
        assert!(matches!(req, BrowserRequest::Navigate { ref url } if url == "https://example.com"));
    }

    #[test]
    fn build_click_extracts_ref_and_selector() {
        let req = build("browser_click", &json!({"ref": 7, "selector": "#x"})).unwrap();
        assert!(matches!(
            req,
            BrowserRequest::Click { ref_id: Some(7), selector: Some(ref s) } if s == "#x"
        ));
    }

    #[test]
    fn build_unknown_method_returns_err() {
        assert!(build("browser_mystery", &json!({})).is_err());
    }

    #[test]
    fn format_response_ok_returns_ok() {
        let (text, bytes) = format_response(BrowserResponse::Ok);
        assert_eq!(text, "ok");
        assert!(bytes.is_empty());
    }

    #[test]
    fn format_response_screenshot_decodes_base64() {
        // "ABCD" base64 → 0x00 0x10 0x83
        let (text, bytes) = format_response(BrowserResponse::Screenshot {
            format: "png".into(),
            bytes_base64: "ABCD".into(),
        });
        assert_eq!(bytes, vec![0x00, 0x10, 0x83]);
        assert!(text.contains("png"));
    }
}