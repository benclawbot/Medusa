//! Shared team coordination tools for independent agent sessions.
//!
//! The durable scheduler and lease controller remain authoritative for task state. Team state is
//! a coordination projection only: instructions that can affect worker reasoning are admitted to
//! the destination `AgentSession` as canonical `SessionAction`s.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, OnceLock},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_protocol::{
    Actor, EventPayload, SessionAction, SessionActionDeliveryPolicy, SessionActionKind,
    SessionActionWakePolicy,
};
use medusa_provider::ToolDefinition;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    evidence::append_event,
    session::{load, persist},
};

type TeamControlSessionKey = (String, String, PathBuf);
type TeamControlSessionRegistry = BTreeMap<TeamControlSessionKey, String>;

static TEAM_REPOSITORIES: OnceLock<Mutex<BTreeMap<String, BTreeSet<PathBuf>>>> = OnceLock::new();
static TEAM_CONTROL_SESSIONS: OnceLock<Mutex<TeamControlSessionRegistry>> = OnceLock::new();

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamRole {
    Lead,
    Planner,
    Researcher,
    Implementer,
    Reviewer,
    Verifier,
}

impl TeamRole {
    #[must_use]
    pub fn can_mutate(self) -> bool {
        matches!(self, Self::Lead | Self::Implementer)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamMemberLifecycle {
    Starting,
    Running,
    Idle,
    ShutdownRequested,
    Stopped,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TeamMember {
    pub id: String,
    pub role: TeamRole,
    pub lifecycle: TeamMemberLifecycle,
    pub current_task: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamMessageDeliveryState {
    #[default]
    Queued,
    ActionAccepted,
    ModelVisible,
    CoordinationOnly,
    Acknowledged,
    LegacyQueued,
    LegacyAcknowledged,
    Rejected,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct DurableTeamMessage {
    sequence: u64,
    #[serde(default)]
    idempotency_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    action_id: Option<String>,
    from: String,
    to: String,
    body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    destination_session_id: Option<String>,
    #[serde(default)]
    delivery_state: TeamMessageDeliveryState,
    #[serde(default, skip_serializing)]
    delivered: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct DurableTeamState {
    team_id: String,
    members: BTreeMap<String, TeamMember>,
    messages: Vec<DurableTeamMessage>,
    next_sequence: u64,
}

#[derive(Clone)]
pub struct TeamRuntime {
    path: PathBuf,
    repo: Option<PathBuf>,
    state: Arc<Mutex<DurableTeamState>>,
}

#[derive(Clone)]
pub struct TeamMemberContext {
    team: TeamRuntime,
    member_id: String,
}

#[derive(Clone, Debug, Default)]
pub struct AgentExecutionPolicy {
    allowed_tools: Option<BTreeSet<String>>,
    allow_user_questions: bool,
    allowed_write_paths: Option<Vec<String>>,
}

impl AgentExecutionPolicy {
    #[must_use]\n    pub fn unrestricted() -> Self {
        Self {
            allowed_tools: None,
            allow_user_questions: true,
            allowed_write_paths: None,
        }
    }

    #[must_use]
    pub fn for_team_role(role: TeamRole) -> Self {
        let mut allowed = BTreeSet::from([
            "fs_read".to_owned(),
            "search_text".to_owned(),
            "semantic_capabilities".to_owned(),
            "code_index".to_owned(),
            "typescript_semantic".to_owned(),
            "web_search".to_owned(),
            "web_fetch".to_owned(),
            "skill_read".to_owned(),
            "skill_execute".to_owned(),
            "analysis_workspace".to_owned(),
            "update_plan".to_owned(),
            "team_list_members".to_owned(),
            "team_read_messages".to_owned(),
            "team_send_message".to_owned(),
        ]);
        if role.can_mutate() {
            allowed.extend([
                "shell_run".to_owned(),
                "fs_create_dir".to_owned(),
                "fs_write".to_owned(),
                "patch_apply".to_owned(),
                "symbol_rename".to_owned(),
                "git_checkpoint".to_owned(),
            ]);
        }
        Self {
            allowed_tools: Some(allowed),
            allow_user_questions: false,
            allowed_write_paths: None,
        }
    }

    #[must_use]
    pub fn with_allowed_write_paths(mut self, paths: impl IntoIterator<Item = String>) -> Self {
        self.allowed_write_paths = Some(
            paths
                .into_iter()
                .filter_map(|path| normalize_relative_path(&path))
                .collect(),
        );
        self
    }

    pub(crate) fn audit_projection(&self) -> Value {
        let mut allowed_write_paths = self.allowed_write_paths.clone();
        if let Some(paths) = &mut allowed_write_paths {
            paths.sort();
            paths.dedup();
        }
        json!({
            "allowed_tools": self.allowed_tools.as_ref().map(|tools| tools.iter().cloned().collect::<Vec<_>>()),
            "allow_user_questions": self.allow_user_questions,
            "allowed_write_paths": allowed_write_paths,
        })
    }

    #[must_use]
    pub fn allows(&self, tool: &str) -> bool {
        if tool == "ask_user_question" && !self.allow_user_questions {
            return false;
        }
        self.allowed_tools
            .as_ref()
            .is_none_or(|allowed| allowed.contains(tool))
    }

    #[must_use]
    pub fn denial_reason(&self, tool: &str, input: &Value) -> Option<String> {
        if !self.allows(tool) {
            return Some(format!(
                "tool `{tool}` is denied by the role-bound execution policy"
            ));
        }
        let scopes = self.allowed_write_paths.as_ref()?;
        if tool == "symbol_rename" {
            return Some(
                "tool `symbol_rename` cannot prove its affected files are inside the delegated write scope; use guarded path-explicit edits instead"
                    .to_owned(),
            );
        }
        let paths = requested_write_paths(tool, input)?;
        let allowed = paths.iter().all(|path| {
            normalize_relative_path(path).is_some_and(|path| {
                scopes.iter().any(|scope| {
                    scope_allows(scope, &path)
                        || (tool == "fs_create_dir" && directory_leads_to_scope(scope, &path))
                })
            })
        });
        (!allowed).then(|| {
            format!(
                "tool `{tool}` requested an out-of-scope path; allowed write scopes are {scopes:?}"
            )
        })
    }
}

fn requested_write_paths<'a>(tool: &str, input: &'a Value) -> Option<Vec<&'a str>> {
    match tool {
        "fs_write" | "fs_create_dir" => Some(vec![input.get("path")?.as_str()?]),
        "patch_apply" => input
            .get("edits")?
            .as_array()?
            .iter()
            .map(|edit| edit.get("path")?.as_str())
            .collect(),
        _ => None,
    }
}

fn normalize_relative_path(path: &str) -> Option<String> {
    let path = path.replace('\\', "/");
    if path.starts_with('/') || path.as_bytes().get(1) == Some(&b':') {
        return None;
    }
    let mut segments = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => return None,
            segment => segments.push(segment),
        }
    }
    Some(if segments.is_empty() {
        ".".to_owned()
    } else {
        segments.join("/")
    })
}

fn scope_allows(scope: &str, path: &str) -> bool {
    matches!(scope, "." | "repository")
        || path == scope
        || path
            .strip_prefix(scope)
            .is_some_and(|remainder| remainder.starts_with('/'))
}

fn directory_leads_to_scope(scope: &str, path: &str) -> bool {
    matches!(scope, "." | "repository")
        || scope
            .strip_prefix(path)
            .is_some_and(|remainder| remainder.starts_with('/'))
}

impl Default for TeamRuntime {
    fn default() -> Self {
        Self {
            path: PathBuf::new(),
            repo: None,
            state: Arc::new(Mutex::new(DurableTeamState {
                team_id: String::new(),
                members: BTreeMap::new(),
                messages: Vec::new(),
                next_sequence: 1,
            })),
        }
    }
}

impl TeamRuntime {
    pub fn create(
        path: impl Into<PathBuf>,
        team_id: impl Into<String>,
        members: Vec<(String, TeamRole)>,
    ) -> Result<Self, String> {
        let path = path.into();
        let team_id = team_id.into();
        if team_id.trim().is_empty() || members.is_empty() {
            return Err("team identity and members are required".to_owned());
        }
        let mut indexed = BTreeMap::new();
        for (id, role) in members {
            if id.trim().is_empty() || indexed.contains_key(&id) {
                return Err("team member identifiers must be unique and non-empty".to_owned());
            }
            indexed.insert(
                id.clone(),
                TeamMember {
                    id,
                    role,
                    lifecycle: TeamMemberLifecycle::Starting,
                    current_task: None,
                    session_id: None,
                },
            );
        }
        let runtime = Self {
            repo: repository_from_team_path(&path),
            path,
            state: Arc::new(Mutex::new(DurableTeamState {
                team_id,
                members: indexed,
                messages: Vec::new(),
                next_sequence: 1,
            })),
        };
        validate_state(&*runtime.lock()?)?;
        runtime.register_repository()?;
        runtime.persist()?;
        Ok(runtime)
    }

    pub fn load(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();
        let mut state: DurableTeamState =
            serde_json::from_slice(&fs::read(&path).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        let migrated = migrate_legacy_messages(&mut state);
        validate_state(&state)?;
        let runtime = Self {
            repo: repository_from_team_path(&path),
            path,
            state: Arc::new(Mutex::new(state)),
        };
        runtime.register_repository()?;
        if migrated {
            runtime.persist()?;
        }
        Ok(runtime)
    }

    pub fn member_context(&self, member_id: &str) -> Result<TeamMemberContext, String> {
        if !self.lock()?.members.contains_key(member_id) {
            return Err(format!("unknown team member: {member_id}"));
        }
        Ok(TeamMemberContext {
            team: self.clone(),
            member_id: member_id.to_owned(),
        })
    }

    pub fn start_member(
        &self,
        member_id: &str,
        task_id: &str,
        session_id: &str,
    ) -> Result<(), String> {
        let mut state = self.lock()?;
        let member = state
            .members
            .get_mut(member_id)
            .ok_or_else(|| format!("unknown team member: {member_id}"))?;
        member.lifecycle = TeamMemberLifecycle::Running;
        member.current_task = Some(task_id.to_owned());
        member.session_id = Some(session_id.to_owned());
        drop(state);
        self.persist()
    }

    pub fn finish_member(&self, member_id: &str, failed: bool) -> Result<(), String> {
        let mut state = self.lock()?;
        let member = state
            .members
            .get_mut(member_id)
            .ok_or_else(|| format!("unknown team member: {member_id}"))?;
        member.lifecycle = if failed {
            TeamMemberLifecycle::Failed
        } else {
            TeamMemberLifecycle::Idle
        };
        member.current_task = None;
        drop(state);
        self.persist()
    }

    pub fn request_shutdown_all(&self) -> Result<(), String> {
        let mut state = self.lock()?;
        for member in state.members.values_mut() {
            if !matches!(
                member.lifecycle,
                TeamMemberLifecycle::Stopped | TeamMemberLifecycle::Failed
            ) {
                member.lifecycle = TeamMemberLifecycle::ShutdownRequested;
            }
        }
        drop(state);
        self.persist()
    }

    pub fn stop_all(&self) -> Result<(), String> {
        let mut state = self.lock()?;
        for member in state.members.values_mut() {
            if member.lifecycle != TeamMemberLifecycle::Failed {
                member.lifecycle = TeamMemberLifecycle::Stopped;
                member.current_task = None;
            }
        }
        drop(state);
        self.persist()
    }

    pub fn snapshot(&self) -> Result<Vec<TeamMember>, String> {
        Ok(self.lock()?.members.values().cloned().collect())
    }

    pub fn team_id(&self) -> Result<String, String> {
        Ok(self.lock()?.team_id.clone())
    }

    fn register_repository(&self) -> Result<(), String> {
        let Some(repo) = self.repo.as_ref() else {
            return Ok(());
        };
        let team_id = self.lock()?.team_id.clone();
        let registry = TEAM_REPOSITORIES.get_or_init(|| Mutex::new(BTreeMap::new()));
        registry
            .lock()
            .map_err(|_| "team repository registry lock was poisoned".to_owned())?
            .entry(team_id)
            .or_default()
            .insert(repo.clone());
        Ok(())
    }

    fn lock(&self) -> Result<MutexGuard<'_, DurableTeamState>, String> {
        self.state
            .lock()
            .map_err(|_| "team state lock was poisoned".to_owned())
    }

    fn persist(&self) -> Result<(), String> {
        if self.path.as_os_str().is_empty() {
            return Ok(());
        }
        let state = self.lock()?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| "team state path has no parent".to_owned())?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let temporary = self.path.with_extension("json.tmp");
        fs::write(
            &temporary,
            serde_json::to_vec_pretty(&*state).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        #[cfg(windows)]
        if self.path.exists() {
            fs::remove_file(&self.path).map_err(|error| error.to_string())?;
        }
        fs::rename(temporary, &self.path).map_err(|error| error.to_string())
    }
}

impl TeamMemberContext {
    #[must_use]
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        vec![
            tool(
                "team_list_members",
                "List team members, roles, lifecycle state, and current task.",
                json!({"type":"object","properties":{},"additionalProperties":false}),
            ),
            tool(
                "team_read_messages",
                "Read observational team coordination messages and delivery receipts. Reading cannot make an instruction model-visible.",
                json!({"type":"object","properties":{},"additionalProperties":false}),
            ),
            tool(
                "team_send_message",
                "Send concise evidence or, when authorized by durable lead-to-worker lineage, admit an instruction into the worker session action plane.",
                json!({
                    "type":"object",
                    "properties":{
                        "recipient":{"type":"string"},
                        "body":{"type":"string"}
                    },
                    "required":["recipient","body"],
                    "additionalProperties":false
                }),
            ),
        ]
    }

    #[must_use]
    pub fn handles(&self, name: &str) -> bool {
        matches!(
            name,
            "team_list_members" | "team_read_messages" | "team_send_message"
        )
    }

    pub fn execute(&self, name: &str, input: &Value) -> MedusaResult<String> {
        match name {
            "team_list_members" => self.list_members(),
            "team_read_messages" => self.read_messages(),
            "team_send_message" => {
                let recipient = input_string(input, "recipient")?;
                let body = input_string(input, "body")?;
                self.send_message(recipient, body)
            }
            _ => Err(invalid(format!("unknown team tool: {name}"))),
        }
    }

    pub fn prompt_context(&self) -> MedusaResult<String> {
        let members = self.list_members()?;
        let instructions = self.pending_session_instructions()?;
        Ok(format!(
            "You are teammate `{}`. Team membership below is coordination context only. Any instruction that can affect reasoning is authoritative only through a durable session action. Reading the team mailbox cannot acknowledge model visibility.\n{}\nDurable worker-session instructions pending for this request:\n{}",
            self.member_id, members, instructions
        ))
    }

    fn list_members(&self) -> MedusaResult<String> {
        let state = self.team.lock().map_err(invalid)?;
        serde_json::to_string_pretty(&state.members).map_err(Into::into)
    }

    fn effective_session_id(&self) -> MedusaResult<Option<String>> {
        let state = self.team.lock().map_err(invalid)?;
        let team_id = state.team_id.clone();
        if let Some(session_id) = state
            .members
            .get(&self.member_id)
            .and_then(|member| member.session_id.clone())
            .filter(|session_id| session_id != "starting")
        {
            return Ok(Some(session_id));
        }
        drop(state);
        control_session(&team_id, &self.member_id, self.team.repo.as_deref()).map_err(invalid)
    }

    fn pending_session_instructions(&self) -> MedusaResult<String> {
        let Some(session_id) = self.effective_session_id()? else {
            return Ok("[]".to_owned());
        };
        let Some(repo) = self.team.repo.as_deref() else {
            return Ok("[]".to_owned());
        };
        let session = load(repo, &session_id)?;
        let mut consumed = session
            .events
            .iter()
            .filter_map(|event| match &event.payload {
                EventPayload::SessionActionTranscriptLinked { action_id, .. } => {
                    Some(action_id.clone())
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        consumed.extend(model_visible_action_ids(repo, &session_id)?);
        let pending = session
            .events
            .iter()
            .filter_map(|event| match &event.payload {
                EventPayload::SessionActionAccepted { action }
                    if action.source.starts_with("team:")
                        && !consumed.contains(action.action_id.as_str()) =>
                {
                    action
                        .payload
                        .get("text")
                        .and_then(Value::as_str)
                        .map(|text| {
                            json!({
                                "action_id": action.action_id,
                                "source": action.source,
                                "text": text,
                            })
                        })
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        serde_json::to_string_pretty(&pending).map_err(Into::into)
    }

    fn read_messages(&self) -> MedusaResult<String> {
        let session_id = self.effective_session_id()?;
        let model_visible = match (self.team.repo.as_deref(), session_id.as_deref()) {
            (Some(repo), Some(session_id)) => model_visible_action_ids(repo, session_id)?,
            _ => BTreeSet::new(),
        };
        let mut state = self.team.lock().map_err(invalid)?;
        let mut visible = Vec::new();
        for message in &mut state.messages {
            if message.to != self.member_id {
                continue;
            }
            if message
                .action_id
                .as_ref()
                .is_some_and(|action_id| model_visible.contains(action_id))
            {
                message.delivery_state = TeamMessageDeliveryState::ModelVisible;
            }
            visible.push(message.clone());
            message.delivery_state = match message.delivery_state {
                TeamMessageDeliveryState::CoordinationOnly => {
                    TeamMessageDeliveryState::Acknowledged
                }
                TeamMessageDeliveryState::LegacyQueued => {
                    TeamMessageDeliveryState::LegacyAcknowledged
                }
                state => state,
            };
        }
        drop(state);
        self.team.persist().map_err(invalid)?;
        serde_json::to_string_pretty(&visible).map_err(Into::into)
    }

    fn send_message(&self, recipient: &str, body: &str) -> MedusaResult<String> {
        if body.trim().is_empty() {
            return Err(invalid("team message body cannot be empty"));
        }
        let mut state = self.team.lock().map_err(invalid)?;
        let sender =
            state.members.get(&self.member_id).cloned().ok_or_else(|| {
                invalid(format!("unknown team message sender: {}", self.member_id))
            })?;
        let recipient_member = state
            .members
            .get(recipient)
            .cloned()
            .ok_or_else(|| invalid(format!("unknown team message recipient: {recipient}")))?;
        let lead_count = state
            .members
            .values()
            .filter(|member| member.role == TeamRole::Lead)
            .count();
        let destination_session_id = recipient_member
            .session_id
            .clone()
            .filter(|session_id| session_id != "starting")
            .or_else(|| {
                control_session(&state.team_id, recipient, self.team.repo.as_deref())
                    .ok()
                    .flatten()
            });
        let instruction_candidate =
            sender.role == TeamRole::Lead && recipient_member.role != TeamRole::Lead;
        if instruction_candidate && lead_count != 1 {
            return Err(invalid(
                "team instruction authority is ambiguous because the durable team does not have exactly one lead",
            ));
        }
        if instruction_candidate
            && matches!(
                recipient_member.lifecycle,
                TeamMemberLifecycle::ShutdownRequested
                    | TeamMemberLifecycle::Stopped
                    | TeamMemberLifecycle::Failed
            )
        {
            return Err(invalid(
                "team instruction destination is terminal or shutting down",
            ));
        }
        if instruction_candidate && destination_session_id.is_none() {
            return Err(invalid(
                "team instruction destination has no durable worker session",
            ));
        }

        let sequence = state.next_sequence;
        state.next_sequence = state.next_sequence.saturating_add(1);
        let idempotency_key = format!("team:{}:{sequence}", state.team_id);
        let authorized_instruction = instruction_candidate;
        let mut message = DurableTeamMessage {
            sequence,
            idempotency_key: idempotency_key.clone(),
            action_id: None,
            from: self.member_id.clone(),
            to: recipient.to_owned(),
            body: body.to_owned(),
            destination_session_id: destination_session_id.clone(),
            delivery_state: if authorized_instruction {
                TeamMessageDeliveryState::Queued
            } else {
                TeamMessageDeliveryState::CoordinationOnly
            },
            delivered: false,
        };
        drop(state);

        if authorized_instruction {
            let repo = self.team.repo.as_deref().ok_or_else(|| {
                invalid("team instruction cannot resolve the repository session authority")
            })?;
            let session_id = destination_session_id.as_deref().ok_or_else(|| {
                invalid("team instruction destination has no durable worker session")
            })?;
            match admit_team_instruction(
                repo,
                session_id,
                &self.member_id,
                recipient,
                body,
                &idempotency_key,
            ) {
                Ok(action_id) => {
                    message.action_id = Some(action_id);
                    message.delivery_state = TeamMessageDeliveryState::ActionAccepted;
                }
                Err(error) => {
                    message.delivery_state = TeamMessageDeliveryState::Rejected;
                    self.store_message(message)?;
                    return Err(error);
                }
            }
        }
        let sequence = message.sequence;
        let state_name = message.delivery_state;
        self.store_message(message)?;
        Ok(format!(
            "message {sequence} accepted for {recipient} with state {state_name:?}"
        ))
    }

    fn store_message(&self, message: DurableTeamMessage) -> MedusaResult<()> {
        let mut state = self.team.lock().map_err(invalid)?;
        if state
            .messages
            .iter()
            .any(|existing| existing.sequence == message.sequence)
        {
            return Ok(());
        }
        state.messages.push(message);
        drop(state);
        self.team.persist().map_err(invalid)
    }
}

/// Binds a production worker's published session to its team member identity. This is a live
/// lookup cache only; the worker session itself remains the durable delivery authority.
pub fn bind_control_session(
    execution_id: &str,
    worker_id: &str,
    session_id: &str,
) -> Result<(), String> {
    if execution_id.trim().is_empty() || worker_id.trim().is_empty() || session_id.trim().is_empty()
    {
        return Err("team execution, worker, and session identities are required".to_owned());
    }
    let repo = repository_for_session(execution_id, session_id)?;
    let registry = TEAM_CONTROL_SESSIONS.get_or_init(|| Mutex::new(BTreeMap::new()));
    registry
        .lock()
        .map_err(|_| "team control session registry lock was poisoned".to_owned())?
        .insert(
            (execution_id.to_owned(), worker_id.to_owned(), repo),
            session_id.to_owned(),
        );
    Ok(())
}

fn control_session(
    execution_id: &str,
    worker_id: &str,
    repo: Option<&Path>,
) -> Result<Option<String>, String> {
    let registry = TEAM_CONTROL_SESSIONS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let registry = registry
        .lock()
        .map_err(|_| "team control session registry lock was poisoned".to_owned())?;
    if let Some(repo) = repo {
        return Ok(registry
            .get(&(
                execution_id.to_owned(),
                worker_id.to_owned(),
                repo.to_path_buf(),
            ))
            .cloned());
    }
    let mut matches = registry
        .iter()
        .filter(|((candidate_execution, candidate_worker, _), _)| {
            candidate_execution == execution_id && candidate_worker == worker_id
        })
        .map(|(_, session_id)| session_id.clone());
    let first = matches.next();
    if matches.next().is_some() {
        return Err(format!(
            "team execution `{execution_id}` has ambiguous worker session bindings for `{worker_id}`"
        ));
    }
    Ok(first)
}

fn repository_for_session(execution_id: &str, session_id: &str) -> Result<PathBuf, String> {
    let registry = TEAM_REPOSITORIES.get_or_init(|| Mutex::new(BTreeMap::new()));
    let candidates = registry
        .lock()
        .map_err(|_| "team repository registry lock was poisoned".to_owned())?
        .get(execution_id)
        .cloned()
        .ok_or_else(|| {
            format!("team execution `{execution_id}` has no durable repository binding")
        })?;
    let mut matches = candidates
        .into_iter()
        .filter(|repo| load(repo, session_id).is_ok());
    let first = matches.next().ok_or_else(|| {
        format!(
            "team execution `{execution_id}` has no durable repository containing session `{session_id}`"
        )
    })?;
    if matches.next().is_some() {
        return Err(format!(
            "team execution `{execution_id}` has ambiguous repository bindings for session `{session_id}`"
        ));
    }
    Ok(first)
}

/// Admits one runtime team-control instruction through the canonical worker session action plane.
pub fn admit_control_instruction(
    execution_id: &str,
    session_id: &str,
    recipient: &str,
    body: &str,
    idempotency_key: &str,
) -> Result<String, String> {
    let repo = repository_for_session(execution_id, session_id)?;
    admit_team_instruction(&repo, session_id, "lead", recipient, body, idempotency_key)
        .map_err(|error| error.to_string())
}

pub fn admit_team_instruction(
    repo: &Path,
    session_id: &str,
    sender: &str,
    recipient: &str,
    body: &str,
    idempotency_key: &str,
) -> MedusaResult<String> {
    let mut session = load(repo, session_id)?;
    let action_id = deterministic_action_id(session_id, idempotency_key);
    let source = format!("team:{sender}:{recipient}");
    let payload = json!({
        "text": body,
        "team": {
            "sender": sender,
            "recipient": recipient,
        }
    });
    if let Some(existing) = session
        .events
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::SessionActionAccepted { action }
                if action.idempotency_key == idempotency_key =>
            {
                Some(action)
            }
            _ => None,
        })
    {
        let same_instruction = existing.action_id == action_id
            && existing.source == source
            && existing.target_session_id == session_id
            && existing.kind == SessionActionKind::Steer
            && existing.delivery_policy == SessionActionDeliveryPolicy::NextSafeTurnBoundary
            && existing.wake_policy == SessionActionWakePolicy::OnBoundary
            && existing.payload == payload;
        if same_instruction {
            return Ok(existing.action_id.clone());
        }
        return Err(invalid(
            "team instruction idempotency key was reused for a different session action",
        ));
    }
    let action = SessionAction {
        action_id: action_id.clone(),
        idempotency_key: idempotency_key.to_owned(),
        source,
        target_session_id: session_id.to_owned(),
        expected_session_revision: session.events.last().map_or(0, |event| event.sequence),
        kind: SessionActionKind::Steer,
        delivery_policy: SessionActionDeliveryPolicy::NextSafeTurnBoundary,
        wake_policy: SessionActionWakePolicy::OnBoundary,
        payload,
    };
    append_event(
        &mut session,
        Actor::Worker(sender.to_owned()),
        EventPayload::SessionActionAccepted { action },
    )?;
    persist(&session)?;
    let persisted = load(repo, session_id)?;
    match persisted
        .events
        .iter()
        .rev()
        .find_map(|event| match &event.payload {
            EventPayload::SessionActionAccepted { action } if action.action_id == action_id => {
                Some(true)
            }
            EventPayload::SessionActionRejected { action, .. } if action.action_id == action_id => {
                Some(false)
            }
            _ => None,
        }) {
        Some(true) => Ok(action_id),
        Some(false) => Err(invalid(
            "team instruction was rejected because the destination session revision changed",
        )),
        None => Err(invalid(
            "team instruction admission was not present after durable persistence",
        )),
    }
}

fn deterministic_action_id(session_id: &str, idempotency_key: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(session_id.as_bytes());
    digest.update([0]);
    digest.update(idempotency_key.as_bytes());
    format!("action-{:x}", digest.finalize())
}

fn model_visible_action_ids(repo: &Path, session_id: &str) -> MedusaResult<BTreeSet<String>> {
    let roots = [
        repo.join(".medusa")
            .join("request-manifests")
            .join(session_id),
        crate::session::fallback_storage_root(repo, "request-manifests").join(session_id),
    ];
    let mut visible = BTreeSet::new();
    for root in roots {
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = entry?;
            if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let fingerprint = entry
                .path()
                .file_stem()
                .and_then(|value| value.to_str())
                .ok_or_else(|| invalid("request manifest file name is not valid UTF-8"))?
                .to_owned();
            let manifest_ref = format!("request-manifest:sha256:{fingerprint}");
            let inspected =
                crate::engine::effective_request::inspect(repo, session_id, &manifest_ref)?;
            for action_id in inspected
                .get("delivered_action_ids")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                visible.insert(action_id.to_owned());
            }
        }
    }
    Ok(visible)
}

fn migrate_legacy_messages(state: &mut DurableTeamState) -> bool {
    let mut changed = false;
    for message in &mut state.messages {
        if message.idempotency_key.is_empty() {
            message.delivery_state = if message.delivered {
                TeamMessageDeliveryState::LegacyAcknowledged
            } else {
                TeamMessageDeliveryState::LegacyQueued
            };
            message.idempotency_key = format!("legacy:{}:{}", state.team_id, message.sequence);
            message.delivered = false;
            changed = true;
        }
    }
    changed
}

fn repository_from_team_path(path: &Path) -> Option<PathBuf> {
    let mut current = path.parent();
    while let Some(directory) = current {
        if directory.file_name().is_some_and(|name| name == ".medusa") {
            return directory.parent().map(Path::to_path_buf);
        }
        current = directory.parent();
    }
    None
}

fn validate_state(state: &DurableTeamState) -> Result<(), String> {
    if state.team_id.trim().is_empty() || state.members.is_empty() {
        return Err("team identity and members are required".to_owned());
    }
    for (id, member) in &state.members {
        if id.trim().is_empty() || member.id != *id {
            return Err("team member map keys must match non-empty member identifiers".to_owned());
        }
    }
    let mut previous = 0;
    for message in &state.messages {
        if message.sequence <= previous
            || message.idempotency_key.trim().is_empty()
            || message.body.trim().is_empty()
            || !state.members.contains_key(&message.from)
            || !state.members.contains_key(&message.to)
        {
            return Err("team mailbox contains invalid or out-of-order messages".to_owned());
        }
        previous = message.sequence;
    }
    if state.next_sequence <= previous {
        return Err("team mailbox sequence is stale".to_owned());
    }
    Ok(())
}

fn tool(name: &str, description: &str, input_schema: Value) -> ToolDefinition {
    ToolDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema,
    }
}

fn input_string<'a>(input: &'a Value, key: &str) -> MedusaResult<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| invalid(format!("{key} must be a non-empty string")))
}

fn invalid(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InvalidConfiguration,
        ErrorCategory::Validation,
        message,
    )
}

#[cfg(test)]
mod tests {
    use medusa_provider::{MessageBlock, ModelProvider, ModelRequest, ModelResponse, Usage};

    use super::*;
    use crate::AgentEngine;
    use medusa_config::Config;

    struct NoopProvider;

    impl ModelProvider for NoopProvider {
        fn complete(&self, _request: &ModelRequest) -> MedusaResult<ModelResponse> {
            Ok(ModelResponse {
                response_id: Some("noop".to_owned()),
                stop_reason: Some("stop".to_owned()),
                blocks: Vec::new(),
                usage: Usage::default(),
            })
        }
    }

    fn team_with_worker(
        directory: &tempfile::TempDir,
        team_id: &str,
        session_id: &str,
    ) -> (TeamRuntime, TeamMemberContext, TeamMemberContext) {
        let team = TeamRuntime::create(
            directory
                .path()
                .join(format!(".medusa/executions/{team_id}/team.json")),
            team_id,
            vec![
                ("lead".to_owned(), TeamRole::Lead),
                ("reviewer".to_owned(), TeamRole::Reviewer),
            ],
        )
        .expect("team");
        team.start_member("reviewer", "review-1", session_id)
            .expect("start reviewer");
        let lead = team.member_context("lead").expect("lead");
        let reviewer = team.member_context("reviewer").expect("reviewer");
        (team, lead, reviewer)
    }

    #[test]
    fn lead_instruction_is_admitted_to_destination_session() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let engine = AgentEngine::new(NoopProvider, Config::default());
        let session = engine
            .create_session(directory.path(), "review".to_owned())
            .expect("session");
        let (team, lead, _) = team_with_worker(&directory, "team-lead", session.id.as_str());
        lead.send_message("reviewer", "check the transaction boundary")
            .expect("send");

        let restored = engine
            .load_session(directory.path(), session.id.as_str())
            .expect("restore session");
        let accepted = restored
            .events
            .iter()
            .find_map(|event| match &event.payload {
                EventPayload::SessionActionAccepted { action } => Some(action),
                _ => None,
            });
        assert!(accepted.is_some_and(|action| {
            action.source == "team:lead:reviewer"
                && action.payload["text"] == json!("check the transaction boundary")
        }));
        let serialized = fs::read_to_string(&team.path).expect("team state");
        assert!(!serialized.contains("\"delivered\""));
        assert!(serialized.contains("action_accepted"));
    }

    #[test]
    fn control_instruction_uses_registered_team_repository_and_session_binding() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let engine = AgentEngine::new(NoopProvider, Config::default());
        let session = engine
            .create_session(directory.path(), "review".to_owned())
            .expect("session");
        let team = TeamRuntime::create(
            directory
                .path()
                .join(".medusa/executions/control-team/team.json"),
            "control-team",
            vec![
                ("lead".to_owned(), TeamRole::Lead),
                ("reviewer".to_owned(), TeamRole::Reviewer),
            ],
        )
        .expect("team");
        let reviewer = team.member_context("reviewer").expect("reviewer");
        bind_control_session("control-team", "reviewer", session.id.as_str()).expect("bind");
        let action_id = admit_control_instruction(
            "control-team",
            session.id.as_str(),
            "reviewer",
            "check durable steering",
            "team-control:control-team:reviewer:1",
        )
        .expect("admit control instruction");
        let context = reviewer.prompt_context().expect("prompt context");
        assert!(context.contains(&action_id));
        assert!(context.contains("check durable steering"));
    }

    #[test]
    fn mailbox_read_cannot_claim_model_visibility() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let engine = AgentEngine::new(NoopProvider, Config::default());
        let session = engine
            .create_session(directory.path(), "review".to_owned())
            .expect("session");
        let (_, lead, reviewer) = team_with_worker(&directory, "team-mailbox", session.id.as_str());
        lead.send_message("reviewer", "inspect the boundary")
            .expect("send");
        let first = reviewer.read_messages().expect("read");
        assert!(first.contains("action_accepted"));
        let restored = engine
            .load_session(directory.path(), session.id.as_str())
            .expect("restore session");
        assert!(!restored.events.iter().any(|event| matches!(
            event.payload,
            EventPayload::SessionActionTranscriptLinked { .. }
        )));
    }

    #[test]
    fn peer_message_is_observational_and_not_session_input() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let engine = AgentEngine::new(NoopProvider, Config::default());
        let session = engine
            .create_session(directory.path(), "review".to_owned())
            .expect("session");
        let team = TeamRuntime::create(
            directory
                .path()
                .join(".medusa/executions/team-peer/team.json"),
            "team-peer",
            vec![
                ("researcher".to_owned(), TeamRole::Researcher),
                ("reviewer".to_owned(), TeamRole::Reviewer),
            ],
        )
        .expect("team");
        team.start_member("reviewer", "review-1", session.id.as_str())
            .expect("start reviewer");
        team.member_context("researcher")
            .expect("researcher")
            .send_message("reviewer", "FYI only")
            .expect("send");
        assert!(
            team.member_context("reviewer")
                .expect("reviewer")
                .read_messages()
                .expect("read")
                .contains("coordination_only")
        );
        let restored = engine
            .load_session(directory.path(), session.id.as_str())
            .expect("restore session");
        assert!(
            !restored
                .events
                .iter()
                .any(|event| matches!(event.payload, EventPayload::SessionActionAccepted { .. }))
        );
    }

    #[test]
    fn legacy_delivered_boolean_migrates_without_claiming_model_visibility() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory
            .path()
            .join(".medusa/executions/legacy-team/team.json");
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(
            &path,
            serde_json::to_vec_pretty(&json!({
                "team_id": "legacy-team",
                "members": {
                    "lead": {"id":"lead","role":"lead","lifecycle":"running","current_task":null,"session_id":null},
                    "reviewer": {"id":"reviewer","role":"reviewer","lifecycle":"idle","current_task":null,"session_id":null}
                },
                "messages": [{
                    "sequence": 1,
                    "from": "lead",
                    "to": "reviewer",
                    "body": "legacy",
                    "delivered": true
                }],
                "next_sequence": 2
            }))
            .expect("json"),
        )
        .expect("legacy state");
        let team = TeamRuntime::load(&path).expect("load");
        assert!(
            team.member_context("reviewer")
                .expect("reviewer")
                .read_messages()
                .expect("read")
                .contains("legacy_acknowledged")
        );
        let migrated = fs::read_to_string(path).expect("migrated state");
        assert!(!migrated.contains("\"delivered\""));
        assert!(migrated.contains("legacy_acknowledged"));
    }

    #[test]
    fn role_policy_denies_mutation_for_reviewers() {
        let policy = AgentExecutionPolicy::for_team_role(TeamRole::Reviewer);
        assert!(policy.allows("fs_read"));
        assert!(policy.allows("team_send_message"));
        assert!(!policy.allows("fs_write"));
        assert!(!policy.allows("shell_run"));
        assert!(!policy.allows("ask_user_question"));
    }

    #[test]
    fn scoped_policy_rejects_out_of_contract_file_operations() {
        let policy = AgentExecutionPolicy::for_team_role(TeamRole::Implementer)
            .with_allowed_write_paths(vec!["src/slugify.py".to_owned()]);

        assert!(
            policy
                .denial_reason(
                    "fs_write",
                    &json!({"path":"src/slugify.py","content":"fixed"})
                )
                .is_none()
        );
        assert!(
            policy
                .denial_reason("fs_create_dir", &json!({"path":"src"}))
                .is_none()
        );
        assert!(
            policy
                .denial_reason(
                    "patch_apply",
                    &json!({"edits":[{
                        "path":"src/slugify.py",
                        "start_byte":0,
                        "end_byte":0,
                        "expected":"",
                        "replacement":"fixed"
                    }]})
                )
                .is_none()
        );

        for path in ["src/__init__.py", "../slugify.py", "/tmp/slugify.py"] {
            assert!(
                policy
                    .denial_reason("fs_write", &json!({"path":path,"content":"blocked"}))
                    .is_some(),
                "{path} must be denied"
            );
        }
        assert!(
            policy
                .denial_reason("fs_create_dir", &json!({"path":"tests"}))
                .is_some()
        );
        assert!(
            policy
                .denial_reason(
                    "patch_apply",
                    &json!({"edits":[{
                        "path":"src/__init__.py",
                        "start_byte":0,
                        "end_byte":0,
                        "expected":"",
                        "replacement":"blocked"
                    }]})
                )
                .is_some()
        );
        assert!(
            policy
                .denial_reason(
                    "symbol_rename",
                    &json!({"old_name":"before","new_name":"after"})
                )
                .is_some()
        );
    }

    #[test]
    fn scoped_policy_allows_directory_scopes_and_unrestricted_reads() {
        let policy = AgentExecutionPolicy::for_team_role(TeamRole::Implementer)
            .with_allowed_write_paths(vec!["src/".to_owned()]);

        assert!(
            policy
                .denial_reason("fs_read", &json!({"path":"README.md"}))
                .is_none()
        );
        assert!(
            policy
                .denial_reason(
                    "fs_write",
                    &json!({"path":"src/nested/file.rs","content":""})
                )
                .is_none()
        );
        assert!(
            policy
                .denial_reason("fs_write", &json!({"path":"tests/file.rs","content":""}))
                .is_some()
        );
    }

    #[test]
    fn pending_session_instruction_is_visible_in_team_prompt_context() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let engine = AgentEngine::new(NoopProvider, Config::default());
        let session = engine
            .create_session_with_content(
                directory.path(),
                "review".to_owned(),
                vec![MessageBlock::Text {
                    text: "review".to_owned(),
                }],
            )
            .expect("session");
        let (_, lead, reviewer) = team_with_worker(&directory, "team-prompt", session.id.as_str());
        lead.send_message("reviewer", "inspect canonical delivery")
            .expect("send");
        let context = reviewer.prompt_context().expect("context");
        assert!(context.contains("inspect canonical delivery"));
        assert!(context.contains("action_id"));
    }
}
