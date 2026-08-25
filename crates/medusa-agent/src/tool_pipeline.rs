use std::time::{Duration, Instant};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const TOOL_PIPELINE_VERSION: u32 = 1;

const CORE_MIDDLEWARE_OWNER: &str = "medusa-core";

type GuardFn = dyn Fn(&ToolPipelineRequest) -> GuardDecision + Send + Sync;
type GuardEntry = (String, String, Box<GuardFn>);
type PreTransformFn = dyn Fn(&Value) -> MedusaResult<Value> + Send + Sync;
type PreTransformEntry = (String, String, Box<PreTransformFn>);
type AroundHookFn = dyn Fn(&ToolPipelineRequest) -> MedusaResult<()> + Send + Sync;
type AroundHookEntry = (String, String, Box<AroundHookFn>);
type PostTransformFn = dyn Fn(String) -> MedusaResult<String> + Send + Sync;
type PostTransformEntry = (String, String, Box<PostTransformFn>);

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
    const BEFORE_FINALIZE: [Self; 7] = [
        Self::Resolve,
        Self::PreExecute,
        Self::Guards,
        Self::Approval,
        Self::AroundDispatch,
        Self::Execute,
        Self::PostExecute,
    ];
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResolvedToolIdentity {
    capability_id: String,
    handler_id: String,
    capability_version: String,
}

impl ResolvedToolIdentity {
    #[must_use]
    pub fn new(
        capability_id: impl Into<String>,
        handler_id: impl Into<String>,
        capability_version: impl Into<String>,
    ) -> Self {
        Self {
            capability_id: capability_id.into(),
            handler_id: handler_id.into(),
            capability_version: capability_version.into(),
        }
    }

    #[must_use]
    pub fn built_in(name: &str) -> Self {
        Self::new(
            format!("tool.{name}"),
            format!("medusa-agent::{name}"),
            "builtin-v1",
        )
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MiddlewareStatus {
    Passed,
    Denied,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MiddlewareReceipt {
    pub owner_id: String,
    pub middleware_id: String,
    pub stage: ToolPipelineStage,
    pub status: MiddlewareStatus,
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
    pipeline_version: u32,
    identity: ResolvedToolIdentity,
    input_fingerprint: String,
    outcome: ToolOutcomeClass,
    output: Option<String>,
    error: Option<MedusaError>,
    artifact_refs: Vec<String>,
    evidence_refs: Vec<String>,
    cancelled: bool,
    guard_receipts: Vec<GuardReceipt>,
    middleware_receipts: Vec<MiddlewareReceipt>,
    stages: Vec<ToolPipelineStage>,
    elapsed_ns: u64,
}

impl FinalToolOutcome {
    pub fn receipt_value(&self) -> MedusaResult<Value> {
        serde_json::to_value(self).map_err(|error| {
            MedusaError::new(
                ErrorCode::InternalInvariant,
                ErrorCategory::Internal,
                format!("could not serialize immutable tool execution receipt: {error}"),
            )
        })
    }

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
    pre_transforms: Vec<PreTransformEntry>,
    guards: Vec<GuardEntry>,
    around_hooks: Vec<AroundHookEntry>,
    post_transforms: Vec<PostTransformEntry>,
}

impl ToolExecutionPipeline {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_pre_transform(
        self,
        id: impl Into<String>,
        transform: impl Fn(&Value) -> MedusaResult<Value> + Send + Sync + 'static,
    ) -> Self {
        self.with_owned_pre_transform(CORE_MIDDLEWARE_OWNER, id, transform)
    }

    #[must_use]
    pub fn with_owned_pre_transform(
        mut self,
        owner: impl Into<String>,
        id: impl Into<String>,
        transform: impl Fn(&Value) -> MedusaResult<Value> + Send + Sync + 'static,
    ) -> Self {
        self.pre_transforms
            .push((owner.into(), id.into(), Box::new(transform)));
        self
    }

    #[must_use]
    pub fn with_guard(
        self,
        id: impl Into<String>,
        guard: impl Fn(&ToolPipelineRequest) -> GuardDecision + Send + Sync + 'static,
    ) -> Self {
        self.with_owned_guard(CORE_MIDDLEWARE_OWNER, id, guard)
    }

    #[must_use]
    pub fn with_owned_guard(
        mut self,
        owner: impl Into<String>,
        id: impl Into<String>,
        guard: impl Fn(&ToolPipelineRequest) -> GuardDecision + Send + Sync + 'static,
    ) -> Self {
        self.guards
            .push((owner.into(), id.into(), Box::new(guard)));
        self
    }

    #[must_use]
    pub fn with_around_hook(
        self,
        id: impl Into<String>,
        hook: impl Fn(&ToolPipelineRequest) -> MedusaResult<()> + Send + Sync + 'static,
    ) -> Self {
        self.with_owned_around_hook(CORE_MIDDLEWARE_OWNER, id, hook)
    }

    #[must_use]
    pub fn with_owned_around_hook(
        mut self,
        owner: impl Into<String>,
        id: impl Into<String>,
        hook: impl Fn(&ToolPipelineRequest) -> MedusaResult<()> + Send + Sync + 'static,
    ) -> Self {
        self.around_hooks
            .push((owner.into(), id.into(), Box::new(hook)));
        self
    }

    #[must_use]
    pub fn with_post_transform(
        self,
        id: impl Into<String>,
        transform: impl Fn(String) -> MedusaResult<String> + Send + Sync + 'static,
    ) -> Self {
        self.with_owned_post_transform(CORE_MIDDLEWARE_OWNER, id, transform)
    }

    #[must_use]
    pub fn with_owned_post_transform(
        mut self,
        owner: impl Into<String>,
        id: impl Into<String>,
        transform: impl Fn(String) -> MedusaResult<String> + Send + Sync + 'static,
    ) -> Self {
        self.post_transforms
            .push((owner.into(), id.into(), Box::new(transform)));
        self
    }

    pub fn remove_owner(&mut self, owner: &str) {
        self.pre_transforms.retain(|entry| entry.0 != owner);
        self.guards.retain(|entry| entry.0 != owner);
        self.around_hooks.retain(|entry| entry.0 != owner);
        self.post_transforms.retain(|entry| entry.0 != owner);
    }

    pub fn execute(
        &self,
        request: ToolPipelineRequest,
        cancellation: &std::sync::atomic::AtomicBool,
        handler: impl FnOnce(&Value) -> MedusaResult<String>,
    ) -> MedusaResult<FinalToolOutcome> {
        let started = Instant::now();
        let mut stages = Vec::with_capacity(ToolPipelineStage::REQUIRED.len());
        let mut guard_receipts = Vec::with_capacity(self.guards.len());
        let mut middleware_receipts = Vec::new();
        let mut terminal_error = None;
        let mut cancelled = false;

        stages.push(ToolPipelineStage::Resolve);
        let identity = request.identity.clone();

        stages.push(ToolPipelineStage::PreExecute);
        let mut input = canonicalize_json(request.input.clone());
        for (owner, middleware_id, transform) in &self.pre_transforms {
            if terminal_error.is_some() {
                middleware_receipts.push(middleware_receipt(
                    owner,
                    middleware_id,
                    ToolPipelineStage::PreExecute,
                    MiddlewareStatus::Denied,
                ));
                continue;
            }
            match transform(&input) {
                Ok(next) => {
                    let next = canonicalize_json(next);
                    if input_is_narrowing(&input, &next) {
                        input = next;
                        middleware_receipts.push(middleware_receipt(
                            owner,
                            middleware_id,
                            ToolPipelineStage::PreExecute,
                            MiddlewareStatus::Passed,
                        ));
                    } else {
                        terminal_error = Some(MedusaError::new(
                            ErrorCode::PolicyDenied,
                            ErrorCategory::Policy,
                            format!(
                                "pre-execution middleware {middleware_id} attempted to widen tool authority"
                            ),
                        ));
                        middleware_receipts.push(middleware_receipt(
                            owner,
                            middleware_id,
                            ToolPipelineStage::PreExecute,
                            MiddlewareStatus::Denied,
                        ));
                    }
                }
                Err(error) => {
                    terminal_error = Some(error);
                    middleware_receipts.push(middleware_receipt(
                        owner,
                        middleware_id,
                        ToolPipelineStage::PreExecute,
                        MiddlewareStatus::Failed,
                    ));
                }
            }
        }
        let input_fingerprint = stable_fingerprint(&input)?;

        stages.push(ToolPipelineStage::Guards);
        for (owner, guard_id, guard) in &self.guards {
            let decision = if terminal_error.is_some() {
                GuardDecision::Deny("prior monotonic denial".to_owned())
            } else {
                guard(&ToolPipelineRequest {
                    identity: identity.clone(),
                    input: input.clone(),
                    approval_required: request.approval_required,
                    approval_granted: request.approval_granted,
                })
            };
            let status = match &decision {
                GuardDecision::Allow => MiddlewareStatus::Passed,
                GuardDecision::Deny(reason) => {
                    if terminal_error.is_none() {
                        terminal_error = Some(MedusaError::new(
                            ErrorCode::PolicyDenied,
                            ErrorCategory::Policy,
                            reason.clone(),
                        ));
                    }
                    MiddlewareStatus::Denied
                }
            };
            guard_receipts.push(GuardReceipt {
                guard_id: guard_id.clone(),
                decision,
            });
            middleware_receipts.push(middleware_receipt(
                owner,
                guard_id,
                ToolPipelineStage::Guards,
                status,
            ));
        }

        stages.push(ToolPipelineStage::Approval);
        if terminal_error.is_none() && request.approval_required && !request.approval_granted {
            terminal_error = Some(MedusaError::new(
                ErrorCode::PolicyDenied,
                ErrorCategory::Policy,
                "tool requires explicit approval",
            ));
        }

        stages.push(ToolPipelineStage::AroundDispatch);
        for (owner, middleware_id, hook) in &self.around_hooks {
            if terminal_error.is_some() {
                middleware_receipts.push(middleware_receipt(
                    owner,
                    middleware_id,
                    ToolPipelineStage::AroundDispatch,
                    MiddlewareStatus::Denied,
                ));
                continue;
            }
            match hook(&ToolPipelineRequest {
                identity: identity.clone(),
                input: input.clone(),
                approval_required: request.approval_required,
                approval_granted: request.approval_granted,
            }) {
                Ok(()) => middleware_receipts.push(middleware_receipt(
                    owner,
                    middleware_id,
                    ToolPipelineStage::AroundDispatch,
                    MiddlewareStatus::Passed,
                )),
                Err(error) => {
                    terminal_error = Some(error);
                    middleware_receipts.push(middleware_receipt(
                        owner,
                        middleware_id,
                        ToolPipelineStage::AroundDispatch,
                        MiddlewareStatus::Failed,
                    ));
                }
            }
        }
        if terminal_error.is_none()
            && cancellation.load(std::sync::atomic::Ordering::Acquire)
        {
            cancelled = true;
            terminal_error = Some(cancelled_error());
        }

        stages.push(ToolPipelineStage::Execute);
        let mut result = terminal_error.map_or_else(|| handler(&input), Err);

        stages.push(ToolPipelineStage::PostExecute);
        for (owner, middleware_id, transform) in &self.post_transforms {
            if result.is_err() {
                break;
            }
            result = result.and_then(|output| match transform(output) {
                Ok(next) => {
                    middleware_receipts.push(middleware_receipt(
                        owner,
                        middleware_id,
                        ToolPipelineStage::PostExecute,
                        MiddlewareStatus::Passed,
                    ));
                    Ok(next)
                }
                Err(error) => {
                    middleware_receipts.push(middleware_receipt(
                        owner,
                        middleware_id,
                        ToolPipelineStage::PostExecute,
                        MiddlewareStatus::Failed,
                    ));
                    Err(error)
                }
            });
        }

        let (outcome, output, error) = match result {
            Ok(output) => (ToolOutcomeClass::Success, Some(output), None),
            Err(error) if cancelled => (ToolOutcomeClass::Cancelled, None, Some(error)),
            Err(error) if error.code == ErrorCode::PolicyDenied => {
                (ToolOutcomeClass::Denied, None, Some(error))
            }
            Err(error) => (ToolOutcomeClass::Failed, None, Some(error)),
        };

        Ok(finalize(FinalizeInput {
            identity,
            input_fingerprint,
            outcome,
            output,
            error,
            artifact_refs: Vec::new(),
            evidence_refs: Vec::new(),
            cancelled,
            guard_receipts,
            middleware_receipts,
            stages,
            elapsed: started.elapsed(),
        }))
    }
}

struct FinalizeInput {
    identity: ResolvedToolIdentity,
    input_fingerprint: String,
    outcome: ToolOutcomeClass,
    output: Option<String>,
    error: Option<MedusaError>,
    artifact_refs: Vec<String>,
    evidence_refs: Vec<String>,
    cancelled: bool,
    guard_receipts: Vec<GuardReceipt>,
    middleware_receipts: Vec<MiddlewareReceipt>,
    stages: Vec<ToolPipelineStage>,
    elapsed: Duration,
}

fn finalize(input: FinalizeInput) -> FinalToolOutcome {
    let FinalizeInput {
        identity,
        input_fingerprint,
        mut outcome,
        mut output,
        mut error,
        artifact_refs,
        evidence_refs,
        cancelled,
        guard_receipts,
        middleware_receipts,
        mut stages,
        elapsed,
    } = input;
    if stages != ToolPipelineStage::BEFORE_FINALIZE {
        outcome = ToolOutcomeClass::Failed;
        output = None;
        error = Some(MedusaError::new(
            ErrorCode::InternalInvariant,
            ErrorCategory::Internal,
            format!("tool pipeline stage invariant failed before finalization: {stages:?}"),
        ));
    }
    stages.push(ToolPipelineStage::Finalize);
    stages.push(ToolPipelineStage::Publish);
    FinalToolOutcome {
        pipeline_version: TOOL_PIPELINE_VERSION,
        identity,
        input_fingerprint,
        outcome,
        output,
        error,
        artifact_refs,
        evidence_refs,
        cancelled,
        guard_receipts,
        middleware_receipts,
        stages,
        elapsed_ns: elapsed.as_nanos().min(u128::from(u64::MAX)) as u64,
    }
}

fn middleware_receipt(
    owner: &str,
    middleware_id: &str,
    stage: ToolPipelineStage,
    status: MiddlewareStatus,
) -> MiddlewareReceipt {
    MiddlewareReceipt {
        owner_id: owner.to_owned(),
        middleware_id: middleware_id.to_owned(),
        stage,
        status,
    }
}

fn cancelled_error() -> MedusaError {
    let mut error = MedusaError::new(
        ErrorCode::ToolExecutionFailed,
        ErrorCategory::Execution,
        "tool execution cancelled",
    );
    error
        .context
        .insert("cancelled".to_owned(), Value::Bool(true));
    error
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

fn input_is_narrowing(original: &Value, candidate: &Value) -> bool {
    match (original, candidate) {
        (Value::Object(original), Value::Object(candidate)) => candidate.iter().all(|(key, value)| {
            original
                .get(key)
                .is_some_and(|original_value| input_is_narrowing(original_value, value))
        }),
        (Value::Array(original), Value::Array(candidate)) => candidate
            .iter()
            .all(|value| original.iter().any(|original_value| original_value == value)),
        _ => original == candidate,
    }
}

fn stable_fingerprint(value: &Value) -> MedusaResult<String> {
    // FNV-1a is sufficient as a deterministic audit fingerprint here; this is not a credential or
    // cryptographic authorization token. Security authority remains in the guard receipts.
    let bytes = serde_json::to_vec(value)?;
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    Ok(format!("fnv1a64:{hash:016x}"))
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
        )
        .expect("pipeline");
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
        )
        .expect("pipeline");
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
        let outcome = pipeline
            .execute(request, &AtomicBool::new(false), |_| Ok("wrote".to_owned()))
            .expect("pipeline");
        assert_eq!(outcome.outcome, ToolOutcomeClass::Denied);
    }

    #[test]
    fn missing_approval_fails_closed_before_handler() {
        let pipeline = ToolExecutionPipeline::new();
        let mut request = ToolPipelineRequest::built_in("fs_write", &json!({"path":"/tmp/x"}));
        request.approval_required = true;
        let outcome = pipeline
            .execute(request, &AtomicBool::new(false), |_| Ok("wrote".to_owned()))
            .expect("pipeline");
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
        )
        .expect("pipeline");
        assert_eq!(outcome.outcome, ToolOutcomeClass::Cancelled);
        assert!(outcome.cancelled);
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
        )
        .expect("pipeline");
        let second = pipeline.execute(
            ToolPipelineRequest::built_in("x", &json!({"a":1,"b":2})),
            &AtomicBool::new(false),
            |_| Ok(String::new()),
        )
        .expect("pipeline");
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
        )
        .expect("pipeline");
        assert_eq!(outcome.identity, identity);
    }

    #[test]
    fn pre_transform_may_narrow_but_cannot_widen_input_scope() {
        let narrowed = ToolExecutionPipeline::new()
            .with_pre_transform("drop_optional", |input| {
                Ok(json!({"path": input["path"].clone()}))
            })
            .execute(
                ToolPipelineRequest::built_in(
                    "fs_read",
                    &json!({"path":"src/lib.rs","optional":true}),
                ),
                &AtomicBool::new(false),
                |_| Ok("ok".to_owned()),
            )
            .expect("pipeline");
        assert_eq!(narrowed.outcome, ToolOutcomeClass::Success);

        let widened = ToolExecutionPipeline::new()
            .with_pre_transform("widen", |_| {
                Ok(json!({"path":"src/lib.rs","network":true}))
            })
            .execute(
                ToolPipelineRequest::built_in("fs_read", &json!({"path":"src/lib.rs"})),
                &AtomicBool::new(false),
                |_| Ok("must not run".to_owned()),
            )
            .expect("pipeline");
        assert_eq!(widened.outcome, ToolOutcomeClass::Denied);
    }

    #[test]
    fn hook_faults_are_normalized_and_cannot_publish_false_success() {
        let pre = ToolExecutionPipeline::new()
            .with_pre_transform("fault", |_| {
                Err(MedusaError::new(
                    ErrorCode::ToolExecutionFailed,
                    ErrorCategory::Internal,
                    "pre hook fault",
                ))
            })
            .execute(
                ToolPipelineRequest::built_in("x", &json!({})),
                &AtomicBool::new(false),
                |_| Ok("must not run".to_owned()),
            )
            .expect("pipeline");
        assert_eq!(pre.outcome, ToolOutcomeClass::Failed);

        let around = ToolExecutionPipeline::new()
            .with_around_hook("fault", |_| {
                Err(MedusaError::new(
                    ErrorCode::ToolExecutionFailed,
                    ErrorCategory::Internal,
                    "around hook fault",
                ))
            })
            .execute(
                ToolPipelineRequest::built_in("x", &json!({})),
                &AtomicBool::new(false),
                |_| Ok("must not run".to_owned()),
            )
            .expect("pipeline");
        assert_eq!(around.outcome, ToolOutcomeClass::Failed);

        let post = ToolExecutionPipeline::new()
            .with_post_transform("fault", |_| {
                Err(MedusaError::new(
                    ErrorCode::ToolExecutionFailed,
                    ErrorCategory::Internal,
                    "post hook fault",
                ))
            })
            .execute(
                ToolPipelineRequest::built_in("x", &json!({})),
                &AtomicBool::new(false),
                |_| Ok("handler success".to_owned()),
            )
            .expect("pipeline");
        assert_eq!(post.outcome, ToolOutcomeClass::Failed);
        assert!(post.output.is_none());
    }

    #[test]
    fn unloading_owner_removes_every_owned_middleware_registration() {
        let mut pipeline = ToolExecutionPipeline::new()
            .with_owned_pre_transform("plugin-a", "pre", |input| Ok(input.clone()))
            .with_owned_guard("plugin-a", "guard", |_| GuardDecision::Deny("deny".to_owned()))
            .with_owned_around_hook("plugin-a", "around", |_| Ok(()))
            .with_owned_post_transform("plugin-a", "post", Ok);
        pipeline.remove_owner("plugin-a");
        let outcome = pipeline.execute(
            ToolPipelineRequest::built_in("x", &json!({})),
            &AtomicBool::new(false),
            |_| Ok("ok".to_owned()),
        )
        .expect("pipeline");
        assert_eq!(outcome.outcome, ToolOutcomeClass::Success);
        assert!(outcome.middleware_receipts.is_empty());
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
        )
        .expect("pipeline");

        assert_eq!(outcome.error.as_ref(), Some(&expected));
        assert_eq!(outcome.into_result(), Err(expected));
    }

    #[test]
    fn immutable_receipt_is_serialized_before_result_consumption() {
        let outcome = ToolExecutionPipeline::new().execute(
            ToolPipelineRequest::built_in("fs_read", &json!({"path":"x"})),
            &AtomicBool::new(false),
            |_| Ok("ok".to_owned()),
        )
        .expect("pipeline");
        let receipt = outcome.receipt_value().expect("receipt");
        assert_eq!(receipt["pipeline_version"], json!(TOOL_PIPELINE_VERSION));
        assert_eq!(receipt["outcome"], json!("success"));
        assert_eq!(outcome.into_result().expect("result"), "ok");
    }

    #[test]
    fn malformed_stage_sequence_fails_closed_in_finalization() {
        let outcome = finalize(FinalizeInput {
            identity: ResolvedToolIdentity::built_in("fs_read"),
            input_fingerprint: "fixture".to_owned(),
            outcome: ToolOutcomeClass::Success,
            output: Some("must not escape".to_owned()),
            error: None,
            artifact_refs: Vec::new(),
            evidence_refs: Vec::new(),
            cancelled: false,
            guard_receipts: Vec::new(),
            middleware_receipts: Vec::new(),
            stages: vec![ToolPipelineStage::Resolve, ToolPipelineStage::Execute],
            elapsed: Duration::ZERO,
        });
        assert_eq!(outcome.outcome, ToolOutcomeClass::Failed);
        assert!(outcome.output.is_none());
        assert_eq!(
            outcome.error.as_ref().map(|error| error.code),
            Some(ErrorCode::InternalInvariant)
        );
    }
}
