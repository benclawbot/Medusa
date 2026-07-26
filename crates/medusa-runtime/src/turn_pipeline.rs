//! Canonical context, memory, MCP, assembly, execution, and verified writeback pipeline.

use std::collections::BTreeMap;

use medusa_context::ContextItem;
use medusa_markdown_memory::{MemoryIndex, RetrievalQuery};
use medusa_memory_consolidation::{ConsolidationPolicy, MemoryObservation, consolidate};
use medusa_memory_writeback::{
    MemoryDocument as WritebackDocument, WritebackPlan, WritebackPolicy, plan_writeback,
};
use medusa_turn_assembly::{
    StableSection, ToolSchema, TurnAssembly, TurnAssemblyInput, TurnBudget,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct McpResourceRequest {
    pub server_id: String,
    pub tool: String,
    pub input: Value,
    pub schema_fingerprint: String,
    pub protocol_version: String,
    pub content: String,
    pub expires_at: OffsetDateTime,
    #[serde(default)]
    pub cacheable: bool,
    #[serde(default)]
    pub sensitive: bool,
}

impl McpResourceRequest {
    fn validate(&self, now: OffsetDateTime) -> Result<(), TurnPipelineError> {
        if self.server_id.trim().is_empty()
            || self.tool.trim().is_empty()
            || self.schema_fingerprint.trim().is_empty()
            || self.protocol_version.trim().is_empty()
        {
            return Err(TurnPipelineError::InvalidMcpResource);
        }
        if self.content.trim().is_empty() {
            return Err(TurnPipelineError::EmptyMcpContent);
        }
        if self.expires_at <= now {
            return Err(TurnPipelineError::ExpiredMcpResource);
        }
        Ok(())
    }

    fn key(&self) -> Result<String, TurnPipelineError> {
        let material = (
            self.server_id.as_str(),
            self.tool.as_str(),
            &self.input,
            self.schema_fingerprint.as_str(),
            self.protocol_version.as_str(),
        );
        let bytes = serde_json::to_vec(&material)?;
        Ok(hex::encode(Sha256::digest(bytes)))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct CachedMcpResource {
    content: String,
    expires_at: OffsetDateTime,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct McpResourceCache {
    entries: BTreeMap<String, CachedMcpResource>,
}

impl McpResourceCache {
    fn resolve(
        &mut self,
        request: &McpResourceRequest,
        now: OffsetDateTime,
    ) -> Result<(String, bool, String), TurnPipelineError> {
        request.validate(now)?;
        let key = request.key()?;
        self.entries.retain(|_, entry| entry.expires_at > now);
        if let Some(entry) = self.entries.get(&key) {
            return Ok((entry.content.clone(), true, key));
        }
        if request.cacheable && !request.sensitive {
            self.entries.insert(
                key.clone(),
                CachedMcpResource {
                    content: request.content.clone(),
                    expires_at: request.expires_at,
                },
            );
        }
        Ok((request.content.clone(), false, key))
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Clone, Debug)]
pub struct TurnRequest {
    pub task: String,
    pub system_sections: Vec<StableSection>,
    pub tool_schemas: Vec<ToolSchema>,
    pub retrieved_context: Vec<ContextItem>,
    pub memory_query: RetrievalQuery,
    pub mcp_resources: Vec<McpResourceRequest>,
    pub budget: TurnBudget,
    pub current_memory_documents: Vec<WritebackDocument>,
    pub writeback_policy: WritebackPolicy,
    pub consolidation_policy: ConsolidationPolicy,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TurnProvenance {
    pub context_ids: Vec<String>,
    pub memory_chunk_ids: Vec<String>,
    pub memory_result_fingerprint: String,
    pub mcp_cache_keys: Vec<String>,
    pub mcp_cache_hits: Vec<bool>,
    pub prompt_fingerprint: String,
}

#[derive(Clone, Debug)]
pub struct PreparedTurn {
    pub assembly: TurnAssembly,
    pub provenance: TurnProvenance,
    request: TurnRequest,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelTurnResult {
    pub response: String,
    #[serde(default)]
    pub memory_observations: Vec<MemoryObservation>,
}

pub trait TurnExecutor {
    fn execute(&mut self, prompt: &str) -> Result<ModelTurnResult, TurnPipelineError>;
}

pub trait TurnVerifier {
    fn verify(&mut self, result: &ModelTurnResult) -> Result<bool, TurnPipelineError>;
}

#[derive(Clone, Debug)]
pub struct VerifiedTurn {
    pub response: String,
    pub provenance: TurnProvenance,
    pub writeback: Option<WritebackPlan>,
}

#[derive(Clone, Debug)]
pub struct TurnPipeline {
    memory: MemoryIndex,
    mcp_cache: McpResourceCache,
}

impl TurnPipeline {
    #[must_use]
    pub fn new(memory: MemoryIndex) -> Self {
        Self {
            memory,
            mcp_cache: McpResourceCache::default(),
        }
    }

    #[must_use]
    pub fn mcp_cache(&self) -> &McpResourceCache {
        &self.mcp_cache
    }

    pub fn prepare(
        &mut self,
        mut request: TurnRequest,
        now: OffsetDateTime,
    ) -> Result<PreparedTurn, TurnPipelineError> {
        if request.task.trim().is_empty() {
            return Err(TurnPipelineError::EmptyTask);
        }
        let memory = self.memory.retrieve(&request.memory_query)?;
        let mut memory_chunk_ids = Vec::new();
        for hit in &memory.hits {
            memory_chunk_ids.push(hit.chunk.id.clone());
            request.system_sections.push(StableSection::new(
                format!("memory:{}", hit.chunk.id),
                format!(
                    "path={}\nheading={}\nrevision={}\ncontent={}",
                    hit.chunk.path, hit.chunk.heading, hit.chunk.revision, hit.chunk.body
                ),
            )?);
        }

        let mut mcp_cache_keys = Vec::new();
        let mut mcp_cache_hits = Vec::new();
        request.mcp_resources.sort_by(|left, right| {
            left.server_id
                .cmp(&right.server_id)
                .then_with(|| left.tool.cmp(&right.tool))
        });
        for resource in &request.mcp_resources {
            let (content, hit, key) = self.mcp_cache.resolve(resource, now)?;
            request.system_sections.push(StableSection::new(
                format!("mcp:{}:{}:{}", resource.server_id, resource.tool, key),
                content,
            )?);
            mcp_cache_keys.push(key);
            mcp_cache_hits.push(hit);
        }

        let assembly = TurnAssemblyInput {
            stable_sections: request.system_sections.clone(),
            tool_schemas: request.tool_schemas.clone(),
            retrieved_context: request.retrieved_context.clone(),
            current_task: request.task.clone(),
            budget: request.budget.clone(),
        }
        .assemble()?;
        let provenance = TurnProvenance {
            context_ids: assembly.included_context_ids.clone(),
            memory_chunk_ids,
            memory_result_fingerprint: memory.result_fingerprint,
            mcp_cache_keys,
            mcp_cache_hits,
            prompt_fingerprint: assembly.full_prompt_fingerprint.clone(),
        };
        Ok(PreparedTurn {
            assembly,
            provenance,
            request,
        })
    }

    pub fn execute_verified(
        &mut self,
        prepared: PreparedTurn,
        executor: &mut impl TurnExecutor,
        verifier: &mut impl TurnVerifier,
    ) -> Result<VerifiedTurn, TurnPipelineError> {
        let result = executor.execute(&prepared.assembly.rendered_prompt)?;
        if !verifier.verify(&result)? {
            return Err(TurnPipelineError::VerificationFailed);
        }
        let writeback = if result.memory_observations.is_empty() {
            None
        } else {
            let consolidated = consolidate(
                &result.memory_observations,
                prepared.request.consolidation_policy,
            )?;
            Some(plan_writeback(
                &consolidated.memories,
                &consolidated.conflicts,
                &prepared.request.current_memory_documents,
                &prepared.request.writeback_policy,
            )?)
        };
        Ok(VerifiedTurn {
            response: result.response,
            provenance: prepared.provenance,
            writeback,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TurnPipelineError {
    #[error("turn task cannot be empty")]
    EmptyTask,
    #[error("MCP resource identity fields cannot be empty")]
    InvalidMcpResource,
    #[error("MCP resource content cannot be empty")]
    EmptyMcpContent,
    #[error("MCP resource is already expired")]
    ExpiredMcpResource,
    #[error("turn verification failed; memory writeback was not staged")]
    VerificationFailed,
    #[error("turn pipeline invariant failed: {0}")]
    Invariant(&'static str),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl From<&'static str> for TurnPipelineError {
    fn from(value: &'static str) -> Self {
        Self::Invariant(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use medusa_markdown_memory::MemoryDocument;
    use medusa_memory_consolidation::MemoryKind;
    use time::{Duration, macros::datetime};

    fn memory_index() -> MemoryIndex {
        let mut index = MemoryIndex::default();
        index
            .refresh(MemoryDocument {
                path: "memory/project.md".into(),
                content: "# Runtime\nUse verified writeback after successful turns".into(),
                revision: 1,
            })
            .expect("memory refresh");
        index
    }

    fn request(content: &str, schema: &str, version: &str) -> TurnRequest {
        TurnRequest {
            task: "integrate memory".into(),
            system_sections: vec![StableSection::new("system", "Follow policy").unwrap()],
            tool_schemas: Vec::new(),
            retrieved_context: Vec::new(),
            memory_query: RetrievalQuery {
                text: "verified writeback".into(),
                maximum_results: 4,
                maximum_bytes: 4_096,
                preferred_paths: BTreeSet::new(),
                required_chunk_ids: BTreeSet::new(),
            },
            mcp_resources: vec![McpResourceRequest {
                server_id: "github".into(),
                tool: "issue".into(),
                input: serde_json::json!({"number": 304}),
                schema_fingerprint: schema.into(),
                protocol_version: version.into(),
                content: content.into(),
                expires_at: datetime!(2026-07-27 00:00 UTC),
                cacheable: true,
                sensitive: false,
            }],
            budget: TurnBudget {
                maximum_input_tokens: 16_384,
                reserved_output_tokens: 2_048,
            },
            current_memory_documents: Vec::new(),
            writeback_policy: WritebackPolicy::default(),
            consolidation_policy: ConsolidationPolicy {
                minimum_support: 1,
                ..ConsolidationPolicy::default()
            },
        }
    }

    struct Executor {
        observations: Vec<MemoryObservation>,
        calls: usize,
    }

    impl TurnExecutor for Executor {
        fn execute(&mut self, prompt: &str) -> Result<ModelTurnResult, TurnPipelineError> {
            assert!(prompt.contains("verified writeback"));
            self.calls += 1;
            Ok(ModelTurnResult {
                response: "done".into(),
                memory_observations: self.observations.clone(),
            })
        }
    }

    struct Verifier(bool);

    impl TurnVerifier for Verifier {
        fn verify(&mut self, _: &ModelTurnResult) -> Result<bool, TurnPipelineError> {
            Ok(self.0)
        }
    }

    fn observation() -> MemoryObservation {
        MemoryObservation {
            id: "obs-1".into(),
            subject: "Runtime".into(),
            predicate: "writeback".into(),
            value: "only after verification".into(),
            kind: MemoryKind::Decision,
            source: "turn".into(),
            observed_at: datetime!(2026-07-26 08:00 UTC),
            confidence_basis_points: 9_000,
        }
    }

    #[test]
    fn retrieval_assembly_execution_and_verified_writeback_are_one_pipeline() {
        let now = datetime!(2026-07-26 08:00 UTC);
        let mut pipeline = TurnPipeline::new(memory_index());
        let prepared = pipeline
            .prepare(request("issue body", "schema-a", "1"), now)
            .unwrap();
        assert_eq!(prepared.provenance.memory_chunk_ids.len(), 1);
        assert_eq!(prepared.provenance.mcp_cache_hits, vec![false]);
        let mut executor = Executor {
            observations: vec![observation()],
            calls: 0,
        };
        let verified = pipeline
            .execute_verified(prepared, &mut executor, &mut Verifier(true))
            .unwrap();
        assert_eq!(executor.calls, 1);
        assert_eq!(verified.response, "done");
        assert_eq!(verified.writeback.unwrap().patches.len(), 1);
    }

    #[test]
    fn failed_verification_never_stages_memory_writeback() {
        let now = datetime!(2026-07-26 08:00 UTC);
        let mut pipeline = TurnPipeline::new(memory_index());
        let prepared = pipeline
            .prepare(request("issue body", "schema-a", "1"), now)
            .unwrap();
        let mut executor = Executor {
            observations: vec![observation()],
            calls: 0,
        };
        assert!(matches!(
            pipeline.execute_verified(prepared, &mut executor, &mut Verifier(false)),
            Err(TurnPipelineError::VerificationFailed)
        ));
    }

    #[test]
    fn identical_mcp_requests_hit_cache_but_schema_or_version_changes_miss() {
        let now = datetime!(2026-07-26 08:00 UTC);
        let mut pipeline = TurnPipeline::new(memory_index());
        let first = pipeline
            .prepare(request("first", "schema-a", "1"), now)
            .unwrap();
        let second = pipeline
            .prepare(
                request("ignored replacement", "schema-a", "1"),
                now + Duration::minutes(1),
            )
            .unwrap();
        let schema_change = pipeline
            .prepare(
                request("schema changed", "schema-b", "1"),
                now + Duration::minutes(2),
            )
            .unwrap();
        let version_change = pipeline
            .prepare(
                request("version changed", "schema-b", "2"),
                now + Duration::minutes(3),
            )
            .unwrap();
        assert_eq!(first.provenance.mcp_cache_hits, vec![false]);
        assert_eq!(second.provenance.mcp_cache_hits, vec![true]);
        assert_eq!(schema_change.provenance.mcp_cache_hits, vec![false]);
        assert_eq!(version_change.provenance.mcp_cache_hits, vec![false]);
    }

    #[test]
    fn sensitive_or_non_cacheable_resources_are_never_persisted() {
        let now = datetime!(2026-07-26 08:00 UTC);
        let mut pipeline = TurnPipeline::new(memory_index());
        let mut sensitive = request("secret", "schema-a", "1");
        sensitive.mcp_resources[0].sensitive = true;
        pipeline.prepare(sensitive, now).unwrap();
        let mut non_cacheable = request("volatile", "schema-a", "1");
        non_cacheable.mcp_resources[0].cacheable = false;
        pipeline.prepare(non_cacheable, now).unwrap();
        assert!(pipeline.mcp_cache().is_empty());
    }
}
