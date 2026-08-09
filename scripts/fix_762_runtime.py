from pathlib import Path

path = Path("crates/medusa-runtime/src/frontend.rs")
text = path.read_text(encoding="utf-8")

replacements = [
    (
        "        let action = request.into_action();\n        let mut submission = lock_submission(&self.submission);\n",
        "        let action = request.into_action();\n        let submission = lock_submission(&self.submission);\n",
    ),
    (
        "    use serde_json::json;\n",
        "    use serde_json::{Value, json};\n",
    ),
    (
        """                SessionActionKind::Steer | SessionActionKind::GoalAdjustment
                    if view.lifecycle == SessionActionLifecycle::Committing
                        || view.lifecycle == SessionActionLifecycle::Running =>
                {
                    reconcile_interrupted_delivery(&self.repo, &view.action)?;
                }
""",
        """                SessionActionKind::Steer | SessionActionKind::GoalAdjustment
                    if view.lifecycle == SessionActionLifecycle::Committing
                        || view.lifecycle == SessionActionLifecycle::Running =>
                {
                    if !reconcile_interrupted_delivery(&self.repo, &view.action)? {
                        self.consume_restored_safe_boundary_entry(&view.action);
                        self.spawn_when_idle_action(view.action)?;
                    }
                }
""",
    ),
    (
        """                    } else if view.action.kind == SessionActionKind::GoalAdjustment
                        && !self.is_busy()
                    {
                        self.deliver_idle_goal_action(&view.action)?;
                    } else {
                        self.spawn_when_idle_action(view.action)?;
                    }
""",
        """                    } else if view.action.kind == SessionActionKind::GoalAdjustment
                        && !self.is_busy()
                    {
                        self.consume_restored_safe_boundary_entry(&view.action);
                        self.deliver_idle_goal_action(&view.action)?;
                    } else {
                        if !self.is_busy() {
                            self.consume_restored_safe_boundary_entry(&view.action);
                        }
                        self.spawn_when_idle_action(view.action)?;
                    }
""",
    ),
    (
        """    fn deliver_idle_message_action(&self, action: &SessionAction) -> Result<(), RuntimeError> {
        dispatch_when_idle(
""",
        """    fn consume_restored_safe_boundary_entry(&self, action: &SessionAction) {
        if action.delivery_policy != SessionActionDeliveryPolicy::NextSafeTurnBoundary {
            return;
        }
        let mut submission = lock_submission(&self.submission);
        if let Some(index) = submission
            .followups
            .iter()
            .position(|queued| queued.command_id == action.action_id)
        {
            submission.followups.remove(index);
        }
    }

    fn deliver_idle_message_action(&self, action: &SessionAction) -> Result<(), RuntimeError> {
        dispatch_when_idle(
""",
    ),
    (
        """                        if view.lifecycle == SessionActionLifecycle::Committing
                            || view.lifecycle == SessionActionLifecycle::Running
                        {
                            if let Err(error) = reconcile_interrupted_delivery(&repo, &action) {
                                let _ = event_sender.send(RuntimeEvent::Notice {
                                    title: \"Session action recovery failed\".to_owned(),
                                    details: vec![error.to_string()],
                                });
                            }
                            return;
                        }
""",
        """                        if view.lifecycle == SessionActionLifecycle::Committing
                            || view.lifecycle == SessionActionLifecycle::Running
                        {
                            match reconcile_interrupted_delivery(&repo, &action) {
                                Ok(true) => return,
                                Ok(false) => {}
                                Err(error) => {
                                    let _ = event_sender.send(RuntimeEvent::Notice {
                                        title: \"Session action recovery failed\".to_owned(),
                                        details: vec![error.to_string()],
                                    });
                                    return;
                                }
                            }
                        }
""",
    ),
    (
        """                    if let Err(error) = result {
                        let _ = event_sender.send(RuntimeEvent::Notice {
                            title: \"Session action delivery failed\".to_owned(),
                            details: vec![error.to_string()],
                        });
                    }
                    return;
""",
        """                    match result {
                        Ok(()) => return,
                        Err(RuntimeError::Busy) => {
                            thread::sleep(Duration::from_millis(10));
                            continue;
                        }
                        Err(error) => {
                            let _ = event_sender.send(RuntimeEvent::Notice {
                                title: \"Session action delivery failed\".to_owned(),
                                details: vec![error.to_string()],
                            });
                            return;
                        }
                    }
""",
    ),
]

for index, (old, new) in enumerate(replacements, 1):
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"replacement {index}: expected exactly one match, got {count}")
    text = text.replace(old, new, 1)

path.write_text(text, encoding="utf-8")
