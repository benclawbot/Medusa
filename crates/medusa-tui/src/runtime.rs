use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use medusa_agent::{AgentEngine, StepOutcome};
use medusa_config::Config;
use medusa_provider::{ImageSource, MessageBlock, MiniMaxProvider, Role};

use crate::clipboard::{ImageAttachment, PromptAttachment, PromptDraft};

const MAX_FILE_CONTEXT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug)]
pub enum RuntimeCommand {
    Submit(PromptDraft),
    Shutdown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeEvent {
    Started,
    Progress {
        turn: u32,
    },
    Completed {
        session_id: String,
        assistant_text: String,
        evidence: Vec<String>,
    },
    Cancelled,
    Failed(String),
}

pub struct RuntimeController {
    commands: Sender<RuntimeCommand>,
    events: Receiver<RuntimeEvent>,
    cancel: Arc<AtomicBool>,
    busy: Arc<AtomicBool>,
}

impl RuntimeController {
    pub fn start(repo: PathBuf) -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let busy = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let worker_busy = Arc::clone(&busy);
        thread::Builder::new()
            .name("medusa-tui-runtime".to_owned())
            .spawn(move || {
                worker_loop(repo, command_rx, event_tx, worker_cancel, worker_busy);
            })
            .expect("spawn TUI runtime worker");
        Self {
            commands: command_tx,
            events: event_rx,
            cancel,
            busy,
        }
    }

    pub fn submit(&self, draft: PromptDraft) -> Result<(), RuntimeError> {
        self.busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| RuntimeError::Busy)?;
        self.cancel.store(false, Ordering::SeqCst);
        if self.commands.send(RuntimeCommand::Submit(draft)).is_err() {
            self.busy.store(false, Ordering::SeqCst);
            return Err(RuntimeError::WorkerStopped);
        }
        Ok(())
    }

    pub fn cancel(&self) -> bool {
        if self.busy.load(Ordering::SeqCst) {
            self.cancel.store(true, Ordering::SeqCst);
            true
        } else {
            false
        }
    }

    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    pub fn try_event(&self) -> Result<Option<RuntimeEvent>, RuntimeError> {
        match self.events.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(RuntimeError::WorkerStopped),
        }
    }
}

impl Drop for RuntimeController {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        let _ = self.commands.send(RuntimeCommand::Shutdown);
    }
}

fn worker_loop(
    repo: PathBuf,
    commands: Receiver<RuntimeCommand>,
    events: Sender<RuntimeEvent>,
    cancel: Arc<AtomicBool>,
    busy: Arc<AtomicBool>,
) {
    while let Ok(command) = commands.recv() {
        match command {
            RuntimeCommand::Submit(draft) => {
                let _ = events.send(RuntimeEvent::Started);
                let outcome = run_prompt(&repo, draft, &events, &cancel);
                let event = match outcome {
                    Ok(completed) => completed,
                    Err(error) => RuntimeEvent::Failed(error.to_string()),
                };
                busy.store(false, Ordering::SeqCst);
                let _ = events.send(event);
            }
            RuntimeCommand::Shutdown => break,
        }
    }
    busy.store(false, Ordering::SeqCst);
}

fn run_prompt(
    repo: &Path,
    draft: PromptDraft,
    events: &Sender<RuntimeEvent>,
    cancel: &AtomicBool,
) -> Result<RuntimeEvent, RuntimeError> {
    let project = repo.join(".medusa/config.toml");
    let project = project.exists().then_some(project);
    let config = Config::load_layers(None, project.as_deref(), &BTreeMap::new(), &BTreeMap::new())
        .map_err(RuntimeError::agent)?;
    let max_turns = config.agent.max_turns;
    let provider = MiniMaxProvider::from_config(&config).map_err(RuntimeError::agent)?;
    let engine = AgentEngine::new(provider, config);
    let objective = objective_for(&draft);
    let content = message_blocks(&draft)?;
    let mut session = engine
        .create_session_with_content(repo, objective, content)
        .map_err(RuntimeError::agent)?;

    while !session.completed && session.turn < max_turns {
        if cancel.load(Ordering::SeqCst) {
            return Ok(RuntimeEvent::Cancelled);
        }
        let outcome = engine.step(&mut session).map_err(RuntimeError::agent)?;
        let _ = events.send(RuntimeEvent::Progress { turn: session.turn });
        if outcome == StepOutcome::Completed {
            break;
        }
    }

    if cancel.load(Ordering::SeqCst) {
        return Ok(RuntimeEvent::Cancelled);
    }
    if !session.completed {
        return Err(RuntimeError::TurnLimit(max_turns));
    }

    let assistant_text = session
        .messages
        .iter()
        .filter(|message| message.role == Role::Assistant)
        .flat_map(|message| &message.content)
        .filter_map(|block| match block {
            MessageBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    Ok(RuntimeEvent::Completed {
        session_id: session.id.to_string(),
        assistant_text,
        evidence: session.evidence,
    })
}

fn objective_for(draft: &PromptDraft) -> String {
    let trimmed = draft.text.trim();
    if trimmed.is_empty() {
        format!(
            "Use the {} attached item(s) as context and complete the coding task.",
            draft.attachments.len()
        )
    } else {
        trimmed.to_owned()
    }
}

pub fn message_blocks(draft: &PromptDraft) -> Result<Vec<MessageBlock>, RuntimeError> {
    let mut blocks = Vec::new();
    if !draft.text.is_empty() {
        blocks.push(MessageBlock::Text {
            text: draft.text.clone(),
        });
    }
    for attachment in &draft.attachments {
        match attachment {
            PromptAttachment::PastedText(text) => blocks.push(MessageBlock::Text {
                text: format!(
                    "<pasted_text name=\"{}\">\n{}\n</pasted_text>",
                    text.display_name, text.text
                ),
            }),
            PromptAttachment::Image(image) => blocks.push(image_block(image)?),
            PromptAttachment::File(file) => {
                let bytes = fs::read(&file.path)?;
                if bytes.len() > MAX_FILE_CONTEXT_BYTES {
                    return Err(RuntimeError::FileTooLarge {
                        path: file.path.clone(),
                        bytes: bytes.len(),
                    });
                }
                let text = String::from_utf8(bytes).map_err(|_| RuntimeError::BinaryFile {
                    path: file.path.clone(),
                })?;
                blocks.push(MessageBlock::Text {
                    text: format!(
                        "<attached_file path=\"{}\">\n{}\n</attached_file>",
                        file.path.display(),
                        text
                    ),
                });
            }
        }
    }
    if blocks.is_empty() {
        return Err(RuntimeError::EmptyPrompt);
    }
    Ok(blocks)
}

fn image_block(image: &ImageAttachment) -> Result<MessageBlock, RuntimeError> {
    let mut encoded = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut encoded, image.width, image.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(RuntimeError::png)?;
        writer
            .write_image_data(&image.rgba)
            .map_err(RuntimeError::png)?;
    }
    Ok(MessageBlock::Image {
        source: ImageSource::Base64 {
            media_type: "image/png".to_owned(),
            data: STANDARD.encode(encoded),
        },
        alt_text: Some(image.display_name.clone()),
    })
}

#[derive(Debug)]
pub enum RuntimeError {
    Agent(String),
    Io(io::Error),
    Png(String),
    WorkerStopped,
    Busy,
    EmptyPrompt,
    TurnLimit(u32),
    BinaryFile { path: PathBuf },
    FileTooLarge { path: PathBuf, bytes: usize },
}

impl RuntimeError {
    fn agent(error: impl std::fmt::Display) -> Self {
        Self::Agent(error.to_string())
    }

    fn png(error: impl std::fmt::Display) -> Self {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::{ImageAttachment, PromptAttachment};
    use tempfile::tempdir;

    #[test]
    fn text_prompt_becomes_user_message_block() {
        let draft = PromptDraft {
            text: "fix the failing test".to_owned(),
            ..PromptDraft::default()
        };
        assert_eq!(
            message_blocks(&draft).expect("message blocks"),
            vec![MessageBlock::Text {
                text: "fix the failing test".to_owned()
            }]
        );
    }

    #[test]
    fn screenshot_is_encoded_as_png_image_block() {
        let draft = PromptDraft {
            attachments: vec![PromptAttachment::Image(ImageAttachment {
                display_name: "screen.png".to_owned(),
                width: 1,
                height: 1,
                rgba: vec![0, 0, 0, 255],
                source_format: Some("image/rgba8".to_owned()),
            })],
            ..PromptDraft::default()
        };
        let blocks = message_blocks(&draft).expect("message blocks");
        assert!(matches!(
            &blocks[0],
            MessageBlock::Image {
                source: ImageSource::Base64 { media_type, data },
                ..
            } if media_type == "image/png" && !data.is_empty()
        ));
    }

    #[test]
    fn attached_utf8_file_is_bounded_and_included() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("error.txt");
        fs::write(&path, "compiler error").expect("write fixture");
        let draft = PromptDraft {
            attachments: vec![PromptAttachment::File(crate::clipboard::FileAttachment {
                path,
                byte_len: 14,
            })],
            ..PromptDraft::default()
        };
        let blocks = message_blocks(&draft).expect("message blocks");
        assert!(matches!(
            &blocks[0],
            MessageBlock::Text { text } if text.contains("compiler error")
        ));
    }

    #[test]
    fn idle_runtime_cancel_is_a_noop() {
        let directory = tempdir().expect("temporary directory");
        let runtime = RuntimeController::start(directory.path().to_path_buf());
        assert!(!runtime.cancel());
        assert!(!runtime.is_busy());
    }
}
