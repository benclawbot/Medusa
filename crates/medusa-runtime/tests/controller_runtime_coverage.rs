use std::{
    fs,
    path::PathBuf,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use medusa_runtime::{
    RuntimeController, RuntimeEvent,
    commands::{Effort, ModelCommand, ModelConfiguration, SlashCommand},
};

fn repo() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("medusa-runtime-coverage-{nonce}"));
    fs::create_dir_all(&path).expect("create repo");
    path
}

fn collect(controller: &RuntimeController, minimum: usize) -> Vec<RuntimeEvent> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut events = Vec::new();
    while Instant::now() < deadline {
        match controller.try_event() {
            Ok(Some(event)) => events.push(event),
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => panic!("runtime stopped: {error}"),
        }
        if events.len() >= minimum {
            break;
        }
    }
    events
}

fn run(controller: &RuntimeController, command: SlashCommand, minimum: usize) -> Vec<RuntimeEvent> {
    controller.run_command(command).expect("run command");
    collect(controller, minimum)
}

#[test]
fn controller_exercises_non_agent_command_lifecycle() {
    let repo = repo();
    let controller = RuntimeController::start(repo.clone());
    let initial = collect(&controller, 1);
    assert!(
        initial
            .iter()
            .any(|event| matches!(event, RuntimeEvent::Settings { .. }))
    );
    assert!(!controller.is_busy());
    assert!(!controller.cancel());

    let help = run(&controller, SlashCommand::Help, 1);
    assert!(help.iter().any(
        |event| matches!(event, RuntimeEvent::Notice { title, .. } if title == "Slash commands")
    ));

    let goal = run(
        &controller,
        SlashCommand::Goal {
            objective: Some("ship safely".into()),
        },
        1,
    );
    assert!(goal.iter().any(
        |event| matches!(event, RuntimeEvent::Notice { title, .. } if title == "Goal updated")
    ));
    let current_goal = run(&controller, SlashCommand::Goal { objective: None }, 1);
    assert!(current_goal.iter().any(
        |event| matches!(event, RuntimeEvent::Notice { title, .. } if title == "Current goal")
    ));

    let compact = run(&controller, SlashCommand::Compact { focus: None }, 1);
    assert!(compact.iter().any(
        |event| matches!(event, RuntimeEvent::Notice { title, .. } if title == "Nothing to compact")
    ));

    run(&controller, SlashCommand::Model(ModelCommand::Show), 1);
    run(
        &controller,
        SlashCommand::Model(ModelCommand::SetModel("alternate-model".into())),
        2,
    );
    run(
        &controller,
        SlashCommand::Model(ModelCommand::SetProvider("anthropic".into())),
        2,
    );
    let invalid_provider = run(
        &controller,
        SlashCommand::Model(ModelCommand::SetProvider("invalid".into())),
        1,
    );
    assert!(invalid_provider.iter().any(
        |event| matches!(event, RuntimeEvent::Notice { title, .. } if title == "Command failed")
    ));
    run(
        &controller,
        SlashCommand::Model(ModelCommand::SetApiKey("session-secret".into())),
        1,
    );

    run(
        &controller,
        SlashCommand::Effort {
            effort: Some(Effort::Low),
        },
        1,
    );
    run(
        &controller,
        SlashCommand::Effort {
            effort: Some(Effort::Auto),
        },
        1,
    );
    let effort = run(&controller, SlashCommand::Effort { effort: None }, 1);
    assert!(
        effort
            .iter()
            .any(|event| matches!(event, RuntimeEvent::Notice { title, .. } if title == "Effort"))
    );

    let skills = run(&controller, SlashCommand::Skills, 1);
    assert!(skills.iter().any(
        |event| matches!(event, RuntimeEvent::Notice { title, .. } if title == "Available skills")
    ));

    run(&controller, SlashCommand::Plan { task: None }, 2);
    run(
        &controller,
        SlashCommand::Plan {
            task: Some("off".into()),
        },
        2,
    );

    controller
        .configure_model(ModelConfiguration {
            provider: "minimax".into(),
            model: "MiniMax-M3".into(),
            effort: Effort::Medium,
            api_key: Some("temporary".into()),
        })
        .expect("configure model");
    let configured = collect(&controller, 2);
    assert!(
        configured
            .iter()
            .any(|event| matches!(event, RuntimeEvent::Settings { .. }))
    );

    let fresh = run(&controller, SlashCommand::New, 2);
    assert!(
        fresh
            .iter()
            .any(|event| matches!(event, RuntimeEvent::NewSession))
    );
    assert!(!controller.is_busy());

    drop(controller);
    fs::remove_dir_all(repo).expect("remove repo");
}
