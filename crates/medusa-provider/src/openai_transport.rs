use std::{
    io::Read,
    sync::{atomic::AtomicBool, mpsc},
    thread,
};

use medusa_core::{ErrorCategory, ErrorCode, MedusaError, MedusaResult};
use reqwest::{Client as AsyncClient, blocking::Client as BlockingClient};
use serde_json::Value;

use crate::{
    ModelResponse, OpenAiStreamAccumulator, ProviderStreamEvent, async_response_error,
    blocking_response_error, plan_usage::observe_provider_plan_headers, provider_error,
    run_cancellable_request,
};

const READ_BUFFER_BYTES: usize = 8 * 1024;

pub(crate) fn complete_blocking(
    client: &BlockingClient,
    endpoint: &str,
    api_key: Option<&str>,
    body: Value,
    sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,
) -> MedusaResult<ModelResponse> {
    let mut builder = client.post(endpoint).json(&body);
    if let Some(key) = api_key {
        builder = builder.bearer_auth(key);
    }
    let mut response = builder.send().map_err(provider_error)?;
    if !response.status().is_success() {
        return Err(blocking_response_error(response));
    }
    let _ = observe_provider_plan_headers(response.headers());

    let mut decoder = SseDecoder::default();
    let mut accumulator = OpenAiStreamAccumulator::default();
    let mut completed = None;
    let mut buffer = [0_u8; READ_BUFFER_BYTES];
    loop {
        let read = response
            .read(&mut buffer)
            .map_err(|error| stream_error(format!("OpenAI SSE read failed: {error}")))?;
        if read == 0 {
            break;
        }
        decoder.push(&buffer[..read], |data| {
            if let Some(response) = accumulator.push_sse_data(data, sink)? {
                completed = Some(response);
            }
            Ok(())
        })?;
    }
    decoder.finish(|data| {
        if let Some(response) = accumulator.push_sse_data(data, sink)? {
            completed = Some(response);
        }
        Ok(())
    })?;
    if completed.is_none() {
        completed = accumulator.finish_at_eof(sink)?;
    }
    completed.ok_or_else(|| stream_error("OpenAI SSE stream ended without [DONE]"))
}

pub(crate) fn complete_cancellable(
    client: &AsyncClient,
    endpoint: &str,
    api_key: Option<&str>,
    body: Value,
    cancel: &AtomicBool,
    sink: &mut dyn FnMut(ProviderStreamEvent) -> MedusaResult<()>,
) -> MedusaResult<ModelResponse> {
    let (sender, receiver) = mpsc::channel::<ProviderStreamEvent>();
    thread::scope(|scope| {
        let worker_sender = sender.clone();
        let worker = scope.spawn(move || {
            run_cancellable_request(cancel, async {
                let mut builder = client.post(endpoint).json(&body);
                if let Some(key) = api_key {
                    builder = builder.bearer_auth(key);
                }
                let mut response = builder.send().await.map_err(provider_error)?;
                if !response.status().is_success() {
                    return Err(async_response_error(response).await);
                }
                let _ = observe_provider_plan_headers(response.headers());

                let mut decoder = SseDecoder::default();
                let mut accumulator = OpenAiStreamAccumulator::default();
                let mut completed = None;
                while let Some(chunk) = response.chunk().await.map_err(provider_error)? {
                    decoder.push(&chunk, |data| {
                        let mut channel_sink = |event| {
                            worker_sender
                                .send(event)
                                .map_err(|_| stream_error("OpenAI stream consumer disconnected"))
                        };
                        if let Some(response) =
                            accumulator.push_sse_data(data, &mut channel_sink)?
                        {
                            completed = Some(response);
                        }
                        Ok(())
                    })?;
                }
                decoder.finish(|data| {
                    let mut channel_sink = |event| {
                        worker_sender
                            .send(event)
                            .map_err(|_| stream_error("OpenAI stream consumer disconnected"))
                    };
                    if let Some(response) = accumulator.push_sse_data(data, &mut channel_sink)? {
                        completed = Some(response);
                    }
                    Ok(())
                })?;
                if completed.is_none() {
                    let mut channel_sink = |event| {
                        worker_sender
                            .send(event)
                            .map_err(|_| stream_error("OpenAI stream consumer disconnected"))
                    };
                    completed = accumulator.finish_at_eof(&mut channel_sink)?;
                }
                completed.ok_or_else(|| stream_error("OpenAI SSE stream ended without [DONE]"))
            })
        });

        drop(sender);
        for event in receiver {
            sink(event)?;
        }
        worker
            .join()
            .map_err(|_| stream_error("OpenAI streaming worker panicked"))?
    })
}

#[derive(Debug, Default)]
struct SseDecoder {
    pending: Vec<u8>,
    data: String,
}

impl SseDecoder {
    fn push(
        &mut self,
        bytes: &[u8],
        mut sink: impl FnMut(&str) -> MedusaResult<()>,
    ) -> MedusaResult<()> {
        self.pending.extend_from_slice(bytes);
        while let Some(newline) = self.pending.iter().position(|byte| *byte == b'\n') {
            let mut line = self.pending.drain(..=newline).collect::<Vec<_>>();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.process_line(&line, &mut sink)?;
        }
        Ok(())
    }

    fn finish(&mut self, mut sink: impl FnMut(&str) -> MedusaResult<()>) -> MedusaResult<()> {
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            self.process_line(&line, &mut sink)?;
        }
        self.dispatch(&mut sink)
    }

    fn process_line(
        &mut self,
        line: &[u8],
        sink: &mut impl FnMut(&str) -> MedusaResult<()>,
    ) -> MedusaResult<()> {
        let line = std::str::from_utf8(line)
            .map_err(|error| stream_error(format!("OpenAI SSE line is not UTF-8: {error}")))?;
        if line.is_empty() {
            return self.dispatch(sink);
        }
        if let Some(value) = line.strip_prefix("data:") {
            if !self.data.is_empty() {
                self.data.push('\n');
            }
            self.data.push_str(value.strip_prefix(' ').unwrap_or(value));
        }
        Ok(())
    }

    fn dispatch(&mut self, sink: &mut impl FnMut(&str) -> MedusaResult<()>) -> MedusaResult<()> {
        if self.data.is_empty() {
            return Ok(());
        }
        let data = std::mem::take(&mut self.data);
        sink(&data)
    }
}

fn stream_error(message: impl Into<String>) -> MedusaError {
    MedusaError::new(
        ErrorCode::DependencyUnavailable,
        ErrorCategory::Execution,
        message.into(),
    )
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read as _, Write as _},
        net::TcpListener,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc::channel,
        },
        time::Duration,
    };

    use super::*;

    #[test]
    fn cancellable_stream_completion_returns_after_done() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let address = listener.local_addr().expect("address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            let body = concat!(
                "data: {\"id\":\"stream-1\",\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n"
            );
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("response");
        });

        let client = AsyncClient::builder().http1_only().build().expect("client");
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_for_worker = Arc::clone(&cancel);
        let (done_sender, done_receiver) = channel();
        thread::spawn(move || {
            let mut events = Vec::new();
            let result = complete_cancellable(
                &client,
                &format!("http://{address}/chat/completions"),
                None,
                serde_json::json!({"model":"MiniMax-M3","messages":[],"stream":true}),
                &cancel_for_worker,
                &mut |event| {
                    events.push(event);
                    Ok(())
                },
            )
            .map(|response| (response, events));
            let _ = done_sender.send(result);
        });

        let (response, events) = done_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("stream completion should not wait on the sender")
            .expect("stream request");
        assert_eq!(response.blocks.len(), 1);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ProviderStreamEvent::Completed { .. }))
        );
        cancel.store(true, Ordering::SeqCst);
        server.join().expect("server");
    }

    #[test]
    fn decoder_handles_fragmented_crlf_and_multiline_data() {
        let mut decoder = SseDecoder::default();
        let mut seen = Vec::new();
        decoder
            .push(b"data: {\"a\":1}\r", |data| {
                seen.push(data.to_owned());
                Ok(())
            })
            .expect("first fragment");
        decoder
            .push(b"\ndata: tail\r\n\r\n", |data| {
                seen.push(data.to_owned());
                Ok(())
            })
            .expect("second fragment");
        assert_eq!(seen, vec!["{\"a\":1}\ntail"]);
    }
}
