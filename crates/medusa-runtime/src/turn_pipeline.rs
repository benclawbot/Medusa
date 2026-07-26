use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use medusa_context::{ContextItem, ContextKind, ContextLedger};
use medusa_context_retrieval::{ContextRetriever, RetrievalQuery as ContextRetrievalQuery};
use medusa_markdown_memory::{
    MemoryDocument as MarkdownMemoryDocument, MemoryIndex,
    RetrievalQuery as MemoryRetrievalQuery,
};
use medusa_mcp_cache::{
    CacheDisposition, McpResultCache, ResultCacheKey, ServerId,
};
use medusa_memory_consolidation::{ConsolidatedMemory, MemoryConflict};
use medusa_memory_writeback::{
    MemoryDocument as WritebackDocument, WriteOperation, WritebackPlan, WritebackPolicy,
    plan_writeback,
};
use medusa_turn_assembly::{
    StableSection, ToolSchema, TurnAssembly, TurnAssemblyInput, TurnBudget,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use time::{Duration, OffsetDateTime};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnExecutionStatus {
    Verified,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TurnExecution {
    pub status: TurnExecutionStatus,
    pub output: String,
    #[serde(default)]
    pub memories: Vec<ConsolidatedMemory>,
    #[serde(default)]
    pub conflicts: Vec<MemoryConflict>,
}

pub trait TurnExecutor {
    fn execute(&mut self, rendered_prompt: &str) -> Result<TurnExecution, String>;
}

pub trait McpResourceResolver {
    fn resolve(&mut self, request: &McpResourceRequest) -> Result<Value, String>;
}

pub trait MemoryWritebackSink {
    fn commit(&mut self, plan: &WritebackPlan) -> Result<(), String>;
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct McpResourceRequest {
    pub server_id: ServerId,
    pub tool_name: String,
    pub input: Value,
    pub schema_fingerprint: String,
    pub protocol_version: String,
    pub server_version: Option<String>,
    pub ttl_seconds: i64,
    pub disposition: CacheDisposition,
}

impl McpResourceRequest {
    fn key(&self) -> Result<ResultCacheKey, TurnPipelineError> {
        ResultCacheKey::build(
            self.server_id.clone(),
            self.tool_name.clone(),
            &self.input,
            self.schema_fingerprint.clone(),
            self.protocol_version.clone(),
            self.server_version.clone(),
        )
        .map_err(TurnPipelineError::InvalidInput)
    }

    fn ttl(&self) -> Result<Duration, TurnPipelineError> {
        if self.ttl_seconds <= 0 {
            return Err(TurnPipelineError::InvalidInput(
                "MCP result ttl must be positive",
            ));
        }
        Ok(Duration::seconds(self.ttl_seconds))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct McpResourceProvenance {
    pub key_fingerprint: String,
    pub server_id: String,
    pub tool_name: String,
    pub cache_hit: bool,
    pub persisted: bool,
    pub value_fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TurnProvenance {
    pub context_ids: Vec<String>,
    pub context_retrieval_fingerprint: String,
    pub memory_chunk_ids: Vec<String>,
    pub memory_index_fingerprint: String,
    pub memory_result_fingerprint: String,
    pub mcp_resources: Vec<McpResourceProvenance>,
    pub stable_prefix_fingerprint: String,
    pub full_prompt_fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TurnPipelineInput {
    pub task: String,
    pub context_ledger: ContextLedger,
    #[serde(default)]
    pub markdown_memory: Vec<MarkdownMemoryDocument>,
    #[serde(default)]
    pub existing_writeback_documents: Vec<WritebackDocument>,
    #[serde(default)]
    pub mcp_requests: Vec<McpResourceRequest>,
    #[serde(default)]
    pub stable_sections: Vec<StableSection>,
    #[serde(default)]
    pub tool_schemas: Vec<ToolSchema>,
    pub budget: TurnBudget,
    #[serde(with = "time::serde::rfc3339")]
    pub now: OffsetDateTime,
    pub context_maximum_items: usize,
    pub context_maximum_bytes: usize,
    pub memory_maximum_results: usize,
    pub memory_maximum_bytes: usize,
    #[serde(default)]
    pub writeback_policy: WritebackPolicy,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TurnPipelineResult {
    pub execution: TurnExecution,
    pub assembly: TurnAssembly,
    pub provenance: TurnProvenance,
    pub writeback_plan: Option<WritebackPlan>,
}

#[derive(Debug, Error)]
pub enum TurnPipelineError {
    #[error("invalid turn pipeline input: {0}")]
    InvalidInput(&'static str),
    #[error("context retrieval failed: {0}")]
    Context(&'static str),
    #[error("memory retrieval failed: {0}")]
    Memory(&'static str),
    #[error("MCP resource resolution failed: {0}")]
    Mcp(String),
    #[error("turn assembly failed: {0}")]
    Assembly(&'static str),
    #[error("turn execution failed: {0}")]
    Execution(String),
    #[error("memory writeback planning failed: {0}")]
    WritebackPlan(&'static str),
    #[error("memory writeback commit failed: {0}")]
    WritebackCommit(String),
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct CanonicalTurnPipeline {
    mcp_cache: McpResultCache,
}

impl CanonicalTurnPipeline {
    #[must_use]
    pub fn new(mcp_cache: McpResultCache) -> Self {
        Self { mcp_cache }
    }

    #[must_use]
    pub fn mcp_cache(&self) -> &McpResultCache {
        &self.mcp_cache
    }

    pub fn run(
        &mut self,
        input: TurnPipelineInput,
        resolver: &mut dyn McpResourceResolver,
        executor: &mut dyn TurnExecutor,
        writeback: &mut dyn MemoryWritebackSink,
    ) -> Result<TurnPipelineResult, TurnPipelineError> {
        validate_input(&input)?;

        let context_result = ContextRetriever
            .retrieve(
                &input.context_ledger,
                &ContextRetrievalQuery {
                    text: input.task.clone(),
                    required_ids: BTreeSet::new(),
                    preferred_kinds: BTreeSet::from([
                        ContextKind::Goal,
                        ContextKind::Constraint,
                        ContextKind::Todo,
                        ContextKind::Blocker,
                        ContextKind::Failure,
                        ContextKind::Evidence,
                        ContextKind::Checkpoint,
                    ]),
                    maximum_items: input.context_maximum_items,
                    maximum_bytes: input.context_maximum_bytes,
                },
            )
            .map_err(TurnPipelineError::Context)?;

        let mut memory_index = MemoryIndex::default();
        for document in input.markdown_memory {
            memory_index
                .refresh(document)
                .map_err(TurnPipelineError::Memory)?;
        }
        let memory_result = memory_index
            .retrieve(&MemoryRetrievalQuery {
                text: input.task.clone(),
                maximum_results: input.memory_maximum_results,
                maximum_bytes: input.memory_maximum_bytes,
                preferred_paths: BTreeSet::new(),
                required_chunk_ids: BTreeSet::new(),
            })
            .map_err(TurnPipelineError::Memory)?;

        let mut assembled_context = context_result
            .items
            .iter()
            .map(|entry| entry.item.clone())
            .collect::<Vec<_>>();
        let mut sequence = assembled_context.len() as u64;
        let mut memory_chunk_ids = Vec::new();
        for hit in &memory_result.hits {
            sequence = sequence.saturating_add(1);
            memory_chunk_ids.push(hit.chunk.id.clone());
            assembled_context.push(
                ContextItem::new(
                    format!("memory:{}", hit.chunk.id),
                    ContextKind::Conversation,
                    format!(
                        "path={}\nheading={}\n{}",
                        hit.chunk.path, hit.chunk.heading, hit.chunk.body
                    ),
                    sequence,
                    input.now,
                )
                .map_err(TurnPipelineError::InvalidInput)?,
            );
        }

        let mut mcp_resources = Vec::new();
        for request in &input.mcp_requests {
            let key = request.key()?;
            let key_fingerprint = key.fingerprint();
            let cached = self.mcp_cache.get(&key, input.now).cloned();
            let (value, cache_hit, persisted, value_fingerprint) = match cached {
                Some(entry) => (
                    entry.value,
                    true,
                    true,
                    entry.value_fingerprint,
                ),
                None => {
                    let value = resolver
                        .resolve(request)
                        .map_err(TurnPipelineError::Mcp)?;
                    let persisted = self
                        .mcp_cache
                        .insert(
                            key,
                            value.clone(),
                            input.now,
                            request.ttl()?,
                            request.disposition,
                        )
                        .map_err(TurnPipelineError::InvalidInput)?;
                    let value_fingerprint = result_fingerprint(&value);
                    (value, false, persisted, value_fingerprint)
                }
            };
            sequence = sequence.saturating_add(1);
            assembled_context.push(
                ContextItem::new(
                    format!("mcp:{key_fingerprint}"),
                    ContextKind::Observation,
                    format!(
                        "server={}\ntool={}\nresult={}",
                        request.server_id.as_str(),
                        request.tool_name,
                        canonical_json(&value)
                    ),
                    sequence,
                    input.now,
                )
                .map_err(TurnPipelineError::InvalidInput)?,
            );
            mcp_resources.push(McpResourceProvenance {
                key_fingerprint,
                server_id: request.server_id.as_str().to_owned(),
                tool_name: request.tool_name.clone(),
                cache_hit,
                persisted,
                value_fingerprint,
            });
        }

        for (index, item) in assembled_context.iter_mut().enumerate() {
            item.sequence = index as u64 + 1;
        }

        let assembly = TurnAssemblyInput {
            stable_sections: input.stable_sections,
            tool_schemas: input.tool_schemas,
            retrieved_context: assembled_context,
            current_task: input.task,
            budget: input.budget,
        }
        .assemble()
        .map_err(TurnPipelineError::Assembly)?;

        let execution = executor
            .execute(&assembly.rendered_prompt)
            .map_err(TurnPipelineError::Execution)?;

        let writeback_plan = if execution.status == TurnExecutionStatus::Verified {
            let plan = plan_writeback(
                &execution.memories,
                &execution.conflicts,
                &input.existing_writeback_documents,
                &input.writeback_policy,
            )
            .map_err(TurnPipelineError::WritebackPlan)?;
            writeback
                .commit(&plan)
                .map_err(TurnPipelineError::WritebackCommit)?;
            Some(plan)
        } else {
            None
        };

        let provenance = TurnProvenance {
            context_ids: context_result
                .items
                .iter()
                .map(|entry| entry.item.id.clone())
                .collect(),
            context_retrieval_fingerprint: context_result.fingerprint,
            memory_chunk_ids,
            memory_index_fingerprint: memory_result.index_fingerprint,
            memory_result_fingerprint: memory_result.result_fingerprint,
            mcp_resources,
            stable_prefix_fingerprint: assembly.stable_prefix_fingerprint.clone(),
            full_prompt_fingerprint: assembly.full_prompt_fingerprint.clone(),
        };

        Ok(TurnPipelineResult {
            execution,
            assembly,
            provenance,
            writeback_plan,
        })
    }
}

fn validate_input(input: &TurnPipelineInput) -> Result<(), TurnPipelineError> {
    if input.task.trim().is_empty() {
        return Err(TurnPipelineError::InvalidInput(
            "current task cannot be empty",
        ));
    }
    if input.context_maximum_items == 0 || input.context_maximum_bytes == 0 {
        return Err(TurnPipelineError::InvalidInput(
            "context retrieval budgets must be positive",
        ));
    }
    if input.memory_maximum_results == 0 || input.memory_maximum_bytes == 0 {
        return Err(TurnPipelineError::InvalidInput(
            "memory retrieval budgets must be positive",
        ));
    }
    Ok(())
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned()),
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Object(values) => {
            let mut fields = values.iter().collect::<Vec<_>>();
            fields.sort_by(|left, right| left.0.cmp(right.0));
            format!(
                "{{{}}}",
                fields
                    .into_iter()
                    .map(|(key, value)| format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_owned()),
                        canonical_json(value)
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
    }
}

fn result_fingerprint(value: &Value) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(canonical_json(value).as_bytes()))
}

pub struct FilesystemMemoryWritebackSink {
    root: PathBuf,
}

impl FilesystemMemoryWritebackSink {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn target(&self, relative: &str) -> Result<PathBuf, String> {
        let path = Path::new(relative);
        if path.is_absolute() || path.components().any(|part| {
            matches!(part, std::path::Component::ParentDir)
        }) {
            return Err("memory path escapes the repository".to_owned());
        }
        Ok(self.root.join(path))
    }
}

impl MemoryWritebackSink for FilesystemMemoryWritebackSink {
    fn commit(&mut self, plan: &WritebackPlan) -> Result<(), String> {
        let transaction_id = &plan.plan_fingerprint[..16.min(plan.plan_fingerprint.len())];
        let mut staged = Vec::new();
        for patch in &plan.patches {
            if patch.operation == WriteOperation::NoChange {
                continue;
            }
            let target = self.target(&patch.path)?;
            let parent = target
                .parent()
                .ok_or_else(|| "memory target has no parent directory".to_owned())?;
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            let temporary = target.with_extension(format!("md.{transaction_id}.tmp"));
            fs::write(&temporary, patch.content.as_bytes()).map_err(|error| error.to_string())?;
            staged.push((target, temporary));
        }

        let mut committed = Vec::new();
        for (target, temporary) in &staged {
            let backup = target.with_extension(format!("md.{transaction_id}.bak"));
            if target.exists() {
                fs::rename(target, &backup).map_err(|error| error.to_string())?;
            }
            if let Err(error) = fs::rename(temporary, target) {
                if backup.exists() {
                    let _ = fs::rename(&backup, target);
                }
                for (completed_target, completed_backup) in committed.iter().rev() {
                    let _ = fs::remove_file(completed_target);
                    if completed_backup.exists() {
                        let _ = fs::rename(completed_backup, completed_target);
                    }
                }
                return Err(error.to_string());
            }
            committed.push((target.clone(), backup));
        }
        for (_, backup) in committed {
            if backup.exists() {
                fs::remove_file(backup).map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use medusa_memory_consolidation::MemoryKind;
    use serde_json::json;
    use time::macros::datetime;

    use super::*;

    #[derive(Default)]
    struct Resolver {
        calls: usize,
    }

    impl McpResourceResolver for Resolver {
        fn resolve(&mut self, _request: &McpResourceRequest) -> Result<Value, String> {
            self.calls += 1;
            Ok(json!({"repository":"medusa","status":"ready"}))
        }
    }

    struct Executor {
        prompts: Arc<Mutex<Vec<String>>>,
        status: TurnExecutionStatus,
    }

    impl TurnExecutor for Executor {
        fn execute(&mut self, rendered_prompt: &str) -> Result<TurnExecution, String> {
            self.prompts
                .lock()
                .expect("prompts")
                .push(rendered_prompt.to_owned());
            Ok(TurnExecution {
                status: self.status,
                output: "done".to_owned(),
                memories: vec![ConsolidatedMemory {
                    key: "project\u{1f}language\u{1f}Fact".to_owned(),
                    subject: "Project".to_owned(),
                    predicate: "language".to_owned(),
                    value: "Rust".to_owned(),
                    kind: MemoryKind::Fact,
                    support_ids: vec!["evidence-1".to_owned()],
                    confidence_basis_points: 9_000,
                    fingerprint: "memory-fingerprint".to_owned(),
                }],
                conflicts: Vec::new(),
            })
        }
    }

    #[derive(Default)]
    struct Sink {
        commits: usize,
        plans: Vec<WritebackPlan>,
    }

    impl MemoryWritebackSink for Sink {
        fn commit(&mut self, plan: &WritebackPlan) -> Result<(), String> {
            self.commits += 1;
            self.plans.push(plan.clone());
            Ok(())
        }
    }

    fn input(disposition: CacheDisposition) -> TurnPipelineInput {
        let now = datetime!(2026-07-26 08:00 UTC);
        let mut ledger = ContextLedger::default();
        ledger
            .append(
                ContextItem::new(
                    "goal",
                    ContextKind::Goal,
                    "ship the runtime turn pipeline",
                    1,
                    now,
                )
                .expect("context"),
            )
            .expect("append");
        TurnPipelineInput {
            task: "ship the runtime turn pipeline".to_owned(),
            context_ledger: ledger,
            markdown_memory: vec![MarkdownMemoryDocument {
                path: "memory/project.md".to_owned(),
                content: "# Preference\nUse deterministic Rust pipelines".to_owned(),
                revision: 1,
            }],
            existing_writeback_documents: Vec::new(),
            mcp_requests: vec![McpResourceRequest {
                server_id: ServerId::parse("github").expect("server"),
                tool_name: "repository_status".to_owned(),
                input: json!({"repository":"medusa"}),
                schema_fingerprint: "schema-v1".to_owned(),
                protocol_version: "2025-11-25".to_owned(),
                server_version: Some("1.0.0".to_owned()),
                ttl_seconds: 300,
                disposition,
            }],
            stable_sections: vec![
                StableSection::new("system", "Follow repository policy").expect("section"),
            ],
            tool_schemas: vec![
                ToolSchema::new("repository_status", "{\"type\":\"object\"}")
                    .expect("tool"),
            ],
            budget: TurnBudget {
                maximum_input_tokens: 32_000,
                reserved_output_tokens: 4_000,
            },
            now,
            context_maximum_items: 16,
            context_maximum_bytes: 32_000,
            memory_maximum_results: 8,
            memory_maximum_bytes: 16_000,
            writeback_policy: WritebackPolicy::default(),
        }
    }

    #[test]
    fn retrieval_assembly_execution_and_verified_writeback_are_one_pipeline() {
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let mut pipeline = CanonicalTurnPipeline::default();
        let mut resolver = Resolver::default();
        let mut executor = Executor {
            prompts: Arc::clone(&prompts),
            status: TurnExecutionStatus::Verified,
        };
        let mut sink = Sink::default();
        let result = pipeline
            .run(
                input(CacheDisposition::Cacheable),
                &mut resolver,
                &mut executor,
                &mut sink,
            )
            .expect("pipeline");

        assert_eq!(resolver.calls, 1);
        assert_eq!(sink.commits, 1);
        assert!(result.writeback_plan.is_some());
        assert_eq!(result.provenance.context_ids, vec!["goal"]);
        assert_eq!(result.provenance.memory_chunk_ids.len(), 1);
        assert_eq!(result.provenance.mcp_resources.len(), 1);
        let prompt = prompts.lock().expect("prompts").pop().expect("prompt");
        assert!(prompt.contains("ship the runtime turn pipeline"));
        assert!(prompt.contains("deterministic Rust pipelines"));
        assert!(prompt.contains("repository_status"));
    }

    #[test]
    fn failed_and_cancelled_turns_never_write_memory() {
        for status in [TurnExecutionStatus::Failed, TurnExecutionStatus::Cancelled] {
            let mut pipeline = CanonicalTurnPipeline::default();
            let mut resolver = Resolver::default();
            let mut executor = Executor {
                prompts: Arc::new(Mutex::new(Vec::new())),
                status,
            };
            let mut sink = Sink::default();
            let result = pipeline
                .run(
                    input(CacheDisposition::Cacheable),
                    &mut resolver,
                    &mut executor,
                    &mut sink,
                )
                .expect("pipeline");
            assert_eq!(sink.commits, 0);
            assert!(result.writeback_plan.is_none());
        }
    }

    #[test]
    fn repeated_mcp_request_hits_cache_and_sensitive_results_do_not_persist() {
        let mut pipeline = CanonicalTurnPipeline::default();
        let mut resolver = Resolver::default();
        for _ in 0..2 {
            let mut executor = Executor {
                prompts: Arc::new(Mutex::new(Vec::new())),
                status: TurnExecutionStatus::Failed,
            };
            let mut sink = Sink::default();
            pipeline
                .run(
                    input(CacheDisposition::Cacheable),
                    &mut resolver,
                    &mut executor,
                    &mut sink,
                )
                .expect("pipeline");
        }
        assert_eq!(resolver.calls, 1);

        let mut sensitive_pipeline = CanonicalTurnPipeline::default();
        let mut sensitive_resolver = Resolver::default();
        for _ in 0..2 {
            let mut executor = Executor {
                prompts: Arc::new(Mutex::new(Vec::new())),
                status: TurnExecutionStatus::Failed,
            };
            let mut sink = Sink::default();
            sensitive_pipeline
                .run(
                    input(CacheDisposition::Sensitive),
                    &mut sensitive_resolver,
                    &mut executor,
                    &mut sink,
                )
                .expect("pipeline");
        }
        assert_eq!(sensitive_resolver.calls, 2);
        assert!(sensitive_pipeline.mcp_cache().is_empty());
    }

    #[test]
    fn identical_ordered_inputs_produce_identical_assembly() {
        let first_input = input(CacheDisposition::Cacheable);
        let second_input = first_input.clone();
        let mut first_pipeline = CanonicalTurnPipeline::default();
        let mut second_pipeline = CanonicalTurnPipeline::default();
        let mut first_resolver = Resolver::default();
        let mut second_resolver = Resolver::default();
        let mut first_executor = Executor {
            prompts: Arc::new(Mutex::new(Vec::new())),
            status: TurnExecutionStatus::Failed,
        };
        let mut second_executor = Executor {
            prompts: Arc::new(Mutex::new(Vec::new())),
            status: TurnExecutionStatus::Failed,
        };
        let mut first_sink = Sink::default();
        let mut second_sink = Sink::default();
        let first = first_pipeline
            .run(
                first_input,
                &mut first_resolver,
                &mut first_executor,
                &mut first_sink,
            )
            .expect("first");
        let second = second_pipeline
            .run(
                second_input,
                &mut second_resolver,
                &mut second_executor,
                &mut second_sink,
            )
            .expect("second");
        assert_eq!(
            first.assembly.full_prompt_fingerprint,
            second.assembly.full_prompt_fingerprint
        );
        assert_eq!(first.provenance, second.provenance);
    }

    #[test]
    fn filesystem_sink_applies_writeback_atomically() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut pipeline = CanonicalTurnPipeline::default();
        let mut resolver = Resolver::default();
        let mut executor = Executor {
            prompts: Arc::new(Mutex::new(Vec::new())),
            status: TurnExecutionStatus::Verified,
        };
        let mut sink = FilesystemMemoryWritebackSink::new(directory.path());
        pipeline
            .run(
                input(CacheDisposition::Cacheable),
                &mut resolver,
                &mut executor,
                &mut sink,
            )
            .expect("pipeline");
        let memory = fs::read_to_string(directory.path().join("memory/MEMORY.md"))
            .expect("memory file");
        assert!(memory.contains("Project — language"));
        assert!(memory.contains("Rust"));
    }
}
