use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Creates a background process command suitable for a GUI desktop application.
///
/// On Windows, child console programs such as `git.exe` must not allocate a visible console
/// window. Keeping this policy in one helper prevents polling and modal actions from flashing
/// command prompts over the desktop UI.
pub fn hidden_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    configure_hidden(&mut command);
    command
}

#[cfg(windows)]
fn configure_hidden(command: &mut Command) {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn configure_hidden(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::Path};

    #[test]
    fn helper_preserves_the_requested_program() {
        let command = hidden_command("git");
        assert_eq!(command.get_program(), "git");
    }

    #[test]
    fn every_desktop_subprocess_uses_the_hidden_command_policy() {
        let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        for entry in fs::read_dir(&source_root).expect("read desktop Rust sources") {
            let path = entry.expect("source entry").path();
            if path.extension().and_then(|value| value.to_str()) != Some("rs")
                || path.file_name().and_then(|value| value.to_str()) == Some("desktop_command.rs")
            {
                continue;
            }
            let source = fs::read_to_string(&path).expect("read desktop Rust source");
            if source.contains("Command::new(") {
                offenders.push(
                    path.file_name()
                        .and_then(|value| value.to_str())
                        .unwrap_or("<unknown>")
                        .to_owned(),
                );
            }
        }
        offenders.sort();
        assert!(
            offenders.is_empty(),
            "desktop subprocesses bypass hidden_command in: {}",
            offenders.join(", ")
        );
    }
}
