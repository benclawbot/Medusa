use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};

use crate::{
    ModelProvider, ModelRequest, ModelResponse, ProviderRouteProfile, ProviderStreamEvent,
};

const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug)]
pub(crate) struct HedgeCandidateOutcome {
    pub index: usize,
    pub duration_ms: u64,
    pub first_token_ms: Option<u64>,
    pub output_started: bool,
    pub result: MedusaResult<ModelResponse>,
}

#[derive(Debug)]
pub(crate) struct HedgeRaceOutcome {
    pub authoritative_index: Option<usize>,
    pub primary: HedgeCandidateOutcome,
    pub secondary: Option<HedgeCandidateOutcome>,
}

enum CandidateMessage {
    Event {
        index: usize,
        event: ProviderStreamEvent,
    },
    Finished(HedgeCandidateOutcome),
}

/// Races one primary request against at most one delayed secondary request.
///
/// Candidate stream events are buffered independently until a candidate emits an authoritative
/// output signal. At that instant only the winner's buffered events are released to the caller and
/// the losing request is cooperatively cancelled. A non-streaming successful response becomes
/// authoritative when it completes.
pub(crate) fn race_provider_candidates<P: ModelProvider + Sync>(
    providers: &[P],
    profiles: &[ProviderRouteProfile],
    request: &ModelRequest,
    primary_index: usize,
    secondary_index: usize,
    launch_after_ms: u64,
    outer_cancel: Option<&AtomicBool>,
    sink: &mut Option<&mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>>,
) -> MedusaResult<HedgeRaceOutcome> {
    let primary = providers.get(primary_index).ok_or_else(|| {
        race_error(format!("hedge primary provider {primary_index} is missing"))
    })?;
    let secondary = providers.get(secondary_index).ok_or_else(|| {
        race_error(format!("hedge secondary provider {secondary_index} is missing"))
    })?;
    let primary_streaming = route_streaming(profiles, primary_index, primary);
    let secondary_streaming = route_streaming(profiles, secondary_index, secondary);
    let launch_after = Duration::from_millis(launch_after_ms.max(1));
    let primary_cancel = Arc::new(AtomicBool::new(false));
    let secondary_cancel = Arc::new(AtomicBool::new(false));

    thread::scope(|scope| {
        let (tx, rx) = mpsc::channel::<CandidateMessage>();
        let primary_cancel_for_worker = Arc::clone(&primary_cancel);
        let primary_tx = tx.clone();
        scope.spawn(move || {
            run_candidate(
                primary,
                request,
                primary_index,
                primary_streaming,
                &primary_cancel_for_worker,
                primary_tx,
            );
        });

        let race_started = Instant::now();
        let mut secondary_started = false;
        let mut primary_outcome = None;
        let mut secondary_outcome = None;
        let mut authoritative_index = None;
        let mut buffers = BTreeMap::<usize, Vec<ProviderStreamEvent>>::new();

        loop {
            if outer_cancel.is_some_and(|cancel| cancel.load(Ordering::SeqCst)) {
                primary_cancel.store(true, Ordering::SeqCst);
                secondary_cancel.store(true, Ordering::SeqCst);
                return Err(crate::cancelled_provider_error());
            }

            if !secondary_started
                && authoritative_index.is_none()
                && primary_outcome.is_none()
                && race_started.elapsed() >= launch_after
            {
                secondary_started = true;
                let secondary_cancel_for_worker = Arc::clone(&secondary_cancel);
                let secondary_tx = tx.clone();
                scope.spawn(move || {
                    run_candidate(
                        secondary,
                        request,
                        secondary_index,
                        secondary_streaming,
                        &secondary_cancel_for_worker,
                        secondary_tx,
                    );
                });
            }

            let timeout = if !secondary_started && authoritative_index.is_none() {
                launch_after
                    .saturating_sub(race_started.elapsed())
                    .min(CANCEL_POLL_INTERVAL)
                    .max(Duration::from_millis(1))
            } else {
                CANCEL_POLL_INTERVAL
            };

            match rx.recv_timeout(timeout) {
                Ok(CandidateMessage::Event { index, event }) => {
                    if authoritative_index == Some(index) {
                        forward_event(sink, event)?;
                        continue;
                    }
                    if authoritative_index.is_some() {
                        continue;
                    }
                    let authoritative = is_authoritative_event(&event);
                    buffers.entry(index).or_default().push(event);
                    if authoritative {
                        authoritative_index = Some(index);
                        cancel_loser(
                            index,
                            primary_index,
                            &primary_cancel,
                            &secondary_cancel,
                        );
                        flush_candidate_events(sink, &mut buffers, index)?;
                    }
                }
                Ok(CandidateMessage::Finished(outcome)) => {
                    let index = outcome.index;
                    let successful = outcome.result.is_ok();
                    if index == primary_index {
                        primary_outcome = Some(outcome);
                    } else if index == secondary_index {
                        secondary_outcome = Some(outcome);
                    }

                    if authoritative_index.is_none() && successful {
                        authoritative_index = Some(index);
                        cancel_loser(
                            index,
                            primary_index,
                            &primary_cancel,
                            &secondary_cancel,
                        );
                        flush_candidate_events(sink, &mut buffers, index)?;
                        let streaming = if index == primary_index {
                            primary_streaming
                        } else {
                            secondary_streaming
                        };
                        if !streaming {
                            let response = if index == primary_index {
                                primary_outcome
                                    .as_ref()
                                    .and_then(|candidate| candidate.result.as_ref().ok())
                            } else {
                                secondary_outcome
                                    .as_ref()
                                    .and_then(|candidate| candidate.result.as_ref().ok())
                            };
                            if let Some(response) = response {
                                forward_event(
                                    sink,
                                    ProviderStreamEvent::Completed {
                                        response: response.clone(),
                                    },
                                )?;
                            }
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(race_error("hedge candidate channel disconnected"));
                }
            }

            if !secondary_started {
                if let Some(primary) = primary_outcome.as_ref()
                    && primary.result.is_err()
                    && authoritative_index.is_none()
                {
                    return Ok(HedgeRaceOutcome {
                        authoritative_index,
                        primary: primary_outcome.take().expect("primary outcome checked"),
                        secondary: None,
                    });
                }
            }

            let winner_finished = authoritative_index.is_some_and(|index| {
                if index == primary_index {
                    primary_outcome.is_some()
                } else {
                    secondary_outcome.is_some()
                }
            });
            let loser_finished = if !secondary_started {
                true
            } else {
                primary_outcome.is_some() && secondary_outcome.is_some()
            };
            let both_failed = secondary_started
                && authoritative_index.is_none()
                && primary_outcome
                    .as_ref()
                    .is_some_and(|candidate| candidate.result.is_err())
                && secondary_outcome
                    .as_ref()
                    .is_some_and(|candidate| candidate.result.is_err());

            if (winner_finished && loser_finished) || both_failed {
                return Ok(HedgeRaceOutcome {
                    authoritative_index,
                    primary: primary_outcome
                        .take()
                        .ok_or_else(|| race_error("hedge primary produced no outcome"))?,
                    secondary: secondary_outcome.take(),
                });
            }
        }
    })
}

fn run_candidate<P: ModelProvider>(
    provider: &P,
    request: &ModelRequest,
    index: usize,
    streaming: bool,
    cancel: &AtomicBool,
    tx: Sender<CandidateMessage>,
) {
    let started = Instant::now();
    let mut first_token_ms = None;
    let mut output_started = false;
    let result = if streaming {
        let mut candidate_sink = |event: ProviderStreamEvent| {
            if matches!(event, ProviderStreamEvent::OutputStarted) && first_token_ms.is_none() {
                first_token_ms = Some(elapsed_ms(started));
                output_started = true;
            }
            tx.send(CandidateMessage::Event { index, event })
                .map_err(|_| race_error("hedge coordinator stopped receiving provider events"))
        };
        provider.complete_streaming_cancellable(request, cancel, &mut candidate_sink)
    } else {
        provider.complete_cancellable(request, cancel)
    };
    let _ = tx.send(CandidateMessage::Finished(HedgeCandidateOutcome {
        index,
        duration_ms: elapsed_ms(started),
        first_token_ms,
        output_started,
        result,
    }));
}

fn route_streaming<P: ModelProvider>(
    profiles: &[ProviderRouteProfile],
    index: usize,
    provider: &P,
) -> bool {
    profiles.get(index).is_some_and(|profile| profile.streaming)
        && provider.capabilities().streaming
}

fn is_authoritative_event(event: &ProviderStreamEvent) -> bool {
    matches!(
        event,
        ProviderStreamEvent::OutputStarted
            | ProviderStreamEvent::TextDelta { .. }
            | ProviderStreamEvent::ToolUseReady { .. }
            | ProviderStreamEvent::Completed { .. }
    )
}

fn cancel_loser(
    winner: usize,
    primary_index: usize,
    primary_cancel: &AtomicBool,
    secondary_cancel: &AtomicBool,
) {
    if winner == primary_index {
        secondary_cancel.store(true, Ordering::SeqCst);
    } else {
        primary_cancel.store(true, Ordering::SeqCst);
    }
}

fn flush_candidate_events(
    sink: &mut Option<&mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>>,
    buffers: &mut BTreeMap<usize, Vec<ProviderStreamEvent>>,
    index: usize,
) -> MedusaResult<()> {
    for event in buffers.remove(&index).unwrap_or_default() {
        forward_event(sink, event)?;
    }
    buffers.clear();
    Ok(())
}

fn forward_event(
    sink: &mut Option<&mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>>,
    event: ProviderStreamEvent,
) -> MedusaResult<()> {
    if let Some(sink) = sink.as_deref_mut() {
        sink(event)?;
    }
    Ok(())
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn race_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::InternalInvariant,
        ErrorCategory::Internal,
        message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    use crate::{ProviderCapabilities, ResponseBlock, RouteRetryPolicy, Usage};

    #[derive(Clone)]
    struct DelayedProvider {
        id: &'static str,
        delay: Duration,
        calls: Arc<AtomicUsize>,
        cancellations: Arc<AtomicUsize>,
        fail: bool,
    }

    impl ModelProvider for DelayedProvider {
        fn complete(&self, request: &ModelRequest) -> MedusaResult<ModelResponse> {
            let cancel = AtomicBool::new(false);
            self.complete_cancellable(request, &cancel)
        }

        fn complete_streaming_cancellable(
            &self,
            _request: &ModelRequest,
            cancel: &AtomicBool,
            sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,
        ) -> MedusaResult<ModelResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            sink(ProviderStreamEvent::ResponseStarted {
                response_id: Some(self.id.to_owned()),
            })?;
            let started = Instant::now();
            while started.elapsed() < self.delay {
                if cancel.load(Ordering::SeqCst) {
                    self.cancellations.fetch_add(1, Ordering::SeqCst);
                    return Err(crate::cancelled_provider_error());
                }
                thread::sleep(Duration::from_millis(1));
            }
            if self.fail {
                return Err(race_error(format!("{} failed", self.id)));
            }
            sink(ProviderStreamEvent::OutputStarted)?;
            let response = response(self.id);
            sink(ProviderStreamEvent::Completed {
                response: response.clone(),
            })?;
            Ok(response)
        }

        fn complete_cancellable(
            &self,
            _request: &ModelRequest,
            cancel: &AtomicBool,
        ) -> MedusaResult<ModelResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let started = Instant::now();
            while started.elapsed() < self.delay {
                if cancel.load(Ordering::SeqCst) {
                    self.cancellations.fetch_add(1, Ordering::SeqCst);
                    return Err(crate::cancelled_provider_error());
                }
                thread::sleep(Duration::from_millis(1));
            }
            if self.fail {
                Err(race_error(format!("{} failed", self.id)))
            } else {
                Ok(response(self.id))
            }
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                streaming: true,
                tool_calling: true,
                ..ProviderCapabilities::default()
            }
        }
    }

    fn provider(id: &'static str, delay_ms: u64) -> DelayedProvider {
        DelayedProvider {
            id,
            delay: Duration::from_millis(delay_ms),
            calls: Arc::new(AtomicUsize::new(0)),
            cancellations: Arc::new(AtomicUsize::new(0)),
            fail: false,
        }
    }

    fn profile(id: &str) -> ProviderRouteProfile {
        ProviderRouteProfile {
            id: id.to_owned(),
            provider: id.to_owned(),
            model: "model".to_owned(),
            protocol: "test".to_owned(),
            endpoint: None,
            auth_source: "test".to_owned(),
            tool_calling: true,
            streaming: true,
            retry: RouteRetryPolicy::default(),
        }
    }

    fn request() -> ModelRequest {
        ModelRequest {
            system: "test".to_owned(),
            messages: Vec::new(),
            tools: Vec::new(),
            max_tokens: 1,
            temperature_milli: 0,
        }
    }

    fn response(id: &str) -> ModelResponse {
        ModelResponse {
            response_id: Some(id.to_owned()),
            stop_reason: Some("end_turn".to_owned()),
            blocks: vec![ResponseBlock::Text {
                text: id.to_owned(),
            }],
            usage: Usage::default(),
        }
    }

    #[test]
    fn primary_completion_before_threshold_never_launches_secondary() {
        let primary = provider("primary", 2);
        let secondary = provider("secondary", 2);
        let secondary_calls = Arc::clone(&secondary.calls);
        let providers = vec![primary, secondary];
        let profiles = vec![profile("primary"), profile("secondary")];
        let mut events = Vec::new();
        let mut record = |event| {
            events.push(event);
            Ok(())
        };
        let mut sink: Option<&mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>> =
            Some(&mut record);
        let outcome = race_provider_candidates(
            &providers,
            &profiles,
            &request(),
            0,
            1,
            20,
            None,
            &mut sink,
        )
        .expect("race");
        assert_eq!(outcome.authoritative_index, Some(0));
        assert!(outcome.secondary.is_none());
        assert_eq!(secondary_calls.load(Ordering::SeqCst), 0);
        assert!(events.iter().any(|event| matches!(
            event,
            ProviderStreamEvent::ResponseStarted { response_id: Some(id) } if id == "primary"
        )));
    }

    #[test]
    fn secondary_can_win_without_exposing_primary_candidate_output() {
        let primary = provider("primary", 100);
        let secondary = provider("secondary", 5);
        let primary_cancellations = Arc::clone(&primary.cancellations);
        let providers = vec![primary, secondary];
        let profiles = vec![profile("primary"), profile("secondary")];
        let mut events = Vec::new();
        let mut record = |event| {
            events.push(event);
            Ok(())
        };
        let mut sink: Option<&mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>> =
            Some(&mut record);
        let outcome = race_provider_candidates(
            &providers,
            &profiles,
            &request(),
            0,
            1,
            20,
            None,
            &mut sink,
        )
        .expect("race");
        assert_eq!(outcome.authoritative_index, Some(1));
        assert_eq!(primary_cancellations.load(Ordering::SeqCst), 1);
        assert!(events.iter().all(|event| !matches!(
            event,
            ProviderStreamEvent::ResponseStarted { response_id: Some(id) } if id == "primary"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            ProviderStreamEvent::ResponseStarted { response_id: Some(id) } if id == "secondary"
        )));
    }
}
