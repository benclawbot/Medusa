//! Shared team coordination tools for independent agent sessions.
//!
//! The durable scheduler and lease controller remain authoritative for task state. This
//! module owns only team membership, lifecycle, and mailbox state, and exposes that state
//! to role-bound `AgentEngine` instances through deterministic tools.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use medusa_provider::ToolDefinition;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TeamMessage {
    pub sequence: u64,
    pub from: String,
    pub to: String,
    pub body: String,
    pub delivered: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct DurableTeamState {
    team_id: String,
    members: BTreeMap<String, TeamMember>,
    messages: Vec<TeamMessage>,
    next_sequence: u64,
}

#[derive(Clone)]
pub struct TeamRuntime {
    path: PathBuf,
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
    #[must_use]
    pub fn unrestricted() -> Self {
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
            path,
            state: Arc::new(Mutex::new(DurableTeamState {
                team_id,
                members: indexed,
                messages: Vec::new(),
                next_sequence: 1,
            })),
        };
        validate_state(&*runtime.lock()?)?;
        runtime.persist()?;
        Ok(runtime)
    }

    pub fn load(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();
        let state: DurableTeamState =
            serde_json::from_slice(&fs::read(&path).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        validate_state(&state)?;
        Ok(Self {
            path,
            state: Arc::new(Mutex::new(state)),
        })
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
                "Read messages delivered to this teammate. Messages are marked delivered.",
                json!({"type":"object","properties":{},"additionalProperties":false}),
            ),
            tool(
                "team_send_message",
                "Send a concise evidence-backed message to another teammate or to `lead`.",
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
        let messages = self.peek_messages()?;
        Ok(format!(
            "You are teammate `{}`. Team coordination is authoritative through the team tools.\n{}\nUnread messages:\n{}",
            self.member_id, members, messages
        ))
    }

    fn list_members(&self) -> MedusaResult<String> {
        let state = self.team.lock().map_err(invalid)?;
        serde_json::to_string_pretty(&state.members).map_err(Into::into)
    }

    fn peek_messages(&self) -> MedusaResult<String> {
        let state = self.team.lock().map_err(invalid)?;
        let messages = state
            .messages
            .iter()
            .filter(|message| message.to == self.member_id && !message.delivered)
            .cloned()
            .collect::<Vec<_>>();
        serde_json::to_string_pretty(&messages).map_err(Into::into)
    }

    fn read_messages(&self) -> MedusaResult<String> {
        let mut state = self.team.lock().map_err(invalid)?;
        let mut delivered = Vec::new();
        for message in &mut state.messages {
            if message.to == self.member_id && !message.delivered {
                message.delivered = true;
                delivered.push(message.clone());
            }
        }
        drop(state);
        self.team.persist().map_err(invalid)?;
        serde_json::to_string_pretty(&delivered).map_err(Into::into)
    }

    fn send_message(&self, recipient: &str, body: &str) -> MedusaResult<String> {
        if body.trim().is_empty() {
            return Err(invalid("team message body cannot be empty"));
        }
        let mut state = self.team.lock().map_err(invalid)?;
        if !state.members.contains_key(recipient) {
            return Err(invalid(format!(
                "unknown team message recipient: {recipient}"
            )));
        }
        let sequence = state.next_sequence;
        state.next_sequence = state.next_sequence.saturating_add(1);
        state.messages.push(TeamMessage {
            sequence,
            from: self.member_id.clone(),
            to: recipient.to_owned(),
            body: body.to_owned(),
            delivered: false,
        });
        drop(state);
        self.team.persist().map_err(invalid)?;
        Ok(format!("message {sequence} delivered to {recipient}"))
    }
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
    use super::*;

    #[test]
    fn direct_messages_are_delivered_once() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let team = TeamRuntime::create(
            directory.path().join("team.json"),
            "team-1",
            vec![
                ("lead".to_owned(), TeamRole::Lead),
                ("reviewer".to_owned(), TeamRole::Reviewer),
            ],
        )
        .expect("team");
        let lead = team.member_context("lead").expect("lead");
        let reviewer = team.member_context("reviewer").expect("reviewer");
        lead.send_message("reviewer", "check the transaction boundary")
            .expect("send");
        assert!(
            reviewer
                .read_messages()
                .expect("read")
                .contains("transaction boundary")
        );
        assert_eq!(reviewer.read_messages().expect("second read"), "[]");
    }

    #[test]
    fn messages_and_member_state_survive_restart() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("team.json");
        let team = TeamRuntime::create(
            &path,
            "team-1",
            vec![
                ("lead".to_owned(), TeamRole::Lead),
                ("reviewer".to_owned(), TeamRole::Reviewer),
            ],
        )
        .expect("team");
        team.start_member("reviewer", "review-1", "session-1")
            .expect("start reviewer");
        team.member_context("lead")
            .expect("lead")
            .send_message("reviewer", "review the coordinator boundary")
            .expect("send");

        let restored = TeamRuntime::load(path).expect("restore team");
        assert_eq!(restored.team_id().expect("team id"), "team-1");
        let reviewer = restored
            .snapshot()
            .expect("members")
            .into_iter()
            .find(|member| member.id == "reviewer")
            .expect("reviewer");
        assert_eq!(reviewer.lifecycle, TeamMemberLifecycle::Running);
        assert_eq!(reviewer.current_task.as_deref(), Some("review-1"));
        assert!(
            restored
                .member_context("reviewer")
                .expect("reviewer context")
                .read_messages()
                .expect("messages")
                .contains("coordinator boundary")
        );
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
}
