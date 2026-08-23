use std::{collections::BTreeMap, path::PathBuf};

use thiserror::Error;

pub type RuntimeInvariantCheck =
    Box<dyn Fn(&RuntimeInvariantContext) -> Result<(), String> + Send + Sync + 'static>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeInvariantContext {
    pub operation: String,
    pub repo: PathBuf,
    pub busy: bool,
    pub active_session_id: Option<String>,
}

impl RuntimeInvariantContext {
    pub fn new(
        operation: impl Into<String>,
        repo: PathBuf,
        busy: bool,
        active_session_id: Option<String>,
    ) -> Self {
        Self {
            operation: operation.into(),
            repo,
            busy,
            active_session_id,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeInvariantViolation {
    pub id: String,
    pub reason: String,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum RuntimeInvariantRegistryError {
    #[error("runtime invariant id must not be empty")]
    EmptyId,
    #[error("runtime invariant id is already registered: {0}")]
    DuplicateId(String),
}

#[derive(Default)]
pub struct RuntimeInvariantRegistry {
    checks: BTreeMap<String, RuntimeInvariantCheck>,
    generation: u64,
}

impl RuntimeInvariantRegistry {
    pub fn register<F>(
        &mut self,
        id: impl Into<String>,
        check: F,
    ) -> Result<(), RuntimeInvariantRegistryError>
    where
        F: Fn(&RuntimeInvariantContext) -> Result<(), String> + Send + Sync + 'static,
    {
        let id = id.into();
        if id.trim().is_empty() {
            return Err(RuntimeInvariantRegistryError::EmptyId);
        }
        if self.checks.contains_key(&id) {
            return Err(RuntimeInvariantRegistryError::DuplicateId(id));
        }
        self.checks.insert(id, Box::new(check));
        self.generation = self.generation.saturating_add(1);
        Ok(())
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let removed = self.checks.remove(id).is_some();
        if removed {
            self.generation = self.generation.saturating_add(1);
        }
        removed
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.checks.keys().map(String::as_str)
    }

    pub fn validate(&self, context: &RuntimeInvariantContext) -> Vec<RuntimeInvariantViolation> {
        self.checks
            .iter()
            .filter_map(|(id, check)| {
                check(context)
                    .err()
                    .map(|reason| RuntimeInvariantViolation {
                        id: id.clone(),
                        reason: if reason.trim().is_empty() {
                            "invariant check failed".to_owned()
                        } else {
                            reason
                        },
                    })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn registry_runs_checks_in_deterministic_order_and_reports_failures() {
        let mut registry = RuntimeInvariantRegistry::default();
        registry
            .register("z-last", |_context| Ok(()))
            .expect("register z-last");
        registry
            .register("a-first", |_context| Err("broken state".to_owned()))
            .expect("register a-first");

        let context = RuntimeInvariantContext::new("submit", PathBuf::from("C:/repo"), false, None);
        let violations = registry.validate(&context);
        assert_eq!(
            registry.ids().collect::<Vec<_>>(),
            vec!["a-first", "z-last"]
        );
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].id, "a-first");
        assert_eq!(violations[0].reason, "broken state");
    }

    #[test]
    fn registry_rejects_duplicates_and_removal_advances_generation() {
        let mut registry = RuntimeInvariantRegistry::default();
        registry
            .register("stable", |_context| Ok(()))
            .expect("register");
        let generation = registry.generation();
        assert!(matches!(
            registry.register("stable", |_context| Ok(())),
            Err(RuntimeInvariantRegistryError::DuplicateId(id)) if id == "stable"
        ));
        assert!(registry.remove("stable"));
        assert!(registry.generation() > generation);
        assert!(!registry.remove("stable"));
    }
}
