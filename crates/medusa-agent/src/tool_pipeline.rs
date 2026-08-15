use std::time::{Duration, Instant};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const TOOL_PIPELINE_VERSION: u32 = 1;

type GuardFn = dyn Fn(&ToolPipelineRequest) -> GuardDecision + Send + Sync;
type GuardEntry = (String, Box<GuardFn>);

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPipelineStage {
    Resolve,
    PreExecute,
    Guards,
    Approval,
    AroundDispatch,
    Execute,
    PostExecute,
    Finalize,
    Publish,
}

impl ToolPipelineStage {
    const REQUIRED: [Self; 9] = [
        Self::Resolve,
        Self::PreExecute,
        Self::Guards,
        Self::Approval,
        Self::AroundDispatch,
        Self::Execute,
        Self::PostExecute,
        Self::Finalize,
        Self::Publish,
    ];
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResolvedToolIdentity {
    pub capability_id: String,
    pub handler_id: String,
    pub capability_version: String,
}

impl ResolvedToolIdentity {
    #[must_use]
    pub fn built_in(name: &str) -> Self {
        Self {
            capability_id: format!("tool.{name}"),
            handler_id: format!("medusa-agent::{name}"),
            capability_version: "builtin-v1".to_owned(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "decision", content = "reason")]
pub enum GuardDecision {
    Allow,
    Deny(String),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GuardReceipt {
    pub guard_id: String,
    pub decision: GuardDecision,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcomeClass {
    Success,
    Denied,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FinalToolOutcome {
    pub pipeline_version: u32,
    pub identity: ResolvedToolIdentity,
    pub input_fingerprint: String,
    pub outcome: ToolOutcomeClass,
    pub output: Option<String>,
    pub error: Option<MedusaError>,
    pub guard_receipts: Vec<GuardReceipt>,
    pub stages: Vec<ToolPipelineStage>,
    pub elapsed_ns: u64,
}

impl FinalToolOutcome {
    pub fn into_result(self) -> MedusaResult<String> {
        match self.outcome {
            ToolOutcomeClass::Success => Ok(self.output.unwrap_or_default()),
            ToolOutcomeClass::Denied => Err(self.error.unwrap_or_else(|| {
                MedusaError::new(
                    ErrorCode::PolicyDenied,
                    ErrorCategory::Policy,
                    "tool execution denied",
                )
            })),
            ToolOutcomeClass::Cancelled | ToolOutcomeClass::Failed => {
                Err(self.error.unwrap_or_else(|| {
                    MedusaError::new(
                        ErrorCode::ToolExecutionFailed,
                        ErrorCategory::Execution,
                        "tool execution failed",
                    )
                }))
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct ToolPipelineRequest {
    pub identity: ResolvedToolIdentity,
    pub input: Value,
    pub approval_required: bool,
    pub approval_granted: bool,
}

impl ToolPipelineRequest {
    #[must_use]
    pub fn built_in(name: &str, input: &Value) -> Self {
        Self {
            identity: ResolvedToolIdentity::built_in(name),
            input: input.clone(),
            approval_required: false,
            approval_granted: false,
        }
    }
}

#[derive(Default)]
pub struct ToolExecutionPipeline {
    guards: Vec<GuardEntry>,
}

impl ToolExecutionPipeline {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_guard(
        mut self,
        id: impl Into<String>,
        guard: impl Fn(&ToolPipelineRequest) -> GuardDecision + Send + Sync + 'static,
    ) -> Self {
        self.guards.push((id.into(), Box::new(guard)));
        self
    }

    pub fn execute(
        &self,
        request: ToolPipelineRequest,
        cancellation: &std::sync::atomic::AtomicBool,
        handler: impl FnOnce(&Value) -> MedusaResult<String>,
    ) -> FinalToolOutcome {
        let started = Instant::now();
        let mut stages = Vec::with_capacity(ToolPipelineStage::REQUIRED.len());
        let mut guard_receipts = Vec::with_capacity(self.guards.len());

        stages.push(ToolPipelineStage::Resolve);
        let identity = request.identity.clone();

        stages.push(ToolPipelineStage::PreExecute);
        let input = canonicalize_json(request.input.clone());
        let input_fingerprint = stable_fingerprint(&input);

        stages.push(ToolPipelineStage::Guards);
        let mut denied = None;
        for (guard_id, guard) in &self.guards {
            let decision = if denied.is_some() {
                GuardDecision::Deny("prior monotonic guard denial".to_owned())
            } else {
                guard(&request)
            };
            if let GuardDecision::Deny(reason) = &decision {
                denied.get_or_insert_with(|| reason.clone());
            }
            guard_receipts.push(GuardReceipt {
                guard_id: guard_id.clone(),
                decision,
            });
        }

        stages.push(ToolPipelineStage::Approval);
        if denied.is_none() && request.approval_required && !request.approval_granted {
            denied = Some("tool requires explicit approval".to_owned());
        }

        stages.push(ToolPipelineStage::AroundDispatch);
        if denied.is_none() && cancellation.load(std::sync::atomic::Ordering::Acquire) {
            stages.push(ToolPipelineStage::Execute);
            stages.push(ToolPipelineStage::PostExecute);
            return finalize(FinalizeInput {
                identity,
                input_fingerprint,
                outcome: ToolOutcomeClass::Cancelled,
                output: None,
                error: Some(MedusaError::new(
                    ErrorCode::ToolExecutionFailed,
                    ErrorCategory::Execution,
                    "tool execution cancelled",
                )),
                guard_receipts,
                stages,
                elapsed: started.elapsed(),
            });
        }

        stages.push(ToolPipelineStage::Execute);
        let result = denied.map_or_else(|| handler(&input), |reason| {
            Err(MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                reason,
            ))
        });

        stages.push(ToolPipelineStage::PostExecute);
        let (outcome, output, error) = match result {
            Ok(output) => (ToolOutcomeClass::Success, Some(output), None),
            Err(error) if error.code == ErrorCode::PolicyDenied => {
                (ToolOutcomeClass::Denied, None, Some(error))
            }
            Err(error) => (ToolOutcomeClass::Failed, None, Some(error)),
        };

        finalize(FinalizeInput {
            identity,
            input_fingerprint,
            outcome,
            output,
            error,
            guard_receipts,
            stages,
            elapsed: started.elapsed(),
        })
    }
}

struct FinalizeInput {
    identity: ResolvedToolIdentity,
    input_fingerprint: String,
    outcome: ToolOutcomeClass,
    output: Option<String>,
    error: Option<MedusaError>,
    guard_receipts: Vec<GuardReceipt>,
    stages: Vec<ToolPipelineStage>,
    elapsed: Duration,
}

fn finalize(input: FinalizeInput) -> FinalToolOutcome {
    let FinalizeInput {
        identity,
        input_fingerprint,
        outcome,
        output,
        error,
        guard_receipts,
        mut stages,
        elapsed,
    } = input;
    stages.push(ToolPipelineStage::Finalize);
    stages.push(ToolPipelineStage::Publish);
    debug_assert_eq!(stages, ToolPipelineStage::REQUIRED);
    FinalToolOutcome {
        pipeline_version: TOOL_PIPELINE_VERSION,
        identity,
        input_fingerprint,
        outcome,
        output,
        error,
        guard_receipts,
        stages,
        elapsed_ns: elapsed.as_nanos().min(u128::from(u64::MAX)) as u64,
    }
}

fn canonicalize_json(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| (key, canonicalize_json(value)))
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize_json).collect()),
        scalar => scalar,
    }
}

fn stable_fingerprint(value: &Value) -> String {
    // FNV-1a is sufficient as a deterministic audit fingerprint here; this is not a credential or
    // cryptographic authorization token. Security authority remains in the guard receipts.
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use serde_json::json;

    use super::*;

    #[test]
    fn normal_execution_traverses_required_stages_exactly_once() {
        let pipeline = ToolExecutionPipeline::new().with_guard("policy", |_| GuardDecision::Allow);
        let outcome = pipeline.execute(
            ToolPipelineRequest::built_in("fs_read", &json!({"path":"src/lib.rs"})),
            &AtomicBool::new(false),
            |_| Ok("ok".to_owned()),
        );
        assert_eq!(outcome.stages, ToolPipelineStage::REQUIRED);
        assert_eq!(outcome.outcome, ToolOutcomeClass::Success);
    }

    #[test]
    fn monotonic_denial_cannot_be_restored_by_later_guard() {
        let pipeline = ToolExecutionPipeline::new()
            .with_guard("hard_policy", |_| GuardDecision::Deny("denied".to_owned()))
            .with_guard("late_allow", |_| GuardDecision::Allow);
        let called = Arc::new(AtomicUsize::new(0));
        let handler_called = Arc::clone(&called);
        let outcome = pipeline.execute(
            ToolPipelineRequest::built_in("shell_run", &json!({})),
            &AtomicBool::new(false),
            move |_| {
                handler_called.fetch_add(1, Ordering::SeqCst);
                Ok("unsafe".to_owned())
            },
        );
        assert_eq!(outcome.outcome, ToolOutcomeClass::Denied);
        assert_eq!(called.load(Ordering::SeqCst), 0);
        assert!(matches!(
            outcome.guard_receipts[1].decision,
            GuardDecision::Deny(_)
        ));
    }

    #[test]
    fn approval_never_overrides_guard_denial() {
        let pipeline = ToolExecutionPipeline::new()
            .with_guard("scope", |_| GuardDecision::Deny("outside scope".to_owned()));
        let mut request = ToolPipelineRequest::built_in("fs_write", &json!({"path":"/tmp/x"}));
        request.approval_required = true;
        request.approval_granted = true;
        let outcome = pipeline.execute(request, &AtomicBool::new(false), |_| Ok("wrote".to_owned()));
        assert_eq!(outcome.outcome, ToolOutcomeClass::Denied);
    }

    #[test]
    fn missing_approval_fails_closed_before_handler() {
        let pipeline = ToolExecutionPipeline::new();
        let mut request = ToolPipelineRequest::built_in("fs_write", &json!({"path":"/tmp/x"}));
        request.approval_required = true;
        let outcome = pipeline.execute(request, &AtomicBool::new(false), |_| Ok("wrote".to_owned()));
        assert_eq!(outcome.outcome, ToolOutcomeClass::Denied);
    }

    #[test]
    fn cancellation_has_one_terminal_normalized_outcome() {
        let called = AtomicUsize::new(0);
        let outcome = ToolExecutionPipeline::new().execute(
            ToolPipelineRequest::built_in("fs_read", &json!({"path":"x"})),
            &AtomicBool::new(true),
            |_| {
                called.fetch_add(1, Ordering::SeqCst);
                Ok("should not run".to_owned())
            },
        );
        assert_eq!(outcome.outcome, ToolOutcomeClass::Cancelled);
        assert_eq!(outcome.stages, ToolPipelineStage::REQUIRED);
        assert_eq!(called.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn canonical_input_fingerprint_is_stable_for_object_key_order() {
        let pipeline = ToolExecutionPipeline::new();
        let first = pipeline.execute(
            ToolPipelineRequest::built_in("x", &json!({"b":2,"a":1})),
            &AtomicBool::new(false),
            |_| Ok(String::new()),
        );
        let second = pipeline.execute(
            ToolPipelineRequest::built_in("x", &json!({"a":1,"b":2})),
            &AtomicBool::new(false),
            |_| Ok(String::new()),
        );
        assert_eq!(first.input_fingerprint, second.input_fingerprint);
    }

    #[test]
    fn resolved_handler_identity_is_immutable_in_final_outcome() {
        let request = ToolPipelineRequest::built_in("fs_read", &json!({"path":"x"}));
        let identity = request.identity.clone();
        let outcome = ToolExecutionPipeline::new().execute(
            request,
            &AtomicBool::new(false),
            |_| Ok("ok".to_owned()),
        );
        assert_eq!(outcome.identity, identity);
    }

    #[test]
    fn structured_handler_error_survives_finalization() {
        let mut source = MedusaError::new(
            ErrorCode::ToolExecutionFailed,
            ErrorCategory::Transient,
            "browser deadline exceeded",
        )
        .with_retryable(true);
        source
            .context
            .insert("browser_error_kind".to_owned(), json!("timeout"));
        source
            .context
            .insert("browser_sidecar_reset".to_owned(), json!(true));
        let expected = source.clone();

        let outcome = ToolExecutionPipeline::new().execute(
            ToolPipelineRequest::built_in("browser_click", &json!({"selector":"#missing"})),
            &AtomicBool::new(false),
            |_| Err(source),
        );

        assert_eq!(outcome.error.as_ref(), Some(&expected));
        assert_eq!(outcome.into_result(), Err(expected));
    }
}
