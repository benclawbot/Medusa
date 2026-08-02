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

    #[test]
    fn helper_preserves_the_requested_program() {
        let command = hidden_command("git");
        assert_eq!(command.get_program(), "git");
    }
}
