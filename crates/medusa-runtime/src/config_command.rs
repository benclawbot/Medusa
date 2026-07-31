use std::{collections::BTreeMap, sync::mpsc::Sender};

use medusa_config::{
    Config, ConfigurationApplyTiming, ConfigurationChangeOrigin, ProviderProfile,
    ProviderProfileCatalog,
};

use crate::{RuntimeError, RuntimeEvent, RuntimeState, commands::ConfigCommand};

pub(super) fn execute(
    state: &mut RuntimeState,
    command: ConfigCommand,
    events: &Sender<RuntimeEvent>,
) -> Result<(), RuntimeError> {
    let catalog = ProviderProfileCatalog::user().map_err(RuntimeError::agent)?;
    execute_with_catalog(state, command, events, &catalog)
}

fn execute_with_catalog(
    state: &mut RuntimeState,
    command: ConfigCommand,
    events: &Sender<RuntimeEvent>,
    catalog: &ProviderProfileCatalog,
) -> Result<(), RuntimeError> {
    match command {
        ConfigCommand::Show => {
            let snapshot = catalog.snapshot().map_err(RuntimeError::agent)?;
            let effective = effective_config(state, &snapshot.profile)?;
            send_notice(
                events,
                "Configuration",
                status_details(
                    catalog,
                    snapshot.revision,
                    &snapshot.active_profile,
                    &snapshot.profile,
                    &effective,
                ),
            );
        }
        ConfigCommand::Profiles => {
            let revision = catalog.revision().map_err(RuntimeError::agent)?;
            let profiles = catalog.list().map_err(RuntimeError::agent)?;
            let mut details = vec![format!("Configuration revision: {revision}")];
            details.extend(profiles
                .into_iter()
                .map(|profile| {
                    let marker = if profile.active { "*" } else { " " };
                    let configured = if profile.configured {
                        "configured"
                    } else {
                        "not configured"
                    };
                    format!(
                        "[{marker}] {} — {} / {} ({configured})",
                        profile.name, profile.provider, profile.model
                    )
                })
                .collect::<Vec<_>>());
            send_notice(events, "Provider profiles", details);
        }
        ConfigCommand::UseProfile { name } => {
            let revision = catalog.revision().map_err(RuntimeError::agent)?;
            let profile = catalog.load_profile(&name).map_err(RuntimeError::agent)?;
            let effective = effective_config(state, &profile)?;
            let (_, change) = catalog
                .use_profile_at_revision(&name, revision, ConfigurationChangeOrigin::Tui)
                .map_err(RuntimeError::agent)?;
            apply_effective_model(state, effective);
            let _ = events.send(RuntimeEvent::ConfigurationChanged(change.clone()));
            let _ = events.send(state.settings_event());
            send_notice(
                events,
                "Provider profile selected",
                vec![
                    format!("Active profile: {name}"),
                    format!("Configuration revision: {}", change.revision),
                    format!(
                        "Effective route: {} / {}",
                        state.config.model.provider, state.config.model.name
                    ),
                    "Credentials remain external and are never displayed by /config.".to_owned(),
                ],
            );
        }
        ConfigCommand::Set { key, value } => {
            let snapshot = catalog.snapshot().map_err(RuntimeError::agent)?;
            let mut candidate = snapshot.profile;
            candidate
                .set_value(&key, &value)
                .map_err(RuntimeError::agent)?;
            let effective = effective_config(state, &candidate)?;
            let change = catalog
                .save_active_profile(
                    &candidate,
                    snapshot.revision,
                    ConfigurationChangeOrigin::Tui,
                    [key.clone()],
                    ConfigurationApplyTiming::Immediate,
                )
                .map_err(RuntimeError::agent)?;
            apply_effective_model(state, effective);
            let _ = events.send(RuntimeEvent::ConfigurationChanged(change.clone()));
            let _ = events.send(state.settings_event());
            send_notice(
                events,
                "Configuration updated",
                vec![
                    format!(
                        "Profile: {}",
                        catalog.active_name().map_err(RuntimeError::agent)?
                    ),
                    format!("Updated key: {key}"),
                    format!("Configuration revision: {}", change.revision),
                    format!(
                        "Effective route: {} / {}",
                        state.config.model.provider, state.config.model.name
                    ),
                ],
            );
        }
        ConfigCommand::Unset { key } => {
            let snapshot = catalog.snapshot().map_err(RuntimeError::agent)?;
            let mut candidate = snapshot.profile;
            candidate.unset_value(&key).map_err(RuntimeError::agent)?;
            let effective = effective_config(state, &candidate)?;
            let change = catalog
                .save_active_profile(
                    &candidate,
                    snapshot.revision,
                    ConfigurationChangeOrigin::Tui,
                    [key.clone()],
                    ConfigurationApplyTiming::Immediate,
                )
                .map_err(RuntimeError::agent)?;
            apply_effective_model(state, effective);
            let _ = events.send(RuntimeEvent::ConfigurationChanged(change.clone()));
            let _ = events.send(state.settings_event());
            send_notice(
                events,
                "Configuration reset",
                vec![
                    format!(
                        "Profile: {}",
                        catalog.active_name().map_err(RuntimeError::agent)?
                    ),
                    format!("Reset key: {key}"),
                    format!("Configuration revision: {}", change.revision),
                    format!(
                        "Effective route: {} / {}",
                        state.config.model.provider, state.config.model.name
                    ),
                ],
            );
        }
        ConfigCommand::Validate => {
            let snapshot = catalog.snapshot().map_err(RuntimeError::agent)?;
            let effective = effective_config(state, &snapshot.profile)?;
            send_notice(
                events,
                "Configuration is valid",
                vec![
                    format!("Active profile: {}", snapshot.active_profile),
                    format!("Configuration revision: {}", snapshot.revision),
                    format!(
                        "Effective route: {} / {} ({}, auth: {})",
                        effective.model.provider,
                        effective.model.name,
                        effective.model.protocol,
                        effective.model.auth
                    ),
                    "Validation performed no provider request and exposed no credential material."
                        .to_owned(),
                ],
            );
        }
    }
    Ok(())
}

fn effective_config(
    state: &RuntimeState,
    profile: &ProviderProfile,
) -> Result<Config, RuntimeError> {
    let project = state.repo.join(".medusa/config.toml");
    let project = project.exists().then_some(project);
    Config::load_layers_with_provider_profile(
        profile,
        None,
        project.as_deref(),
        &BTreeMap::new(),
        &BTreeMap::new(),
    )
    .map_err(RuntimeError::agent)
}

fn apply_effective_model(state: &mut RuntimeState, effective: Config) {
    state.base_config.model = effective.model.clone();
    state.config.model = effective.model;
}

fn status_details(
    catalog: &ProviderProfileCatalog,
    revision: u64,
    name: &str,
    profile: &ProviderProfile,
    effective: &Config,
) -> Vec<String> {
    vec![
        format!("Configuration revision: {revision}"),
        format!("Active profile: {name}"),
        format!("Profile path: {}", catalog.active_store().map_or_else(
            |error| format!("unavailable: {error}"),
            |store| store.path().display().to_string()
        )),
        format!(
            "Stored route: {} / {} (connection: {}, auth: {})",
            profile.provider, profile.model, profile.connection, profile.auth
        ),
        format!(
            "Effective route: {} / {} ({}, auth: {})",
            effective.model.provider,
            effective.model.name,
            effective.model.protocol,
            effective.model.auth
        ),
        format!(
            "Base URL: {}",
            effective.model.base_url.as_deref().unwrap_or("provider default")
        ),
        format!("Configured: {}", profile.configured),
        "Credentials are external and redacted. Use /config profiles, /config use <name>, /config set <key> <value>, /config unset <key>, or /config validate."
            .to_owned(),
    ]
}

fn send_notice(events: &Sender<RuntimeEvent>, title: &str, details: Vec<String>) {
    let _ = events.send(RuntimeEvent::Notice {
        title: title.to_owned(),
        details,
    });
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use medusa_config::{Mode, ProviderProfile};

    use super::*;
    use crate::commands::Effort;

    fn configured_profile(model: &str) -> ProviderProfile {
        ProviderProfile {
            model: model.to_owned(),
            configured: true,
            ..ProviderProfile::default()
        }
    }

    fn state(repo: &std::path::Path, profile: &ProviderProfile) -> RuntimeState {
        let config = Config::load_layers_with_provider_profile(
            profile,
            None,
            None,
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .expect("config");
        RuntimeState::from_config(repo.to_path_buf(), config)
    }

    #[test]
    fn invalid_effective_mutation_does_not_replace_profile() {
        let directory = tempfile::tempdir().expect("tempdir");
        let catalog = ProviderProfileCatalog::at(directory.path().join("config"));
        let original = configured_profile("MiniMax-M3");
        catalog
            .active_store()
            .expect("store")
            .save(&original)
            .expect("save");
        let mut runtime = state(directory.path(), &original);
        let (events, _) = mpsc::channel();

        let error = execute_with_catalog(
            &mut runtime,
            ConfigCommand::Set {
                key: "auth".to_owned(),
                value: "oauth".to_owned(),
            },
            &events,
            &catalog,
        )
        .expect_err("effective auth must fail");

        assert!(error.to_string().contains("auth must be api-key or none"));
        assert_eq!(
            catalog.active_store().expect("store").load().expect("load"),
            original
        );
    }

    #[test]
    fn profile_selection_refreshes_model_and_preserves_process_state() {
        let directory = tempfile::tempdir().expect("tempdir");
        let catalog = ProviderProfileCatalog::at(directory.path().join("config"));
        let default = configured_profile("default-model");
        catalog
            .active_store()
            .expect("default store")
            .save(&default)
            .expect("save default");
        catalog.create("work").expect("create work");
        catalog.use_profile("work").expect("select work");
        let mut work = catalog
            .active_store()
            .expect("work store")
            .load()
            .expect("load");
        work.set_value("model", "work-model").expect("set model");
        catalog
            .active_store()
            .expect("work store")
            .save(&work)
            .expect("save");
        catalog.use_profile("default").expect("select default");

        let mut runtime = state(directory.path(), &default);
        runtime.effort = Effort::High;
        runtime.plan_mode = true;
        runtime.config.agent.mode = Mode::ReadOnly;
        runtime.session_api_key = Some("process-only-secret".to_owned());
        let (events, _) = mpsc::channel();

        execute_with_catalog(
            &mut runtime,
            ConfigCommand::UseProfile {
                name: "work".to_owned(),
            },
            &events,
            &catalog,
        )
        .expect("select profile");

        assert_eq!(runtime.config.model.name, "work-model");
        assert_eq!(runtime.effort, Effort::High);
        assert!(runtime.plan_mode);
        assert_eq!(runtime.config.agent.mode, Mode::ReadOnly);
        assert_eq!(
            runtime.session_api_key.as_deref(),
            Some("process-only-secret")
        );
    }

    #[test]
    fn project_model_override_remains_higher_precedence() {
        let directory = tempfile::tempdir().expect("tempdir");
        let project_root = directory.path().join(".medusa");
        std::fs::create_dir_all(&project_root).expect("project config root");
        std::fs::write(
            project_root.join("config.toml"),
            "[model]\nname = 'project-model'\n",
        )
        .expect("project config");
        let catalog = ProviderProfileCatalog::at(directory.path().join("profile-config"));
        let profile = configured_profile("profile-model");
        catalog
            .active_store()
            .expect("store")
            .save(&profile)
            .expect("save");
        let mut runtime = state(directory.path(), &profile);
        let (events, _) = mpsc::channel();

        execute_with_catalog(
            &mut runtime,
            ConfigCommand::Set {
                key: "model".to_owned(),
                value: "new-profile-model".to_owned(),
            },
            &events,
            &catalog,
        )
        .expect("set profile model");

        assert_eq!(runtime.config.model.name, "project-model");
        assert_eq!(
            catalog
                .active_store()
                .expect("store")
                .load()
                .expect("load")
                .model,
            "new-profile-model"
        );
    }

    #[test]
    fn show_notice_is_redacted_and_discoverable() {
        let directory = tempfile::tempdir().expect("tempdir");
        let catalog = ProviderProfileCatalog::at(directory.path().join("config"));
        let profile = configured_profile("MiniMax-M3");
        catalog
            .active_store()
            .expect("store")
            .save(&profile)
            .expect("save");
        let mut runtime = state(directory.path(), &profile);
        runtime.session_api_key = Some("never-print-this".to_owned());
        let (events, receiver) = mpsc::channel();

        execute_with_catalog(&mut runtime, ConfigCommand::Show, &events, &catalog)
            .expect("show config");
        let RuntimeEvent::Notice { details, .. } = receiver.recv().expect("notice") else {
            panic!("expected notice");
        };
        let output = details.join("\n");
        assert!(output.contains("Active profile"));
        assert!(output.contains("Credentials are external and redacted"));
        assert!(!output.contains("never-print-this"));
    }
}
