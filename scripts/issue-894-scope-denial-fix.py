from pathlib import Path

path = Path("crates/medusa-agent/src/engine.rs")
text = path.read_text()

old = '''                ResponseBlock::ToolUse { id, name, input } => {
                    if scoped_tool_names.binary_search(&name).is_err() {
                        return Err(MedusaError::new(
                            ErrorCode::PolicyDenied,
                            ErrorCategory::Policy,
                            format!("tool {name} is revoked or outside the active agent scope"),
                        ));
                    }
                    if let Some(early) = early_tool_executions.get(&id)
'''
new = '''                ResponseBlock::ToolUse { id, name, input } => {
                    if let Some(early) = early_tool_executions.get(&id)
'''
if old not in text:
    raise SystemExit("response scope-denial anchor not found")
text = text.replace(old, new, 1)

old = '''                    name == ANALYSIS_WORKSPACE_TOOL
                        || name == "update_plan"
                        || name == "ask_user_question"
                        || name == "desktop_commander"
                        || self
'''
new = '''                    scoped_tool_names.binary_search(name).is_err()
                        || name == ANALYSIS_WORKSPACE_TOOL
                        || name == "update_plan"
                        || name == "ask_user_question"
                        || name == "desktop_commander"
                        || self
'''
if old not in text:
    raise SystemExit("serial dispatch anchor not found")
text = text.replace(old, new, 1)

old = '''                let execution = if let Some(early) = early_tool_executions.remove(&id) {
                    if early.name != name || early.input != input {
'''
new = '''                let execution = if scoped_tool_names.binary_search(&name).is_err() {
                    measured = true;
                    append_observed(
                        session,
                        EventPayload::ToolExecutionStarted {
                            tool: audited_tool_name(&name, &input),
                        },
                        &mut observer,
                    )?;
                    execute_engine_tool_with_policy(
                        &name,
                        &input,
                        self.cancellation.as_ref(),
                        &self.execution_policy,
                        |_| {
                            Err(MedusaError::new(
                                ErrorCode::PolicyDenied,
                                ErrorCategory::Policy,
                                format!(
                                    "tool {name} is revoked or outside the active agent scope"
                                ),
                            ))
                        },
                    )?
                } else if let Some(early) = early_tool_executions.remove(&id) {
                    if early.name != name || early.input != input {
'''
if old not in text:
    raise SystemExit("single execution anchor not found")
text = text.replace(old, new, 1)

path.write_text(text)
