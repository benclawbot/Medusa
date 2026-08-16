from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    target = Path(path)
    text = target.read_text()
    if old not in text:
        raise SystemExit(f"missing anchor in {target}: {old[:160]!r}")
    target.write_text(text.replace(old, new, 1))


# A worker may share the parent cancellation source. Finishing one worker must not cancel unrelated
# sibling scopes; stop_session_scope is invoked only after the worker activity has already drained.
path = Path("crates/medusa-agent/src/engine.rs")
text = path.read_text()
old = '''    pub fn stop_session_scope(
        &self,
        session: &AgentSession,
        cause: impl Into<String>,
    ) -> MedusaResult<crate::agent_scope::AgentScopeStopReceipt> {
        self.cancellation
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Ok(mut client) = self.desktop_commander.lock() {
            client.take();
        }
        stop_agent_scope(&session.repo, session.id.as_str(), cause)
    }'''
new = '''    pub fn stop_session_scope(
        &self,
        session: &AgentSession,
        cause: impl Into<String>,
    ) -> MedusaResult<crate::agent_scope::AgentScopeStopReceipt> {
        if let Ok(mut client) = self.desktop_commander.lock() {
            client.take();
        }
        stop_agent_scope(&session.repo, session.id.as_str(), cause)
    }'''
if old not in text:
    raise SystemExit("engine stop-session cancellation anchor missing")
path.write_text(text.replace(old, new, 1))

# Read-only production worker: always close the live scope after the bounded execution is done,
# regardless of success/cancellation/model/tool failure. The scheduler evidence survives separately.
path = Path("crates/medusa-runtime/src/multi_agent_coordinator.rs")
text = path.read_text()
old = '''    request
        .team_context
        .clone()
        .execute('''
new = '''    let result = (|| -> Result<WorkerEvidence, String> {
    request
        .team_context
        .clone()
        .execute('''
if old not in text:
    raise SystemExit("read-only worker execution closure start missing")
text = text.replace(old, new, 1)
old = '''    Ok(WorkerEvidence {
        task_id: request.contract.task_id,
        worker_id: request.worker_id,
        role: request.contract.role,
        context_fingerprint: request.packet.fingerprint,
        lease_epoch: 0,
        delegation_contract_id: request.delegation.contract_id,
        delegation_contract_fingerprint: request.delegation.fingerprint,
        delegation_attempt_fingerprint: request.attempt.fingerprint,
        session_id: session.id.to_string(),
        turns: session.turn,
        summary,
    })
}'''
new = '''    Ok(WorkerEvidence {
        task_id: request.contract.task_id,
        worker_id: request.worker_id,
        role: request.contract.role,
        context_fingerprint: request.packet.fingerprint,
        lease_epoch: 0,
        delegation_contract_id: request.delegation.contract_id,
        delegation_contract_fingerprint: request.delegation.fingerprint,
        delegation_attempt_fingerprint: request.attempt.fingerprint,
        session_id: session.id.to_string(),
        turns: session.turn,
        summary,
    })
    })();
    let stop_cause = if result.is_ok() {
        "read-only worker completed"
    } else {
        "read-only worker stopped after execution failure"
    };
    let stop = engine
        .stop_session_scope(&session, stop_cause)
        .map_err(|error| error.to_string());
    match (result, stop) {
        (Ok(evidence), Ok(_)) => Ok(evidence),
        (Err(error), Ok(_)) => Err(error),
        (Ok(_), Err(stop_error)) => Err(format!("worker scope teardown failed: {stop_error}")),
        (Err(error), Err(stop_error)) => Err(format!(
            "{error}; worker scope teardown also failed: {stop_error}"
        )),
    }
}'''
if old not in text:
    raise SystemExit("read-only worker execution closure end missing")
path.write_text(text.replace(old, new, 1))

# Mutating implementer gets the same stop-on-all-paths treatment after scope publication.
path = Path("crates/medusa-runtime/src/mutating_worker_coordinator.rs")
text = path.read_text()
old = '''    request
        .team_context
        .clone()
        .execute(
            "team_send_message",
            &json!({"recipient":"lead","body":format!("{} implementation started", request.contract.task_id)}),
        )'''
new = '''    let result = (|| -> Result<WorkerRun, String> {
    request
        .team_context
        .clone()
        .execute(
            "team_send_message",
            &json!({"recipient":"lead","body":format!("{} implementation started", request.contract.task_id)}),
        )'''
if old not in text:
    raise SystemExit("mutating worker execution closure start missing")
text = text.replace(old, new, 1)
old = '''    Ok(WorkerRun {
        session_id: session.id.to_string(),
        turns: session.turn,
        summary,
    })
}

#[cfg(test)]'''
new = '''    Ok(WorkerRun {
        session_id: session.id.to_string(),
        turns: session.turn,
        summary,
    })
    })();
    let stop_cause = if result.is_ok() {
        "implementer worker completed"
    } else {
        "implementer worker stopped after execution failure"
    };
    let stop = engine
        .stop_session_scope(&session, stop_cause)
        .map_err(|error| error.to_string());
    match (result, stop) {
        (Ok(run), Ok(_)) => Ok(run),
        (Err(error), Ok(_)) => Err(error),
        (Ok(_), Err(stop_error)) => Err(format!("implementer scope teardown failed: {stop_error}")),
        (Err(error), Err(stop_error)) => Err(format!(
            "{error}; implementer scope teardown also failed: {stop_error}"
        )),
    }
}

#[cfg(test)]'''
if old not in text:
    raise SystemExit("mutating worker execution closure end missing")
path.write_text(text.replace(old, new, 1))
