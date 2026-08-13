use serde::{Deserialize, Serialize};

pub const MAX_BROWSER_REQUEST_FRAME_BYTES: usize = 64 * 1024;
pub const MAX_BROWSER_RESPONSE_FRAME_BYTES: usize = 12 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BrowserRpcRequest {
    pub request_id: u64,
    #[serde(flatten)]
    pub request: BrowserRequest,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BrowserRpcResponse {
    pub request_id: u64,
    #[serde(flatten)]
    pub response: BrowserResponse,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum BrowserRequest {
    Ping,
    Navigate { url: String },
    Snapshot,
    Click { ref_id: Option<u32>, selector: Option<String> },
    Fill { ref_id: Option<u32>, selector: Option<String>, value: String },
    Press { key: String },
    Screenshot { full_page: bool },
    Evaluate { expression: String },
    Tabs,
    Close,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BrowserResponse {
    Ok,
    Navigate { final_url: String, status: u16 },
    Snapshot { text: String, refs: Vec<ElementRef> },
    Screenshot { format: String, bytes_base64: String },
    Evaluate { value: serde_json::Value },
    Tabs { tabs: Vec<TabInfo> },
    Error { code: String, message: String },
}

impl BrowserResponse {
    #[must_use]
    pub fn is_ok(&self) -> bool {
        !matches!(self, BrowserResponse::Error { .. })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ElementRef {
    pub id: u32,
    pub role: String,
    pub name: String,
    pub selector: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TabInfo {
    pub id: u32,
    pub url: String,
    pub title: String,
}
