from pathlib import Path


def replace_once(source: str, old: str, new: str, label: str) -> str:
    if new in source:
        return source
    count = source.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected one target, found {count}")
    return source.replace(old, new, 1)


live_path = Path("crates/medusa-daemon/src/live_session.rs")
live = live_path.read_text()
live = replace_once(
    live,
    '''    pub fn attach_current(
        &mut self,
        session_id: &str,
        client_id: String,
        client_kind: ClientKind,
        requested_mode: AttachmentMode,
        cursor: u64,
        occurred_at_unix_ms: i64,
        event_id: String,
    ) -> Result<LiveSessionAttachmentView, LiveSessionBrokerError> {
        let store = ContinuityStore::new(
            self.repo
                .join(".medusa/continuity")
                .join(format!("{session_id}.json")),
        );
        let expected_revision = match store.load() {
            Ok(continuity) => continuity.revision,
            Err(ContinuityError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                0
            }
            Err(error) => return Err(LiveSessionBrokerError::Session(error.to_string())),
        };
        self.attach(RuntimeAttachRequest {
            session_id: session_id.to_owned(),
            client_id,
            client_kind,
            requested_mode,
            expected_revision,
            cursor,
            occurred_at_unix_ms,
            event_id,
        })
    }
''',
    '''    pub fn attach_current(
        &mut self,
        mut request: RuntimeAttachRequest,
    ) -> Result<LiveSessionAttachmentView, LiveSessionBrokerError> {
        let store = ContinuityStore::new(
            self.repo
                .join(".medusa/continuity")
                .join(format!("{}.json", request.session_id)),
        );
        request.expected_revision = match store.load() {
            Ok(continuity) => continuity.revision,
            Err(ContinuityError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                0
            }
            Err(error) => return Err(LiveSessionBrokerError::Session(error.to_string())),
        };
        self.attach(request)
    }
''',
    "typed current attachment request",
)
live_path.write_text(live)

path = Path("crates/medusa-daemon/src/frontend_control.rs")
source = path.read_text()
source = replace_once(
    source,
    '''use medusa_runtime::{RuntimeController, SubmitDisposition, prompt::PromptDraft};
''',
    '''use medusa_runtime::{
    RuntimeController, SubmitDisposition,
    commands::{Effort, ModelCommand, SlashCommand, TeamCommand},
    prompt::PromptDraft,
};
''',
    "runtime command imports",
)
source = replace_once(
    source,
    '''use medusa_runtime::attachment::session::{AttachmentMode, ClientKind};
''',
    '''use medusa_runtime::attachment::session::{
    AttachmentMode, ClientKind, RuntimeAttachRequest,
};
''',
    "typed attachment import",
)
source = replace_once(
    source,
    '''    CancellationRequested {
        session_id: String,
        requested: bool,
    },
    Status {
''',
    '''    CancellationRequested {
        session_id: String,
        requested: bool,
    },
    CommandAccepted {
        session_id: String,
        command: String,
    },
    Status {
''',
    "generic command acknowledgement",
)
source = replace_once(
    source,
    '''                let attachment = self.broker.attach_current(
                    session_id,
                    envelope.client_id.clone(),
                    client_kind(envelope.frontend),
                    attachment_mode(*mode),
                    after_cursor.unwrap_or_default(),
                    timestamp_unix_ms(envelope.timestamp),
                    envelope.command_id.clone(),
                )?;
''',
    '''                if *mode == FrontendAttachmentMode::Owner {
                    if !self.controllers.contains_key(session_id) {
                        return Err(FrontendControlError::RuntimeNotActive(
                            session_id.clone(),
                        ));
                    }
                    self.control_clients
                        .insert(session_id.clone(), envelope.client_id.clone());
                }
                let attachment = self.broker.attach_current(RuntimeAttachRequest {
                    session_id: session_id.clone(),
                    client_id: envelope.client_id.clone(),
                    client_kind: client_kind(envelope.frontend),
                    requested_mode: AttachmentMode::ReadOnly,
                    expected_revision: 0,
                    cursor: after_cursor.unwrap_or_default(),
                    occurred_at_unix_ms: timestamp_unix_ms(envelope.timestamp),
                    event_id: envelope.command_id.clone(),
                })?;
''',
    "frontend attach routing",
)
source = source.replace(
    '''                self.broker.attach_current(
                    session_id,
                    daemon_client_id.clone(),
                    ClientKind::Daemon,
                    AttachmentMode::Owner,
                    0,
                    timestamp_unix_ms(envelope.timestamp),
                    format!("{}:daemon-owner", envelope.command_id),
                )?;
                let attachment = self.broker.attach_current(
                    session_id,
                    envelope.client_id.clone(),
                    client_kind(envelope.frontend),
                    AttachmentMode::ReadOnly,
                    0,
                    timestamp_unix_ms(envelope.timestamp),
                    format!("{}:frontend", envelope.command_id),
                )?;
''',
    '''                self.broker.attach_current(RuntimeAttachRequest {
                    session_id: session_id.clone(),
                    client_id: daemon_client_id.clone(),
                    client_kind: ClientKind::Daemon,
                    requested_mode: AttachmentMode::Owner,
                    expected_revision: 0,
                    cursor: 0,
                    occurred_at_unix_ms: timestamp_unix_ms(envelope.timestamp),
                    event_id: format!("{}:daemon-owner", envelope.command_id),
                })?;
                let attachment = self.broker.attach_current(RuntimeAttachRequest {
                    session_id: session_id.clone(),
                    client_id: envelope.client_id.clone(),
                    client_kind: client_kind(envelope.frontend),
                    requested_mode: AttachmentMode::ReadOnly,
                    expected_revision: 0,
                    cursor: 0,
                    occurred_at_unix_ms: timestamp_unix_ms(envelope.timestamp),
                    event_id: format!("{}:frontend", envelope.command_id),
                })?;
''',
)
source = replace_once(
    source,
    '''                self.controllers.insert(session_id.clone(), controller);
                self.control_clients
                    .insert(session_id.clone(), envelope.client_id.clone());
                Ok(FrontendControlResult::SubmissionAccepted {
''',
    '''                self.broker.attach_current(RuntimeAttachRequest {
                    session_id: session_id.clone(),
                    client_id: format!("daemon-runtime:{session_id}"),
                    client_kind: ClientKind::Daemon,
                    requested_mode: AttachmentMode::Owner,
                    expected_revision: 0,
                    cursor: 0,
                    occurred_at_unix_ms: timestamp_unix_ms(envelope.timestamp),
                    event_id: format!("{}:daemon-owner", envelope.command_id),
                })?;
                self.broker.attach_current(RuntimeAttachRequest {
                    session_id: session_id.clone(),
                    client_id: envelope.client_id.clone(),
                    client_kind: client_kind(envelope.frontend),
                    requested_mode: AttachmentMode::ReadOnly,
                    expected_revision: 0,
                    cursor: 0,
                    occurred_at_unix_ms: timestamp_unix_ms(envelope.timestamp),
                    event_id: format!("{}:frontend", envelope.command_id),
                })?;
                self.controllers.insert(session_id.clone(), controller);
                self.control_clients
                    .insert(session_id.clone(), envelope.client_id.clone());
                Ok(FrontendControlResult::SubmissionAccepted {
''',
    "new session continuity attachments",
)
source = replace_once(
    source,
    '''            FrontendCommand::ConfigureModel { .. }
            | FrontendCommand::SetEffort { .. }
            | FrontendCommand::SetPlanMode { .. }
            | FrontendCommand::SteerWorker { .. }
            | FrontendCommand::CancelWorker { .. }
            | FrontendCommand::StopTeam => Err(FrontendControlError::UnsupportedCommand(
                "command mapping is not yet available in the daemon control plane",
            )),
''',
    '''            FrontendCommand::ConfigureModel { provider, model } => {
                let session_id = required_session_id(envelope)?;
                self.authorize_control(&session_id, &envelope.client_id)?;
                let controller = self.controller(&session_id)?;
                if let Some(provider) = provider {
                    controller.run_command(SlashCommand::Model(ModelCommand::SetProvider(
                        provider.clone(),
                    )))?;
                }
                controller.run_command(SlashCommand::Model(ModelCommand::SetModel(
                    model.clone(),
                )))?;
                Ok(FrontendControlResult::CommandAccepted {
                    session_id,
                    command: "configure_model".to_owned(),
                })
            }
            FrontendCommand::SetEffort { effort } => {
                let session_id = required_session_id(envelope)?;
                self.authorize_control(&session_id, &envelope.client_id)?;
                self.controller(&session_id)?.run_command(SlashCommand::Effort {
                    effort: Some(parse_effort(effort)?),
                })?;
                Ok(FrontendControlResult::CommandAccepted {
                    session_id,
                    command: "set_effort".to_owned(),
                })
            }
            FrontendCommand::SetPlanMode { enabled } => {
                let session_id = required_session_id(envelope)?;
                self.authorize_control(&session_id, &envelope.client_id)?;
                self.controller(&session_id)?.run_command(SlashCommand::Plan {
                    task: (!enabled).then(|| "off".to_owned()),
                })?;
                Ok(FrontendControlResult::CommandAccepted {
                    session_id,
                    command: "set_plan_mode".to_owned(),
                })
            }
            FrontendCommand::SteerWorker {
                worker_id,
                instruction,
            } => {
                let session_id = required_session_id(envelope)?;
                self.authorize_control(&session_id, &envelope.client_id)?;
                self.controller(&session_id)?.run_command(SlashCommand::Team(
                    TeamCommand::Steer {
                        worker_id: worker_id.clone(),
                        instruction: instruction.clone(),
                    },
                ))?;
                Ok(FrontendControlResult::CommandAccepted {
                    session_id,
                    command: "steer_worker".to_owned(),
                })
            }
            FrontendCommand::CancelWorker { worker_id } => {
                let session_id = required_session_id(envelope)?;
                self.authorize_control(&session_id, &envelope.client_id)?;
                self.controller(&session_id)?.run_command(SlashCommand::Team(
                    TeamCommand::StopWorker {
                        worker_id: worker_id.clone(),
                    },
                ))?;
                Ok(FrontendControlResult::CommandAccepted {
                    session_id,
                    command: "cancel_worker".to_owned(),
                })
            }
            FrontendCommand::StopTeam => {
                let session_id = required_session_id(envelope)?;
                self.authorize_control(&session_id, &envelope.client_id)?;
                self.controller(&session_id)?
                    .run_command(SlashCommand::Team(TeamCommand::StopTeam))?;
                Ok(FrontendControlResult::CommandAccepted {
                    session_id,
                    command: "stop_team".to_owned(),
                })
            }
''',
    "supported frontend runtime commands",
)
helper = '''fn parse_effort(value: &str) -> Result<Effort, FrontendControlError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "low" => Ok(Effort::Low),
        "medium" => Ok(Effort::Medium),
        "high" => Ok(Effort::High),
        "auto" => Ok(Effort::Auto),
        _ => Err(FrontendControlError::InvalidEffort(value.to_owned())),
    }
}

'''
marker = "fn command_session_id(envelope: &FrontendCommandEnvelope) -> Option<String> {\n"
if helper not in source:
    if source.count(marker) != 1:
        raise SystemExit("effort helper insertion target changed")
    source = source.replace(marker, helper + marker, 1)
source = replace_once(
    source,
    '''    #[error("runtime for session {0} is not active")]
    RuntimeNotActive(String),
''',
    '''    #[error("runtime for session {0} is not active")]
    RuntimeNotActive(String),
    #[error("invalid effort level {0}")]
    InvalidEffort(String),
''',
    "invalid effort error",
)
path.write_text(source)
