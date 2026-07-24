use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult, SessionId};
use medusa_escalation::EscalationPacket;

use super::export_manual_escalation;

const CHATGPT_NEW_CHAT_URL: &str = "https://chatgpt.com/";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserAssistedLaunch {
    pub packet_path: PathBuf,
    pub prompt_path: PathBuf,
    pub clipboard_ready: bool,
    pub browser_opened: bool,
}

/// Prepares a bounded escalation prompt, copies it to the system clipboard when possible, and
/// opens ChatGPT in the user's normal browser profile. It never submits the prompt, reads browser
/// cookies, calls private endpoints, or scrapes the response.
pub fn launch_browser_assisted_escalation(
    repo: &Path,
    session_id: &SessionId,
    packet: &EscalationPacket,
) -> MedusaResult<BrowserAssistedLaunch> {
    let packet_path = export_manual_escalation(repo, session_id, packet)?;
    let prompt = render_chatgpt_prompt(packet)?;
    let prompt_path = packet_path.with_extension("prompt.txt");
    fs::write(&prompt_path, &prompt).map_err(transport_error)?;

    let clipboard_ready = copy_to_clipboard(&prompt).is_ok();
    let browser_opened = open_chatgpt().is_ok();
    if !browser_opened {
        return Err(transport_error(format!(
            "could not open ChatGPT; prompt remains available at {}",
            prompt_path.display()
        )));
    }

    Ok(BrowserAssistedLaunch {
        packet_path,
        prompt_path,
        clipboard_ready,
        browser_opened,
    })
}

pub fn render_chatgpt_prompt(packet: &EscalationPacket) -> MedusaResult<String> {
    if !packet.verify_digest().map_err(transport_error)? {
        return Err(transport_error("escalation packet digest is invalid"));
    }
    let packet_json = serde_json::to_string_pretty(packet).map_err(transport_error)?;
    Ok(format!(
        "You are providing bounded reasoning advice to Medusa, a local coding agent.\n\
Do not claim to have executed commands, changed files, approved tools, or verified results.\n\
Return concise advice only. Medusa will independently inspect, apply, and test it.\n\
\nDecision requested: {}\n\
\nEscalation packet (integrity-bound JSON):\n```json\n{}\n```\n\
\nRespond with:\n1. Recommended decision\n2. Reasoning\n3. Risks or assumptions\n4. Concrete verification steps",
        packet.decision_question, packet_json
    ))
}

fn copy_to_clipboard(text: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        return pipe_to("pbcopy", &[], text);
    }
    #[cfg(target_os = "windows")]
    {
        return pipe_to("clip", &[], text);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if pipe_to("wl-copy", &[], text).is_ok() {
            return Ok(());
        }
        return pipe_to("xclip", &["-selection", "clipboard"], text);
    }
    #[allow(unreachable_code)]
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "clipboard integration is not supported on this platform",
    ))
}

fn pipe_to(program: &str, args: &[&str], text: &str) -> std::io::Result<()> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or_else(|| std::io::Error::other("clipboard process stdin unavailable"))?
        .write_all(text.as_bytes())?;
    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other("clipboard command failed"))
    }
}

fn open_chatgpt() -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let status = Command::new("open").arg(CHATGPT_NEW_CHAT_URL).status()?;
    #[cfg(target_os = "windows")]
    let status = Command::new("cmd")
        .args(["/C", "start", "", CHATGPT_NEW_CHAT_URL])
        .status()?;
    #[cfg(all(unix, not(target_os = "macos")))]
    let status = Command::new("xdg-open").arg(CHATGPT_NEW_CHAT_URL).status()?;

    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other("browser launch command failed"))
    }
}

fn transport_error(message: impl ToString) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use medusa_escalation::{EscalationMode, EscalationReason};
    use time::OffsetDateTime;

    use super::*;

    #[test]
    fn prompt_preserves_bounded_trust_boundary_and_packet_identity() {
        let packet = EscalationPacket::new(
            "packet-browser-1",
            "session-1",
            "task-1",
            EscalationMode::Manual,
            "repair parser",
            "Which invariant is broken?",
            BTreeSet::from([EscalationReason::ExplicitUserRequest]),
            OffsetDateTime::UNIX_EPOCH,
        )
        .expect("packet");
        let prompt = render_chatgpt_prompt(&packet).expect("prompt");
        assert!(prompt.contains("packet-browser-1"));
        assert!(prompt.contains("independently inspect, apply, and test"));
        assert!(prompt.contains("Which invariant is broken?"));
    }

    #[test]
    fn mutated_packet_is_rejected_before_browser_launch() {
        let mut packet = EscalationPacket::new(
            "packet-browser-2",
            "session-1",
            "task-1",
            EscalationMode::Manual,
            "repair parser",
            "Which invariant is broken?",
            BTreeSet::from([EscalationReason::ExplicitUserRequest]),
            OffsetDateTime::UNIX_EPOCH,
        )
        .expect("packet");
        packet.objective = "changed after signing".into();
        assert!(render_chatgpt_prompt(&packet).is_err());
    }
}
