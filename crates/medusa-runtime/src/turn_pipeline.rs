use medusa_context::{ContextItem, ContextKind};
use medusa_markdown_memory::{MemoryDocument as IndexedMemoryDocument, MemoryIndex, RetrievalQuery};
use medusa_mcp_cache::result_cache::{ResultCache, ResultCacheKey};
use medusa_memory_consolidation::{ConsolidatedMemory, MemoryConflict};
use medusa_memory_writeback::{
    MemoryDocument as WritebackDocument, WritebackPlan, WritebackPolicy, plan_writeback,
};
use medusa_turn_assembly::{StableSection, ToolSchema, TurnAssembly, TurnAssemblyInput, TurnBudget};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::{Duration, OffsetDateTime};

#[derive(Clone, Debug)]
pub struct McpResourceRequest {
    pub key: ResultCacheKey,
    pub ttl: Duration,
    pub sensitive: bool,
    pub cacheable: bool,
}

#[derive(Clone, Debug)]
pub struct TurnPipelineInput {
    pub stable_sections: Vec<StableSection>,
    pub tool_schemas: Vec<ToolSchema>,
    pub retrieved_context: Vec<ContextItem>,
    pub memory_documents: Vec<IndexedMemoryDocument>,
    pub memory_query: RetrievalQuery,
    pub mcp_resources: Vec<McpResourceRequest>,
    pub current_task: String,
    pub budget: TurnBudget,
    pub now: OffsetDateTime,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct McpResourceProvenance {
    pub key_fingerprint: String,
    pub cache_hit: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MemoryProvenance {
    pub chunk_ids: Vec<String>,
    pub index_fingerprint: String,
    pub query_fingerprint: String,
    pub result_fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TurnProvenance {
    pub context_ids: Vec<String>,
    pub memory: MemoryProvenance,
    pub mcp_resources: Vec<McpResourceProvenance>,
    pub stable_prefix_fingerprint: String,
    pub full_prompt_fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PreparedTurn {
    pub assembly: TurnAssembly,
    pub mcp_values: Vec<Value>,
    pub provenance: TurnProvenance,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TurnCompletion {
    VerifiedSuccess,
    Failed,
    Cancelled,
}

#[derive(Default)]
pub struct TurnPipeline {
    memory_index: MemoryIndex,
    mcp_cache: ResultCache,
}

impl TurnPipeline {
    pub fn prepare(
        &mut self,
        mut input: TurnPipelineInput,
        mut resolve_mcp: impl FnMut(&ResultCacheKey) -> Result<Value, &'static str>,
    ) -> Result<PreparedTurn, &'static str> {
        for document in input.memory_documents {
            self.memory_index.refresh(document)?;
        }
        let memory = self.memory_index.retrieve(&input.memory_query)?;
        input.retrieved_context.sort_by_key(|item| item.sequence);
        let mut sequence = input
            .retrieved_context
            .last()
            .map_or(1, |item| item.sequence.saturating_add(1));
        for hit in &memory.hits {
            input.retrieved_context.push(
                ContextItem::new(
                    format!("memory:{}", hit.chunk.id),
                    ContextKind::Observation,
                    format!("# {}\n{}", hit.chunk.heading, hit.chunk.body),
                    sequence,
                    input.now,
                )?
                .with_reference(hit.chunk.path.clone()),
            );
            sequence = sequence.saturating_add(1);
        }

        let mut mcp_values = Vec::with_capacity(input.mcp_resources.len());
        let mut mcp_provenance = Vec::with_capacity(input.mcp_resources.len());
        for request in input.mcp_resources {
            let key_fingerprint = request.key.fingerprint();
            let (value, cache_hit) = match self.mcp_cache.get(&request.key, input.now) {
                Some(value) => (value, true),
                None => {
                    let value = resolve_mcp(&request.key)?;
                    let _ = self.mcp_cache.insert(
                        request.key.clone(),
                        value.clone(),
                        input.now,
                        request.ttl,
                        request.sensitive,
                        request.cacheable,
                    )?;
                    (value, false)
                }
            };
            input.retrieved_context.push(ContextItem::new(
                format!("mcp:{key_fingerprint}"),
                ContextKind::Observation,
                serde_json::to_string(&value).map_err(|_| "MCP result serialization failed")?,
                sequence,
                input.now,
            )?);
            sequence = sequence.saturating_add(1);
            mcp_values.push(value);
            mcp_provenance.push(McpResourceProvenance {
                key_fingerprint,
                cache_hit,
            });
        }

        let assembly = TurnAssemblyInput {
            stable_sections: input.stable_sections,
            tool_schemas: input.tool_schemas,
            retrieved_context: input.retrieved_context,
            current_task: input.current_task,
            budget: input.budget,
        }
        .assemble()?;
        let context_ids = assembly.included_context_ids.clone();
        let provenance = TurnProvenance {
            context_ids,
            memory: MemoryProvenance {
                chunk_ids: memory
                    .hits
                    .iter()
                    .map(|hit| hit.chunk.id.clone())
                    .collect(),
                index_fingerprint: memory.index_fingerprint,
                query_fingerprint: memory.query_fingerprint,
                result_fingerprint: memory.result_fingerprint,
            },
            mcp_resources: mcp_provenance,
            stable_prefix_fingerprint: assembly.stable_prefix_fingerprint.clone(),
            full_prompt_fingerprint: assembly.full_prompt_fingerprint.clone(),
        };
        Ok(PreparedTurn {
            assembly,
            mcp_values,
            provenance,
        })
    }

    pub fn finish(
        &self,
        completion: TurnCompletion,
        memories: &[ConsolidatedMemory],
        conflicts: &[MemoryConflict],
        documents: &[WritebackDocument],
        policy: &WritebackPolicy,
        mut commit: impl FnMut(&WritebackPlan) -> Result<(), &'static str>,
    ) -> Result<Option<WritebackPlan>, &'static str> {
        if completion != TurnCompletion::VerifiedSuccess {
            return Ok(None);
        }
        let plan = plan_writeback(memories, conflicts, documents, policy)?;
        commit(&plan)?;
        Ok(Some(plan))
    }

    #[must_use]
    pub fn cached_mcp_results(&self) -> usize {
        self.mcp_cache.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use medusa_memory_consolidation::MemoryKind;
    use serde_json::json;
    use std::collections::{BTreeMap, BTreeSet};
    use time::macros::datetime;

    fn pipeline_input() -> TurnPipelineInput {
        let now = datetime!(2026-07-26 08:00 UTC);
        TurnPipelineInput {
            stable_sections: vec![StableSection::new("system", "Follow policy").expect("section")],
            tool_schemas: vec![],
            retrieved_context: vec![ContextItem::new(
                "goal",
                ContextKind::Goal,
                "Ship the feature",
                1,
                now,
            )
            .expect("context")],
            memory_documents: vec![IndexedMemoryDocument {
                path: "memory/project.md".into(),
                content: "# Preference\nUse Rust".into(),
                revision: 1,
            }],
            memory_query: RetrievalQuery {
                text: "Rust".into(),
                maximum_results: 3,
                maximum_bytes: 1_000,
                preferred_paths: BTreeSet::new(),
                required_chunk_ids: BTreeSet::new(),
            },
            mcp_resources: vec![McpResourceRequest {
                key: ResultCacheKey::build(
                    "github",
                    "search",
                    &json!({"q":"medusa"}),
                    "schema-1",
                    "2026-01",
                    Some("1".into()),
                )
                .expect("key"),
                ttl: Duration::minutes(5),
                sensitive: false,
                cacheable: true,
            }],
            current_task: "Implement the turn pipeline".into(),
            budget: TurnBudget {
                maximum_input_tokens: 10_000,
                reserved_output_tokens: 1_000,
            },
            now,
        }
    }

    fn memory() -> ConsolidatedMemory {
        ConsolidatedMemory {
            key: "project\u{1f}language\u{1f}Fact".into(),
            subject: "Project".into(),
            predicate: "language".into(),
            value: "Rust".into(),
            kind: MemoryKind::Fact,
            support_ids: vec!["turn-1".into()],
            confidence_basis_points: 9000,
            fingerprint: "memory-fingerprint".into(),
        }
    }

    #[test]
    fn end_to_end_retrieval_assembly_cache_and_verified_writeback() {
        let mut pipeline = TurnPipeline::default();
        let mut calls = 0;
        let first = pipeline
            .prepare(pipeline_input(), |_| {
                calls += 1;
                Ok(json!({"items":["result"]}))
            })
            .expect("first");
        assert!(first.assembly.rendered_prompt.contains("Use Rust"));
        assert_eq!(calls, 1);
        let second = pipeline
            .prepare(pipeline_input(), |_| {
                calls += 1;
                Ok(json!({"items":["result"]}))
            })
            .expect("second");
        assert_eq!(calls, 1);
        assert!(second.provenance.mcp_resources[0].cache_hit);
        assert_eq!(
            first.assembly.full_prompt_fingerprint,
            second.assembly.full_prompt_fingerprint
        );
        let mut committed = false;
        let plan = pipeline
            .finish(
                TurnCompletion::VerifiedSuccess,
                &[memory()],
                &[],
                &[],
                &WritebackPolicy {
                    default_path: "memory/MEMORY.md".into(),
                    paths_by_kind: BTreeMap::new(),
                    include_provenance: true,
                    include_conflicts: true,
                },
                |_| {
                    committed = true;
                    Ok(())
                },
            )
            .expect("finish")
            .expect("plan");
        assert!(committed);
        assert_eq!(plan.patches.len(), 1);
    }

    #[test]
    fn failed_and_cancelled_turns_do_not_write_memory() {
        let pipeline = TurnPipeline::default();
        for completion in [TurnCompletion::Failed, TurnCompletion::Cancelled] {
            let mut committed = false;
            let result = pipeline
                .finish(
                    completion,
                    &[memory()],
                    &[],
                    &[],
                    &WritebackPolicy::default(),
                    |_| {
                        committed = true;
                        Ok(())
                    },
                )
                .expect("finish");
            assert!(result.is_none());
            assert!(!committed);
        }
    }
}
