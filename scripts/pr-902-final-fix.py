from pathlib import Path

engine_path = Path("crates/medusa-agent/src/engine.rs")
engine = engine_path.read_text()
start_marker = """                if let Err(error) = &result
                    && error.code == ErrorCode::PolicyDenied
                    && self.config.agent.mode != Mode::ReadOnly
                    && self.execution_policy.denial_reason(&name, &input).is_none()
                    && interactively_approvable(&name, &input)
                {
"""
end_marker = """                let event_tool = audited_tool_name(&name, &input);
"""
start = engine.index(start_marker)
end = engine.index(end_marker, start)
old = engine[start:end]
approval_body_start = old.index("                    let action = approval_action_label")
approval_body = old[approval_body_start:]
approval_body = approval_body.rsplit("                }\n", 1)[0]
new = """                let awaiting_approval = result.as_ref().err().is_some_and(|error| {
                    error.code == ErrorCode::PolicyDenied
                        && self.config.agent.mode != Mode::ReadOnly
                        && self.execution_policy.denial_reason(&name, &input).is_none()
                        && interactively_approvable(&name, &input)
                });
                if let Err(error) = &result
                    && error.code == ErrorCode::PolicyDenied
                {
                    append_observed(
                        session,
                        EventPayload::ToolCallDenied {
                            tool: audited_tool_name(&name, &input),
                            reason: if awaiting_approval {
                                \"tool requires explicit user approval\".to_owned()
                            } else {
                                error.to_string()
                            },
                        },
                        &mut observer,
                    )?;
                }
                if awaiting_approval {
""" + approval_body + """
                }
"""
engine_path.write_text(engine[:start] + new + engine[end:])

tools_path = Path("crates/medusa-agent/src/tools/mod.rs")
tools = tools_path.read_text()
old_test = """        let error = execute_tool_cancellable_with_policy_certified(
            directory.path(),
            \"fs_write\",
            &json!({\"path\":\"denied.txt\",\"content\":\"no\"}),
            &AtomicBool::new(false),
            &policy,
        )
        .err()
        .expect(\"researcher write must be denied by certified pipeline\");"""
new_test = """        let error = execute_tool_cancellable_with_policy_certified(
            directory.path(),
            \"fs_write\",
            &json!({\"path\":\"denied.txt\",\"content\":\"no\"}),
            &AtomicBool::new(false),
            &policy,
        )
        .expect(\"certified execution\")
        .result
        .expect_err(\"researcher write must be denied by certified pipeline\");"""
assert tools.count(old_test) == 1
tools_path.write_text(tools.replace(old_test, new_test))
