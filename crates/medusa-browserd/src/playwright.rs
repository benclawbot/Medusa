use std::io::Write;
use std::process::{Child, Command, Stdio};

use medusa_browser_client::protocol::BrowserRequest;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PlaywrightError {
    #[error("could not spawn playwright bridge: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("playwright bridge exited with code {0}")]
    Exit(i32),
}

pub struct PlaywrightBridge {
    child: Child,
}

impl PlaywrightBridge {
    pub fn spawn() -> Result<Self, PlaywrightError> {
        let child = Command::new("node")
            .arg("browser/playwright_bridge.mjs")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        Ok(Self { child })
    }

    pub fn dispatch(
        &mut self,
        request: &BrowserRequest,
    ) -> Result<serde_json::Value, PlaywrightError> {
        let stdin = self.child.stdin.as_mut().expect("stdin");
        let mut line = serde_json::to_string(request).expect("serialize request");
        line.push('\n');
        stdin.write_all(line.as_bytes())?;
        Ok(serde_json::Value::Null)
    }
}