//! Repository-wide lifecycle/privacy inventory used by certification tests.
//!
//! This is the machine-enforced authority for durable and derived Medusa state. The
//! human-readable matrix in `docs/data-lifecycle-certification.md` is derived from the
//! same categories and points back to production owners.

#[cfg(test)]
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Authority {
    Authoritative,
    Derived,
    Exported,
    Ephemeral,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Retention {
    WhileReferenced,
    SessionScoped,
    RepositoryScoped,
    UserScoped,
    SecurityAudit,
    Ephemeral,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Deletion {
    Immediate,
    TombstoneThenGc,
    ScopeBoundGc,
    ImmutableSecurityRecord,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Redaction {
    Redacted,
    MetadataOnly,
    ExcludeSecrets,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LifecycleEntry {
    pub data_class: &'static str,
    pub owner: &'static str,
    pub authority: Authority,
    pub storage: &'static str,
    pub provenance: &'static str,
    pub retention: Retention,
    pub max_retention_days: Option<u16>,
    pub gc_trigger: &'static str,
    pub exportable: bool,
    pub redaction: Redaction,
    pub deletion: Deletion,
    pub backup_implications: &'static str,
    pub visibility: &'static str,
}

pub const LIFECYCLE: &[LifecycleEntry] = &[
    entry(
        "session_journal_events",
        "medusa-agent::journal / medusa-protocol",
        Authority::Authoritative,
        "repository-local canonical session journal",
        "validated EventEnvelope stream",
        Retention::SessionScoped,
        None,
        "explicit session disposition after required audit/recovery references are gone",
        true,
        Redaction::ExcludeSecrets,
        Deletion::TombstoneThenGc,
        "backups inherit the session disposition and may not rehydrate a deleted live scope",
        "owning session and explicitly authorized projections",
    ),
    entry(
        "execution_checkpoints",
        "medusa-execution-checkpoint / medusa-runtime::checkpoint_store",
        Authority::Derived,
        "repository-local checkpoint store",
        "journal position plus validated execution payload",
        Retention::WhileReferenced,
        None,
        "checkpoint superseded and no recovery reference remains",
        true,
        Redaction::ExcludeSecrets,
        Deletion::ScopeBoundGc,
        "checkpoint backups follow their owning session and recovery window",
        "owning session",
    ),
    entry(
        "materialized_projections",
        "medusa-runtime / medusa-execution-replay",
        Authority::Derived,
        "rebuildable runtime projections",
        "authoritative journal replay",
        Retention::WhileReferenced,
        None,
        "projection invalidation or source-scope disposition",
        false,
        Redaction::ExcludeSecrets,
        Deletion::ScopeBoundGc,
        "rebuild rather than restore a projection after source deletion",
        "owning session/repository",
    ),
    entry(
        "compaction_manifests_and_semantic_summaries",
        "medusa-agent::compaction_v2 / medusa-context",
        Authority::Derived,
        "session compaction state",
        "bounded source ranges from session history",
        Retention::WhileReferenced,
        None,
        "source range expires or owning session is disposed",
        true,
        Redaction::ExcludeSecrets,
        Deletion::ScopeBoundGc,
        "summaries never extend the source retention window",
        "owning session",
    ),
    entry(
        "time_travel_branch_summaries",
        "medusa-time-travel / medusa-agent::branch_summary",
        Authority::Derived,
        "repository-local time-travel branch state",
        "journal fork plus branch-local execution",
        Retention::WhileReferenced,
        None,
        "branch abandoned/merged and no recovery reference remains",
        true,
        Redaction::ExcludeSecrets,
        Deletion::ScopeBoundGc,
        "abandoned-branch backups are collectible with the branch",
        "owning session/repository",
    ),
    entry(
        "frontend_message_history_and_transcripts",
        "medusa-runtime::frontend / observer / voice",
        Authority::Derived,
        "frontend-visible session projection",
        "redacted canonical message/session events",
        Retention::SessionScoped,
        None,
        "owning session disposition",
        true,
        Redaction::ExcludeSecrets,
        Deletion::ScopeBoundGc,
        "frontend caches must not restore a disposed session",
        "owning session and authorized frontend",
    ),
    entry(
        "tool_model_outputs_and_content_addressed_artifacts",
        "medusa-evidence::ArtifactStore",
        Authority::Derived,
        "content-addressed artifact objects plus metadata/read receipts",
        "typed evidence references from authorized execution scopes",
        Retention::WhileReferenced,
        None,
        "last authorized authoritative reference removed",
        true,
        Redaction::ExcludeSecrets,
        Deletion::ScopeBoundGc,
        "deduplicated blobs remain only while an authorized live reference exists; a hash is never authorization",
        "reference-owning session/repository/user scope only",
    ),
    entry(
        "repository_evidence_receipts_diffs_diagnostics_and_logs",
        "medusa-evidence / medusa-runtime / verification authorities",
        Authority::Derived,
        "repository-local evidence and verification records",
        "tool executions, repository revisions, and verification receipts",
        Retention::RepositoryScoped,
        None,
        "owning repository/session disposition subject to required security evidence",
        true,
        Redaction::Redacted,
        Deletion::ScopeBoundGc,
        "security-critical records keep only their declared audit window",
        "owning repository and authorized session",
    ),
    entry(
        "analysis_workspace_snapshots_and_exports",
        "medusa-runtime::analysis_workspace",
        Authority::Derived,
        "analysis workspace state and explicit exports",
        "authorized workspace inputs and analysis outputs",
        Retention::SessionScoped,
        None,
        "workspace/session disposition or explicit export cleanup",
        true,
        Redaction::Redacted,
        Deletion::ScopeBoundGc,
        "exports are separately user-managed once delivered; Medusa-owned copies follow workspace policy",
        "owning workspace/session",
    ),
    entry(
        "refinement_proposals_activation_history_evaluations_and_rollbacks",
        "medusa-improvement",
        Authority::Authoritative,
        "user/repository refinement state",
        "reviewed refinement proposals and activation receipts",
        Retention::UserScoped,
        None,
        "explicit learned/refined-state disposition, preserving required rollback/security evidence",
        true,
        Redaction::ExcludeSecrets,
        Deletion::TombstoneThenGc,
        "rollback lineage may not silently recreate deleted proposal content",
        "owning user/repository",
    ),
    entry(
        "executable_skill_packages_execution_artifacts_and_provenance",
        "medusa-extensions / runtime skill authorities",
        Authority::Derived,
        "repository/user scoped skill state and execution evidence",
        "installed package identity plus authorized execution receipts",
        Retention::RepositoryScoped,
        None,
        "skill removal or owning scope disposition after required provenance window",
        true,
        Redaction::ExcludeSecrets,
        Deletion::ScopeBoundGc,
        "package provenance may retain identity/hash without private execution payloads",
        "owning repository/user and authorized execution session",
    ),
    entry(
        "scheduled_and_session_action_records",
        "medusa-runtime::scheduled_actions / medusa-protocol::SessionAction",
        Authority::Authoritative,
        "canonical session action journal records",
        "validated action admission and lifecycle events",
        Retention::SessionScoped,
        None,
        "owning session disposition subject to required auditability",
        true,
        Redaction::ExcludeSecrets,
        Deletion::TombstoneThenGc,
        "backup copies follow the session lifecycle",
        "owning session/user",
    ),
    entry(
        "provider_and_oauth_metadata",
        "medusa-config / provider authorities",
        Authority::Authoritative,
        "configuration state excluding credential secret material",
        "explicit provider configuration and non-secret OAuth metadata",
        Retention::UserScoped,
        None,
        "provider disconnect/reset or user-state disposition",
        true,
        Redaction::MetadataOnly,
        Deletion::Immediate,
        "backups must exclude credentials and raw secret values",
        "owning user/repository configuration scope",
    ),
    entry(
        "telegram_voice_realtime_transcripts_media_metadata_and_acceptance_evidence",
        "medusa-runtime::voice / medusa-openai-realtime / frontend adapters",
        Authority::Derived,
        "session transcript projection and sanitized acceptance evidence",
        "authorized live-session events; raw audio is ephemeral unless an explicit feature requires otherwise",
        Retention::SessionScoped,
        None,
        "owning session disposition; raw live buffers are released at turn/session end",
        true,
        Redaction::Redacted,
        Deletion::ScopeBoundGc,
        "raw microphone/audio is not part of durable backup state by default",
        "owning session/user",
    ),
    entry(
        "configuration_history_and_redacted_audit_records",
        "medusa-config / medusa-runtime::config_command",
        Authority::Authoritative,
        "configuration state and canonical configuration-change records",
        "explicit configuration mutations with secret values excluded",
        Retention::RepositoryScoped,
        None,
        "configuration reset or repository/user disposition subject to audit requirements",
        true,
        Redaction::Redacted,
        Deletion::TombstoneThenGc,
        "backups store redacted configuration/audit data only",
        "owning repository/user",
    ),
    entry(
        "crash_support_and_diagnostic_bundles",
        "runtime support/diagnostic authorities",
        Authority::Exported,
        "explicitly generated diagnostic/support bundle",
        "bounded redacted snapshots of selected operational state",
        Retention::UserScoped,
        Some(30),
        "explicit deletion or bounded Medusa-owned support retention expiry",
        true,
        Redaction::Redacted,
        Deletion::Immediate,
        "Medusa-owned retained copies must expire; user-delivered copies are outside Medusa storage authority",
        "requesting user/support scope only",
    ),
    entry(
        "memory_markdown_authority_and_rebuildable_index",
        "medusa-memory",
        Authority::Authoritative,
        "authoritative Markdown plus rebuildable SQLite/search projection",
        "explicit memory writes with derived indexing",
        Retention::UserScoped,
        None,
        "memory deletion/update removes authoritative content and invalidates/rebuilds derived indexes",
        true,
        Redaction::ExcludeSecrets,
        Deletion::ScopeBoundGc,
        "derived indexes must not resurrect content missing from authoritative Markdown",
        "owning user/repository memory scope",
    ),
    entry(
        "prompt_mcp_and_context_caches",
        "medusa-prompt-cache / medusa-mcp-cache / medusa-context-retrieval",
        Authority::Derived,
        "bounded cache/index state",
        "authorized source content and cache keys",
        Retention::WhileReferenced,
        Some(30),
        "expiry, source invalidation, scope disposition, or bounded disk-pressure cleanup",
        false,
        Redaction::ExcludeSecrets,
        Deletion::ScopeBoundGc,
        "cache backups are unnecessary; rebuild only from live authorized sources",
        "same scope as source; cache key/hash is never authorization",
    ),
    entry(
        "temporary_worktrees_files_and_resource_pool_state",
        "medusa-process-containment / medusa-workers / runtime",
        Authority::Ephemeral,
        "temporary filesystem/process/resource-pool state",
        "bounded execution transaction",
        Retention::Ephemeral,
        Some(1),
        "transaction/session completion, cancellation, crash reconciliation, or expiry",
        false,
        Redaction::ExcludeSecrets,
        Deletion::Immediate,
        "never intentionally backed up",
        "owning transaction/session only",
    ),
];

const fn entry(
    data_class: &'static str,
    owner: &'static str,
    authority: Authority,
    storage: &'static str,
    provenance: &'static str,
    retention: Retention,
    max_retention_days: Option<u16>,
    gc_trigger: &'static str,
    exportable: bool,
    redaction: Redaction,
    deletion: Deletion,
    backup_implications: &'static str,
    visibility: &'static str,
) -> LifecycleEntry {
    LifecycleEntry {
        data_class,
        owner,
        authority,
        storage,
        provenance,
        retention,
        max_retention_days,
        gc_trigger,
        exportable,
        redaction,
        deletion,
        backup_implications,
        visibility,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQUIRED_CLASSES: &[&str] = &[
        "session_journal_events",
        "execution_checkpoints",
        "materialized_projections",
        "compaction_manifests_and_semantic_summaries",
        "time_travel_branch_summaries",
        "frontend_message_history_and_transcripts",
        "tool_model_outputs_and_content_addressed_artifacts",
        "repository_evidence_receipts_diffs_diagnostics_and_logs",
        "analysis_workspace_snapshots_and_exports",
        "refinement_proposals_activation_history_evaluations_and_rollbacks",
        "executable_skill_packages_execution_artifacts_and_provenance",
        "scheduled_and_session_action_records",
        "provider_and_oauth_metadata",
        "telegram_voice_realtime_transcripts_media_metadata_and_acceptance_evidence",
        "configuration_history_and_redacted_audit_records",
        "crash_support_and_diagnostic_bundles",
        "memory_markdown_authority_and_rebuildable_index",
        "prompt_mcp_and_context_caches",
        "temporary_worktrees_files_and_resource_pool_state",
    ];

    #[test]
    fn lifecycle_inventory_is_complete_unique_and_actionable() {
        let mut names = BTreeSet::new();
        for entry in LIFECYCLE {
            assert!(
                names.insert(entry.data_class),
                "duplicate lifecycle class: {}",
                entry.data_class
            );
            for (field, value) in [
                ("owner", entry.owner),
                ("storage", entry.storage),
                ("provenance", entry.provenance),
                ("gc_trigger", entry.gc_trigger),
                ("backup_implications", entry.backup_implications),
                ("visibility", entry.visibility),
            ] {
                assert!(
                    !value.trim().is_empty(),
                    "{} has empty {field}",
                    entry.data_class
                );
            }
        }
        for required in REQUIRED_CLASSES {
            assert!(
                names.contains(required),
                "missing required lifecycle class: {required}"
            );
        }
    }

    #[test]
    fn ephemeral_and_bounded_support_state_has_a_maximum_retention() {
        for entry in LIFECYCLE {
            if entry.authority == Authority::Ephemeral
                || entry.retention == Retention::Ephemeral
                || entry.authority == Authority::Exported
            {
                assert!(
                    entry.max_retention_days.is_some(),
                    "{} requires a bounded maximum retention",
                    entry.data_class
                );
            }
        }
    }

    #[test]
    fn exports_and_private_durable_state_have_explicit_redaction() {
        for entry in LIFECYCLE {
            if entry.exportable || entry.authority != Authority::Ephemeral {
                assert!(
                    matches!(
                        entry.redaction,
                        Redaction::Redacted | Redaction::MetadataOnly | Redaction::ExcludeSecrets
                    ),
                    "{} lacks a redaction policy",
                    entry.data_class
                );
            }
        }
    }

    #[test]
    fn content_hashes_are_never_declared_as_authorization() {
        let artifact = LIFECYCLE
            .iter()
            .find(|entry| entry.data_class == "tool_model_outputs_and_content_addressed_artifacts")
            .expect("artifact lifecycle entry");
        assert!(
            artifact
                .backup_implications
                .contains("hash is never authorization")
        );
        assert!(!artifact.visibility.contains("hash"));

        let caches = LIFECYCLE
            .iter()
            .find(|entry| entry.data_class == "prompt_mcp_and_context_caches")
            .expect("cache lifecycle entry");
        assert!(caches.visibility.contains("never authorization"));
    }

    #[test]
    fn disk_pressure_cannot_opportunistically_delete_authoritative_state() {
        for entry in LIFECYCLE
            .iter()
            .filter(|entry| entry.authority == Authority::Authoritative)
        {
            assert_ne!(
                entry.retention,
                Retention::Ephemeral,
                "{} is authoritative but ephemeral",
                entry.data_class
            );
        }
    }
}
