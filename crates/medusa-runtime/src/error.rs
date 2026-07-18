use std::{io, path::PathBuf};

#[derive(Debug)]
pub enum RuntimeError {
    Agent(String),
    Io(io::Error),
    Png(String),
    WorkerStopped,
    Busy,
    EmptyPrompt,
    TurnLimit(u32),
    InvalidCommand(String),
    BinaryFile { path: PathBuf },
    FileTooLarge { path: PathBuf, bytes: usize },
}

impl RuntimeError {
    pub(crate) fn agent(error: impl std::fmt::Display) -> Self {
        Self::Agent(error.to_string())
    }

    pub(crate) fn png(error: impl std::fmt::Display) -> Self {
        Self::Png(error.to_string())
    }
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Agent(error) => write!(formatter, "agent runtime failed: {error}"),
            Self::Io(error) => write!(formatter, "runtime I/O failed: {error}"),
            Self::Png(error) => write!(formatter, "screenshot encoding failed: {error}"),
            Self::WorkerStopped => formatter.write_str("agent runtime worker stopped"),
            Self::Busy => formatter.write_str("an agent task is already running"),
            Self::EmptyPrompt => formatter.write_str("prompt and attachments are empty"),
            Self::TurnLimit(limit) => write!(formatter, "agent reached the {limit}-turn limit"),
            Self::InvalidCommand(error) => formatter.write_str(error),
            Self::BinaryFile { path } => write!(
                formatter,
                "attached file is not UTF-8 text: {}",
                path.display()
            ),
            Self::FileTooLarge { path, bytes } => write!(
                formatter,
                "attached file is too large for prompt context: {} ({bytes} bytes)",
                path.display()
            ),
        }
    }
}

impl std::error::Error for RuntimeError {}

impl From<io::Error> for RuntimeError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
