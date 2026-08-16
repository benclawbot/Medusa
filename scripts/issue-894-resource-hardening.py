from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    target = Path(path)
    text = target.read_text()
    if old not in text:
        raise SystemExit(f"missing anchor in {target}: {old[:160]!r}")
    target.write_text(text.replace(old, new, 1))


# Fix preparation resource seeding after authority fields are moved into the immutable contract.
path = Path("crates/medusa-agent/src/agent_scope.rs")
text = path.read_text()
old = '''                owned_resources: initial_resources(
                    team_id.as_deref(),
                    member_id.as_deref(),
                    analysis_workspace,
                ),'''
new = '''                owned_resources: initial_resources(
                    contract.team_id.as_deref(),
                    contract.member_id.as_deref(),
                    contract.analysis_workspace,
                ),'''
if old not in text:
    raise SystemExit("initial resource ownership anchor missing")
path.write_text(text.replace(old, new, 1))

# Desktop Commander is dynamically registered under the session scope when the owned client is
# first created, and released if setup fails. Normal/cancelled worker stop drops the client then
# terminal scope teardown revokes the registration.
path = Path("crates/medusa-agent/src/engine.rs")
text = path.read_text()
old = '''    fn execute_desktop_commander(
        &self,
        repo: &Path,
        input: &serde_json::Value,
    ) -> MedusaResult<String> {'''
new = '''    fn execute_desktop_commander(
        &self,
        repo: &Path,
        session_id: &str,
        input: &serde_json::Value,
    ) -> MedusaResult<String> {'''
if old not in text:
    raise SystemExit("desktop commander signature anchor missing")
text = text.replace(old, new, 1)
old = '''        if client.is_none() {
            *client = Some(DesktopCommanderClient::connect(
                repo,
                self.desktop_commander_settings.clone(),
            )?);
        }'''
new = '''        if client.is_none() {
            let scope = load_published_scope_ref(repo, session_id)?;
            register_agent_scope_resource(
                repo,
                session_id,
                &scope,
                "desktop-commander",
                AgentScopeResourceKind::DesktopCommander,
            )?;
            match DesktopCommanderClient::connect(repo, self.desktop_commander_settings.clone()) {
                Ok(connected) => *client = Some(connected),
                Err(error) => {
                    let _ = release_agent_scope_resource(
                        repo,
                        session_id,
                        &scope,
                        "desktop-commander",
                    );
                    return Err(error);
                }
            }
        }'''
if old not in text:
    raise SystemExit("desktop commander owned registration anchor missing")
text = text.replace(old, new, 1)
old = '''                            self.execute_desktop_commander(&session.repo, canonical_input)'''
new = '''                            self.execute_desktop_commander(
                                &session.repo,
                                session.id.as_str(),
                                canonical_input,
                            )'''
if old not in text:
    raise SystemExit("desktop commander scoped call anchor missing")
text = text.replace(old, new, 1)

# A previously approved action still cannot execute after its capability is dynamically revoked.
old = '''            let (content, is_error) = if decision == ApprovalDecision::Approved {
                let event_tool = audited_tool_name(&approval.tool, &approval.input);'''
new = '''            let (content, is_error) = if decision == ApprovalDecision::Approved {
                let scoped_tools = self.scoped_runtime_tools(session)?;
                if scoped_tools.binary_search(&approval.tool).is_err() {
                    return Err(MedusaError::new(
                        ErrorCode::PolicyDenied,
                        ErrorCategory::Policy,
                        format!(
                            "approved tool {} was revoked from the active agent scope before execution",
                            approval.tool
                        ),
                    ));
                }
                let event_tool = audited_tool_name(&approval.tool, &approval.input);'''
if old not in text:
    raise SystemExit("approved action scope revocation anchor missing")
text = text.replace(old, new, 1)
path.write_text(text)
