use crate::config::{JResult, ModelChoice};
use crate::tools::Tool;
use crate::types::ModelProtocol;
use reqwest::blocking::Client;
use serde_json::{json, Map, Value};
use std::io::{BufRead, BufReader, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const MAX_ATTEMPTS: u64 = 6;
const ANTHROPIC_VERSION: &str = "2023-06-01";
const MAX_STREAM_BLOCKS: usize = 64;
const MAX_MODEL_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_SSE_LINE_BYTES: usize = 1024 * 1024;
const MAX_SSE_EVENT_BYTES: usize = 2 * 1024 * 1024;
const RETRY_BASE: Duration = Duration::from_millis(250);

fn is_cancelled(cancelled: Option<&AtomicBool>) -> bool {
    cancelled.is_some_and(|token| token.load(Ordering::SeqCst))
}

fn ensure_active(cancelled: Option<&AtomicBool>) -> JResult<()> {
    if is_cancelled(cancelled) {
        return Err("MODEL_CANCELLED".into());
    }
    Ok(())
}

fn pause_before_retry(attempt: u64, cancelled: Option<&AtomicBool>) -> JResult<()> {
    let base = crate::net::retry_base(crate::env::MODEL_RETRY_BASE_MS, RETRY_BASE);
    let delay = crate::net::retry_delay(base, attempt, crate::net::RETRY_CAP);
    let deadline = Instant::now() + delay;
    while Instant::now() < deadline {
        ensure_active(cancelled)?;
        std::thread::sleep(Duration::from_millis(50).min(delay));
    }
    ensure_active(cancelled)
}

struct CancelledReader<'a, R> {
    inner: R,
    cancelled: Option<&'a AtomicBool>,
}

impl<'a, R> CancelledReader<'a, R> {
    fn new(inner: R, cancelled: Option<&'a AtomicBool>) -> Self {
        Self { inner, cancelled }
    }
}

impl<R: Read> Read for CancelledReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        ensure_active(self.cancelled).map_err(|error| std::io::Error::other(error.to_string()))?;
        self.inner.read(buffer)
    }
}

impl<R: BufRead> BufRead for CancelledReader<'_, R> {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        ensure_active(self.cancelled).map_err(|error| std::io::Error::other(error.to_string()))?;
        self.inner.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        self.inner.consume(amount);
    }
}

fn read_bounded_text<R: Read>(reader: R, cancelled: Option<&AtomicBool>) -> JResult<String> {
    read_bounded_text_with_limit(reader, cancelled, MAX_MODEL_BODY_BYTES)
}

fn read_bounded_text_with_limit<R: Read>(
    reader: R,
    cancelled: Option<&AtomicBool>,
    max_bytes: usize,
) -> JResult<String> {
    let limited = reader.take(max_bytes as u64 + 1);
    let mut bytes = Vec::new();
    CancelledReader::new(limited, cancelled)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    ensure_active(cancelled)?;
    if bytes.len() > max_bytes {
        return Err("model response exceeds byte limit".into());
    }
    String::from_utf8(bytes).map_err(|_| "model response is not UTF-8".into())
}

#[derive(Debug)]
pub struct ModelReply {
    pub message: Value,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

/// The OpenAI-shaped tool-call object every codec emits. Anthropic `tool_use`
/// blocks and both stream assemblers must produce the identical structure so
/// the reply fed back to the model is indistinguishable across providers.
fn openai_tool_call(id: &str, name: &str, kind: &str, arguments: String) -> Value {
    json!({
        "id": id,
        "type": kind,
        "function": {"name": name, "arguments": arguments},
    })
}

/// The OpenAI-shaped assistant message every codec emits; `tool_calls` appears
/// only when calls exist, matching what providers send back.
fn assistant_message(text: String, calls: Vec<Value>) -> Value {
    let mut message = Map::new();
    message.insert("role".into(), json!("assistant"));
    message.insert("content".into(), json!(text));
    if !calls.is_empty() {
        message.insert("tool_calls".into(), Value::Array(calls));
    }
    Value::Object(message)
}

#[derive(Clone, Debug, PartialEq)]
pub enum ModelDelta {
    Text(String),
    Reasoning(String),
}

pub struct ChatModel {
    client: Client,
    endpoint: String,
    model: String,
    key: String,
    protocol: ModelProtocol,
    max_output_tokens: usize,
}

impl ChatModel {
    pub fn new(choice: &ModelChoice) -> JResult<Self> {
        let key = choice.api_key().map(str::to_string).ok_or_else(|| {
            format!(
                "set JANI_API_KEY, OPENAI_API_KEY, or a settings apiKey/apiKeyEnv to call model '{}'",
                choice.model
            )
        })?;
        let client = crate::http::client_builder()
            .timeout(crate::http::REQUEST_TIMEOUT)
            .build()
            .map_err(|e| e.to_string())?;
        let base = choice.api_base.trim_end_matches('/');
        let endpoint = match choice.protocol {
            ModelProtocol::OpenAi => format!("{base}/chat/completions"),
            ModelProtocol::Anthropic => {
                if base.ends_with("/v1") {
                    format!("{base}/messages")
                } else {
                    format!("{base}/v1/messages")
                }
            }
        };
        Ok(Self {
            client,
            endpoint,
            model: choice.model.clone(),
            key,
            protocol: choice.protocol,
            max_output_tokens: choice.max_output_tokens,
        })
    }

    fn send(&self, request: &Value) -> JResult<reqwest::blocking::Response> {
        let mut http = self.client.post(&self.endpoint);
        http = match self.protocol {
            ModelProtocol::OpenAi => http.bearer_auth(&self.key),
            ModelProtocol::Anthropic => http
                .header("x-api-key", &self.key)
                .header("anthropic-version", ANTHROPIC_VERSION),
        };
        http.json(request)
            .send()
            .map_err(|e| format!("model request failed: {e}"))
    }

    /// `tool_choice` is "auto" for normal turns and "none" for the closing
    /// handoff of a turn, where the model must answer in words. The tool
    /// definitions stay in the request because providers reject a history that
    /// contains tool calls when no tools are declared.
    pub fn complete_with(
        &self,
        messages: &[Value],
        tools: &[Value],
        tool_choice: &str,
        cancelled: Option<&AtomicBool>,
    ) -> JResult<ModelReply> {
        ensure_active(cancelled)?;
        let request = match self.protocol {
            ModelProtocol::OpenAi => {
                json!({"model": self.model, "messages": messages, "tools": tools, "tool_choice": tool_choice})
            }
            ModelProtocol::Anthropic => {
                let mut request =
                    anthropic_request(&self.model, self.max_output_tokens, messages, tools)?;
                if tool_choice == "none" && !tools.is_empty() {
                    if let Some(object) = request.as_object_mut() {
                        object.insert("tool_choice".into(), json!({"type": "none"}));
                    }
                }
                request
            }
        };
        let mut attempt = 0u64;
        loop {
            ensure_active(cancelled)?;
            attempt += 1;
            let resp = match self.send(&request) {
                Ok(resp) => resp,
                Err(_) if attempt < MAX_ATTEMPTS => {
                    pause_before_retry(attempt, cancelled)?;
                    continue;
                }
                Err(error) => {
                    ensure_active(cancelled)?;
                    return Err(format!("{error} after {attempt} attempts"));
                }
            };
            let status = resp.status();
            let body = match read_bounded_text(resp, cancelled) {
                Ok(body) => body,
                Err(_error) if is_cancelled(cancelled) => return Err("MODEL_CANCELLED".into()),
                Err(_) if attempt < MAX_ATTEMPTS => {
                    pause_before_retry(attempt, cancelled)?;
                    continue;
                }
                Err(error) => {
                    return Err(format!(
                        "model response read failed after {attempt} attempts: {error}"
                    ));
                }
            };
            ensure_active(cancelled)?;
            if status.is_success() {
                let parsed = match self.protocol {
                    ModelProtocol::OpenAi => serde_json::from_str::<Value>(&body)
                        .map_err(|e| format!("invalid model JSON: {e}"))
                        .and_then(|value| openai_reply(&value)),
                    ModelProtocol::Anthropic => serde_json::from_str::<Value>(&body)
                        .map_err(|e| format!("invalid model JSON: {e}"))
                        .and_then(|value| anthropic_reply(&value)),
                };
                match parsed {
                    Ok(reply) => {
                        ensure_active(cancelled)?;
                        return Ok(reply);
                    }
                    Err(_) if attempt < MAX_ATTEMPTS => {
                        pause_before_retry(attempt, cancelled)?;
                        continue;
                    }
                    Err(error) => {
                        ensure_active(cancelled)?;
                        return Err(format!("{error} after {attempt} attempts"));
                    }
                }
            }
            let retryable = matches!(status.as_u16(), 408 | 429 | 500 | 502 | 503 | 504);
            if retryable && attempt < MAX_ATTEMPTS {
                pause_before_retry(attempt, cancelled)?;
                continue;
            }
            let excerpt = crate::process::truncate(&body, 900);
            return Err(format!(
                "model HTTP {} ({} {}): {excerpt}",
                status.as_u16(),
                self.protocol.as_str(),
                self.endpoint
            ));
        }
    }

    pub fn complete_streaming(
        &self,
        messages: &[Value],
        tools: &[Value],
        on_delta: &mut dyn FnMut(ModelDelta) -> JResult<()>,
        cancelled: Option<&AtomicBool>,
    ) -> JResult<ModelReply> {
        ensure_active(cancelled)?;
        let request = match self.protocol {
            ModelProtocol::OpenAi => json!({
                "model": self.model,
                "messages": messages,
                "tools": tools,
                "tool_choice": "auto",
                "stream": true,
                "stream_options": {"include_usage": true},
            }),
            ModelProtocol::Anthropic => {
                let mut request =
                    anthropic_request(&self.model, self.max_output_tokens, messages, tools)?;
                if let Some(object) = request.as_object_mut() {
                    object.insert("stream".into(), json!(true));
                }
                request
            }
        };
        let mut attempt = 0u64;
        loop {
            ensure_active(cancelled)?;
            attempt += 1;
            let resp = match self.send(&request) {
                Ok(resp) => resp,
                Err(_) if attempt < MAX_ATTEMPTS => {
                    pause_before_retry(attempt, cancelled)?;
                    continue;
                }
                Err(error) => {
                    ensure_active(cancelled)?;
                    return Err(format!("{error} after {attempt} attempts"));
                }
            };
            let status = resp.status();
            if status.is_success() {
                let reader = BufReader::new(CancelledReader::new(resp, cancelled));
                let mut emitted = false;
                let outcome = {
                    let mut delta = |delta: ModelDelta| {
                        ensure_active(cancelled)?;
                        emitted = true;
                        on_delta(delta)
                    };
                    match self.protocol {
                        ModelProtocol::OpenAi => openai_stream(reader, &mut delta),
                        ModelProtocol::Anthropic => anthropic_stream(reader, &mut delta),
                    }
                };
                match outcome {
                    Ok(reply) => {
                        ensure_active(cancelled)?;
                        return Ok(reply);
                    }
                    Err(_error) if is_cancelled(cancelled) => {
                        return Err("MODEL_CANCELLED".into());
                    }
                    Err(_) if !emitted && attempt < MAX_ATTEMPTS => {
                        pause_before_retry(attempt, cancelled)?;
                        continue;
                    }
                    Err(error) if emitted => {
                        return Err(format!(
                            "{error} (stream interrupted after output began; not retried)"
                        ));
                    }
                    Err(error) => {
                        return Err(format!("{error} after {attempt} attempts"));
                    }
                }
            }
            let body = match read_bounded_text(resp, cancelled) {
                Ok(body) => body,
                Err(_error) if is_cancelled(cancelled) => return Err("MODEL_CANCELLED".into()),
                Err(_) if attempt < MAX_ATTEMPTS => {
                    pause_before_retry(attempt, cancelled)?;
                    continue;
                }
                Err(error) => {
                    return Err(format!(
                        "model response read failed after {attempt} attempts: {error}"
                    ));
                }
            };
            ensure_active(cancelled)?;
            let retryable = matches!(status.as_u16(), 408 | 429 | 500 | 502 | 503 | 504);
            if retryable && attempt < MAX_ATTEMPTS {
                pause_before_retry(attempt, cancelled)?;
                continue;
            }
            let excerpt = crate::process::truncate(&body, 900);
            return Err(format!(
                "model HTTP {} ({} {}): {excerpt}",
                status.as_u16(),
                self.protocol.as_str(),
                self.endpoint
            ));
        }
    }
}

fn openai_reply(value: &Value) -> JResult<ModelReply> {
    let raw = value
        .pointer("/choices/0/message")
        .ok_or("response missing choices[0].message")?;
    let text = raw
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let calls = raw
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|call| {
                    let id = call.get("id").and_then(Value::as_str).unwrap_or("");
                    let kind = call
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("function");
                    let name = call
                        .pointer("/function/name")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let arguments = call
                        .pointer("/function/arguments")
                        .and_then(Value::as_str)
                        .unwrap_or("{}")
                        .to_string();
                    openai_tool_call(id, name, kind, arguments)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut message = assistant_message(text, calls);
    if let Some(reasoning) = raw.get("reasoning_content").and_then(Value::as_str) {
        if let Some(obj) = message.as_object_mut() {
            obj.insert("reasoning_content".into(), Value::String(reasoning.into()));
        }
    }
    let prompt_tokens = value
        .pointer("/usage/prompt_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let completion_tokens = value
        .pointer("/usage/completion_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Ok(ModelReply {
        message,
        prompt_tokens,
        completion_tokens,
    })
}

fn anthropic_reply(value: &Value) -> JResult<ModelReply> {
    let blocks = value
        .get("content")
        .and_then(Value::as_array)
        .ok_or("response missing content blocks")?;
    let mut text = String::new();
    let mut calls = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => text.push_str(block.get("text").and_then(Value::as_str).unwrap_or("")),
            Some("tool_use") => {
                let id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or("tool_use block missing id")?;
                if !crate::types::valid_id(id, 128) {
                    return Err("tool_use block has invalid id".into());
                }
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or("tool_use block missing name")?;
                let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
                calls.push(openai_tool_call(
                    id,
                    name,
                    "function",
                    serde_json::to_string(&input).map_err(|e| e.to_string())?,
                ));
            }
            _ => {}
        }
    }
    let message = assistant_message(text, calls);
    let prompt_tokens = value
        .pointer("/usage/input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let completion_tokens = value
        .pointer("/usage/output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Ok(ModelReply {
        message,
        prompt_tokens,
        completion_tokens,
    })
}

fn read_sse<R: BufRead>(reader: R, on_data: impl FnMut(&str) -> JResult<()>) -> JResult<()> {
    read_sse_with_limits(
        reader,
        on_data,
        MAX_SSE_LINE_BYTES,
        MAX_SSE_EVENT_BYTES,
        MAX_MODEL_BODY_BYTES,
    )
}

fn read_sse_with_limits<R: BufRead>(
    mut reader: R,
    mut on_data: impl FnMut(&str) -> JResult<()>,
    max_line_bytes: usize,
    max_event_bytes: usize,
    max_stream_bytes: usize,
) -> JResult<()> {
    let mut bytes = Vec::new();
    let mut data = String::new();
    let mut total = 0usize;
    loop {
        bytes.clear();
        let read = (&mut reader)
            .take(max_line_bytes as u64 + 1)
            .read_until(b'\n', &mut bytes)
            .map_err(|e| format!("model stream read failed: {e}"))?;
        if read == 0 {
            if !data.is_empty() {
                on_data(&data)?;
            }
            return Ok(());
        }
        if bytes.len() > max_line_bytes {
            return Err("model stream line exceeds byte limit".into());
        }
        total = total.saturating_add(bytes.len());
        if total > max_stream_bytes {
            return Err("model stream exceeds byte limit".into());
        }
        let line = std::str::from_utf8(&bytes)
            .map_err(|_| "model stream is not UTF-8".to_string())?
            .to_string();
        let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
        if trimmed.is_empty() {
            if !data.is_empty() {
                on_data(&data)?;
                data.clear();
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("data:") {
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            let additional = rest.len() + usize::from(!data.is_empty());
            if data.len().saturating_add(additional) > max_event_bytes {
                return Err("model stream event exceeds byte limit".into());
            }
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest);
        }
    }
}

#[derive(Default)]
struct CallAccum {
    id: Option<String>,
    kind: Option<String>,
    name: String,
    arguments: String,
}

fn openai_stream<R: BufRead>(
    reader: R,
    on_delta: &mut dyn FnMut(ModelDelta) -> JResult<()>,
) -> JResult<ModelReply> {
    let mut text = String::new();
    let mut calls: Vec<CallAccum> = Vec::new();
    let mut prompt_tokens = 0u64;
    let mut completion_tokens = 0u64;
    let mut saw_chunk = false;
    let mut done = false;
    read_sse(reader, |data| {
        if data.trim() == "[DONE]" {
            done = true;
            return Ok(());
        }
        let value: Value =
            serde_json::from_str(data).map_err(|e| format!("invalid model stream JSON: {e}"))?;
        if let Some(error) = value.get("error") {
            return Err(format!("model stream error: {error}"));
        }
        saw_chunk = true;
        if let Some(usage) = value.get("usage").filter(|usage| !usage.is_null()) {
            prompt_tokens = usage
                .get("prompt_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(prompt_tokens);
            completion_tokens = usage
                .get("completion_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(completion_tokens);
        }
        let Some(delta) = value.pointer("/choices/0/delta") else {
            return Ok(());
        };
        if let Some(reasoning) = delta.get("reasoning_content").and_then(Value::as_str) {
            if !reasoning.is_empty() {
                on_delta(ModelDelta::Reasoning(reasoning.to_string()))?;
            }
        }
        if let Some(content) = delta.get("content").and_then(Value::as_str) {
            if !content.is_empty() {
                text.push_str(content);
                on_delta(ModelDelta::Text(content.to_string()))?;
            }
        }
        if let Some(streamed) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in streamed {
                let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                if index >= MAX_STREAM_BLOCKS {
                    return Err("model stream returned too many tool calls".into());
                }
                while calls.len() <= index {
                    calls.push(CallAccum::default());
                }
                let accumulated = &mut calls[index];
                if let Some(id) = call.get("id").and_then(Value::as_str) {
                    accumulated.id = Some(id.to_string());
                }
                if let Some(kind) = call.get("type").and_then(Value::as_str) {
                    accumulated.kind = Some(kind.to_string());
                }
                if let Some(function) = call.get("function") {
                    if let Some(name) = function.get("name").and_then(Value::as_str) {
                        accumulated.name.push_str(name);
                    }
                    if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                        accumulated.arguments.push_str(arguments);
                    }
                }
            }
        }
        Ok(())
    })?;
    if !saw_chunk {
        return Err("model stream produced no chunks".into());
    }
    if !done {
        return Err("model stream ended before [DONE]; the reply is incomplete".into());
    }
    let mut assembled = Vec::new();
    for call in calls {
        if call.name.is_empty() {
            continue;
        }
        let id = call.id.ok_or("streamed tool call is missing its id")?;
        assembled.push(openai_tool_call(
            &id,
            &call.name,
            &call.kind.unwrap_or_else(|| "function".into()),
            if call.arguments.is_empty() {
                "{}".to_string()
            } else {
                call.arguments
            },
        ));
    }
    Ok(ModelReply {
        message: assistant_message(text, assembled),
        prompt_tokens,
        completion_tokens,
    })
}

#[derive(Default)]
struct BlockAccum {
    kind: String,
    text: String,
    reasoning: String,
    id: Option<String>,
    name: Option<String>,
    partial_json: String,
}

fn anthropic_stream<R: BufRead>(
    reader: R,
    on_delta: &mut dyn FnMut(ModelDelta) -> JResult<()>,
) -> JResult<ModelReply> {
    let mut blocks: Vec<BlockAccum> = Vec::new();
    let mut prompt_tokens = 0u64;
    let mut completion_tokens = 0u64;
    let mut saw_event = false;
    let mut stopped = false;
    read_sse(reader, |data| {
        let value: Value =
            serde_json::from_str(data).map_err(|e| format!("invalid model stream JSON: {e}"))?;
        match value.get("type").and_then(Value::as_str).unwrap_or("") {
            "message_start" => {
                saw_event = true;
                prompt_tokens = value
                    .pointer("/message/usage/input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(prompt_tokens);
            }
            "content_block_start" => {
                saw_event = true;
                let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                if index >= MAX_STREAM_BLOCKS {
                    return Err("model stream returned too many content blocks".into());
                }
                while blocks.len() <= index {
                    blocks.push(BlockAccum::default());
                }
                let block = &mut blocks[index];
                let started = value.get("content_block").cloned().unwrap_or_default();
                block.kind = started
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if let Some(text) = started.get("text").and_then(Value::as_str) {
                    if !text.is_empty() {
                        block.text.push_str(text);
                        on_delta(ModelDelta::Text(text.to_string()))?;
                    }
                }
                if let Some(thinking) = started.get("thinking").and_then(Value::as_str) {
                    if !thinking.is_empty() {
                        block.reasoning.push_str(thinking);
                        on_delta(ModelDelta::Reasoning(thinking.to_string()))?;
                    }
                }
                block.id = started
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                block.name = started
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                if let Some(input) = started.get("input") {
                    let present = input.as_object().is_some_and(|object| !object.is_empty());
                    if present {
                        block.partial_json =
                            serde_json::to_string(input).map_err(|e| e.to_string())?;
                    }
                }
            }
            "content_block_delta" => {
                let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let block = blocks
                    .get_mut(index)
                    .ok_or("model stream delta arrived before its content block")?;
                let delta = value.get("delta").cloned().unwrap_or_default();
                match delta.get("type").and_then(Value::as_str).unwrap_or("") {
                    "text_delta" => {
                        if let Some(text) = delta.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                block.text.push_str(text);
                                on_delta(ModelDelta::Text(text.to_string()))?;
                            }
                        }
                    }
                    "thinking_delta" => {
                        if let Some(thinking) = delta.get("thinking").and_then(Value::as_str) {
                            if !thinking.is_empty() {
                                block.reasoning.push_str(thinking);
                                on_delta(ModelDelta::Reasoning(thinking.to_string()))?;
                            }
                        }
                    }
                    "input_json_delta" => {
                        if let Some(partial) = delta.get("partial_json").and_then(Value::as_str) {
                            block.partial_json.push_str(partial);
                        }
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                saw_event = true;
                if let Some(output) = value
                    .pointer("/usage/output_tokens")
                    .and_then(Value::as_u64)
                {
                    completion_tokens = output;
                }
            }
            "message_stop" => stopped = true,
            "error" => {
                let message = value
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown stream error");
                return Err(format!("model stream error: {message}"));
            }
            _ => {}
        }
        Ok(())
    })?;
    if !saw_event {
        return Err("model stream produced no events".into());
    }
    if !stopped {
        return Err("model stream ended before message_stop; the reply is incomplete".into());
    }
    let mut text = String::new();
    let mut calls = Vec::new();
    for block in blocks {
        match block.kind.as_str() {
            "text" => text.push_str(&block.text),
            "tool_use" => {
                let id = block
                    .id
                    .ok_or("streamed tool_use block is missing its id")?;
                let name = block
                    .name
                    .ok_or("streamed tool_use block is missing its name")?;
                let input: Value = if block.partial_json.trim().is_empty() {
                    json!({})
                } else {
                    serde_json::from_str(&block.partial_json)
                        .map_err(|e| format!("invalid streamed tool input: {e}"))?
                };
                calls.push(openai_tool_call(
                    &id,
                    &name,
                    "function",
                    serde_json::to_string(&input).map_err(|e| e.to_string())?,
                ));
            }
            _ => {}
        }
    }
    Ok(ModelReply {
        message: assistant_message(text, calls),
        prompt_tokens,
        completion_tokens,
    })
}

fn anthropic_request(
    model: &str,
    max_tokens: usize,
    messages: &[Value],
    tools: &[Value],
) -> JResult<Value> {
    let mut system = String::new();
    let mut turns: Vec<(String, Vec<Value>)> = Vec::new();
    for message in messages {
        let role = message.get("role").and_then(Value::as_str).unwrap_or("");
        match role {
            "system" => {
                if let Some(content) = message.get("content").and_then(Value::as_str) {
                    if !system.is_empty() {
                        system.push_str("\n\n");
                    }
                    system.push_str(content);
                }
            }
            "user" => push_blocks(
                &mut turns,
                "user",
                vec![json!({
                    "type": "text",
                    "text": message.get("content").and_then(Value::as_str).unwrap_or(""),
                })],
            ),
            "assistant" => {
                let mut blocks = Vec::new();
                if let Some(content) = message.get("content").and_then(Value::as_str) {
                    if !content.is_empty() {
                        blocks.push(json!({"type": "text", "text": content}));
                    }
                }
                if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                    for call in calls {
                        let id = call
                            .get("id")
                            .and_then(Value::as_str)
                            .ok_or("tool call missing id")?;
                        let function = call.get("function").ok_or("tool call missing function")?;
                        let name = function
                            .get("name")
                            .and_then(Value::as_str)
                            .ok_or("tool call missing function.name")?;
                        let arguments = function
                            .get("arguments")
                            .and_then(Value::as_str)
                            .unwrap_or("{}");
                        let input: Value =
                            serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
                        blocks.push(json!({
                            "type": "tool_use",
                            "id": id,
                            "name": name,
                            "input": input,
                        }));
                    }
                }
                if !blocks.is_empty() {
                    push_blocks(&mut turns, "assistant", blocks);
                }
            }
            "tool" => {
                let id = message
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .ok_or("tool result missing tool_call_id")?;
                push_blocks(
                    &mut turns,
                    "user",
                    vec![json!({
                        "type": "tool_result",
                        "tool_use_id": id,
                        "content": message.get("content").and_then(Value::as_str).unwrap_or(""),
                    })],
                );
            }
            _ => {}
        }
    }

    let mut request = Map::new();
    request.insert("model".into(), json!(model));
    request.insert("max_tokens".into(), json!(max_tokens));
    if !system.is_empty() {
        request.insert("system".into(), json!(system));
    }
    request.insert(
        "messages".into(),
        Value::Array(
            turns
                .into_iter()
                .map(|(role, blocks)| json!({"role": role, "content": blocks}))
                .collect(),
        ),
    );
    if !tools.is_empty() {
        request.insert("tools".into(), Value::Array(anthropic_tools(tools)));
        request.insert("tool_choice".into(), json!({"type": "auto"}));
    }
    Ok(Value::Object(request))
}

fn push_blocks(turns: &mut Vec<(String, Vec<Value>)>, role: &str, blocks: Vec<Value>) {
    if let Some((last_role, last_blocks)) = turns.last_mut() {
        if last_role == role {
            last_blocks.extend(blocks);
            return;
        }
    }
    turns.push((role.to_string(), blocks));
}

fn anthropic_tools(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .filter_map(|tool| {
            let function = tool.get("function")?;
            let name = function.get("name")?.as_str()?;
            Some(json!({
                "name": name,
                "description": function.get("description").and_then(Value::as_str).unwrap_or(""),
                "input_schema": function.get("parameters").cloned().unwrap_or_else(|| json!({"type": "object"})),
            }))
        })
        .collect()
}

fn spec_for(tool: Tool) -> (&'static str, Value) {
    match tool {
        Tool::SkillRead => (
            "Read a discovered SKILL.md for workflow guidance",
            json!({"type":"object","properties":{"name":{"type":"string"}},"required":["name"],"additionalProperties":false}),
        ),
        Tool::List => (
            "List non-sensitive files in the assigned worktree",
            json!({"type":"object","properties":{"directory":{"type":"string","description":"relative directory or '.'"}},"required":["directory"],"additionalProperties":false}),
        ),
        Tool::Read => (
            "Read a UTF-8 source file from the assigned worktree",
            json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}),
        ),
        Tool::Write => (
            "Write one source file within the operator-approved edit scope",
            json!({"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"},"idempotency_key":{"type":"string"}},"required":["path","content"],"additionalProperties":false}),
        ),
        Tool::NativeTool => (
            "Call one of Jani's 12 native indexing, context, planning, pruning, checkpoint, verification, impact, scheduling, workspace, evidence, optimization or simulation primitives. Never executes arbitrary tests or commits external effects.",
            json!({"type":"object","properties":{
                "tool":{"type":"string"},
                "operation":{"type":"string"},
                "args":{"type":"array","items":{"type":"string"}},
                "idempotency_key":{"type":"string"}
            },"required":["tool","operation","args"],"additionalProperties":false}),
        ),
        Tool::Command => (
            "Run one command as an argv list (no shell: no pipes, &&, redirection or globbing; one program per call, working directory is the worktree). What runs depends on how the operator started Jani: a fixed list of test and git commands, or in smart mode read-only inspection (ls, grep, rg, find, git status...) and, inside a sandbox, project build and test commands run directly, while anything else is put to the operator, who may decline. A refusal or decline is final: do not retry a variant of the same command; continue without it or say what you need.",
            json!({"type":"object","properties":{"argv":{"type":"array","items":{"type":"string"}},"timeout_secs":{"type":"integer","minimum":1,"maximum":crate::exec::MAX_TIMEOUT_SECS},"idempotency_key":{"type":"string"}},"required":["argv","timeout_secs"],"additionalProperties":false}),
        ),
        Tool::Plugin => (
            "Invoke an administrator-approved, cryptographically signed connector or sidecar plugin capability inside an isolated sandbox with zero-secret credential injection.",
            json!({"type":"object","properties":{
                "plugin_id":{"type":"string"},
                "capability":{"type":"string"},
                "arguments":{"type":"object"},
                "idempotency_key":{"type":"string"}
            },"required":["plugin_id","capability"],"additionalProperties":false}),
        ),
    }
}

pub fn specs() -> Vec<Value> {
    Tool::ALL
        .iter()
        .map(|tool| {
            let (description, parameters) = spec_for(*tool);
            function(tool.name(), description, parameters)
        })
        .collect()
}

pub fn tool_schema(name: &str) -> Option<Value> {
    for spec in specs() {
        if spec.pointer("/function/name").and_then(Value::as_str) == Some(name) {
            return spec.pointer("/function/parameters").cloned();
        }
    }
    None
}

fn function(name: &str, description: &str, parameters: Value) -> Value {
    json!({"type":"function","function":{"name":name,"description":description,"parameters":parameters}})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": name,
                "description": "do a thing",
                "parameters": {"type": "object", "properties": {"path": {"type": "string"}}},
            },
        })
    }

    #[test]
    fn anthropic_request_translates_system_tool_calls_and_results() {
        let messages = vec![
            json!({"role": "system", "content": "policy"}),
            json!({"role": "user", "content": "read it"}),
            json!({"role": "assistant", "content": "", "tool_calls": [{
                "id": "toolu_1",
                "type": "function",
                "function": {"name": "read", "arguments": "{\"path\":\"src/main.rs\"}"},
            }]}),
            json!({"role": "tool", "tool_call_id": "toolu_1", "content": "fn main() {}"}),
        ];
        let request =
            anthropic_request("claude-sonnet-5-5", 8192, &messages, &[tool("read")]).unwrap();
        assert_eq!(request["model"], "claude-sonnet-5-5");
        assert_eq!(request["max_tokens"], 8192);
        assert_eq!(request["system"], "policy");
        assert_eq!(request["messages"][0]["role"], "user");
        assert_eq!(request["messages"][1]["role"], "assistant");
        assert_eq!(request["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(
            request["messages"][1]["content"][0]["input"]["path"],
            "src/main.rs"
        );
        assert_eq!(request["messages"][2]["role"], "user");
        assert_eq!(request["messages"][2]["content"][0]["type"], "tool_result");
        assert_eq!(request["tools"][0]["name"], "read");
        assert_eq!(request["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(request["tool_choice"]["type"], "auto");
    }

    #[test]
    fn anthropic_reply_becomes_an_openai_style_message() {
        let value = json!({
            "content": [
                {"type": "text", "text": "Here is the file."},
                {"type": "tool_use", "id": "toolu_2", "name": "read", "input": {"path": "src/lib.rs"}}
            ],
            "usage": {"input_tokens": 12, "output_tokens": 7}
        });
        let reply = anthropic_reply(&value).unwrap();
        assert_eq!(reply.message["role"], "assistant");
        assert_eq!(reply.message["content"], "Here is the file.");
        assert_eq!(reply.message["tool_calls"][0]["id"], "toolu_2");
        assert_eq!(reply.message["tool_calls"][0]["function"]["name"], "read");
        let arguments: Value = serde_json::from_str(
            reply.message["tool_calls"][0]["function"]["arguments"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(arguments["path"], "src/lib.rs");
        assert_eq!(reply.prompt_tokens, 12);
        assert_eq!(reply.completion_tokens, 7);
    }

    #[test]
    fn openai_reply_shape_is_unchanged() {
        let value = json!({
            "choices": [{"message": {"role": "assistant", "content": "ok"}}],
            "usage": {"prompt_tokens": 3, "completion_tokens": 4}
        });
        let reply = openai_reply(&value).unwrap();
        assert_eq!(reply.message["content"], "ok");
        assert_eq!(reply.prompt_tokens, 3);
        assert_eq!(reply.completion_tokens, 4);
    }

    #[test]
    fn tool_definitions_have_unique_names_and_schemas() {
        let tools = specs();
        for tool in &tools {
            let name = tool
                .pointer("/function/name")
                .and_then(Value::as_str)
                .unwrap();
            assert!(tool_schema(name).is_some());
        }
    }

    #[test]
    fn openai_stream_assembles_text_reasoning_tool_calls_and_usage() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"weigh \",\"content\":\"\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hello \"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"world\\n\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"notes.txt\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":7}}\n\n",
            "data: [DONE]\n\n",
        );
        let mut deltas = Vec::new();
        let reply = openai_stream(std::io::Cursor::new(body), &mut |delta| {
            deltas.push(delta);
            Ok(())
        })
        .unwrap();
        assert_eq!(
            deltas,
            vec![
                ModelDelta::Reasoning("weigh ".into()),
                ModelDelta::Text("Hello ".into()),
                ModelDelta::Text("world\n".into()),
            ]
        );
        assert_eq!(reply.message["content"], "Hello world\n");
        assert_eq!(reply.message["tool_calls"][0]["id"], "call_1");
        assert_eq!(reply.message["tool_calls"][0]["function"]["name"], "read");
        let arguments: Value = serde_json::from_str(
            reply.message["tool_calls"][0]["function"]["arguments"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(arguments["path"], "notes.txt");
        assert_eq!(reply.prompt_tokens, 11);
        assert_eq!(reply.completion_tokens, 7);
    }

    #[test]
    fn anthropic_stream_assembles_thinking_text_tool_input_and_usage() {
        let body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":9}}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"weigh options\"}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Answer\"}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"read\",\"input\":{}}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"notes.txt\\\"}\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":2}\n\n",
            "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":5}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let mut deltas = Vec::new();
        let reply = anthropic_stream(std::io::Cursor::new(body), &mut |delta| {
            deltas.push(delta);
            Ok(())
        })
        .unwrap();
        assert_eq!(
            deltas,
            vec![
                ModelDelta::Reasoning("weigh options".into()),
                ModelDelta::Text("Answer".into()),
            ]
        );
        assert_eq!(reply.message["content"], "Answer");
        assert_eq!(reply.message["tool_calls"][0]["id"], "toolu_1");
        let arguments: Value = serde_json::from_str(
            reply.message["tool_calls"][0]["function"]["arguments"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(arguments["path"], "notes.txt");
        assert_eq!(reply.prompt_tokens, 9);
        assert_eq!(reply.completion_tokens, 5);
    }

    #[test]
    fn stream_errors_are_reported() {
        let body = "data: {\"error\":{\"message\":\"overloaded\"}}\n\n";
        let error = openai_stream(std::io::Cursor::new(body), &mut |_| Ok(())).unwrap_err();
        assert!(error.contains("overloaded"), "{error}");

        let body = "data: {\"type\":\"error\",\"error\":{\"message\":\"bad request\"}}\n\n";
        let error = anthropic_stream(std::io::Cursor::new(body), &mut |_| Ok(())).unwrap_err();
        assert!(error.contains("bad request"), "{error}");
    }

    #[test]
    fn truncated_streams_are_errors_not_partial_answers() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"half an ans\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"wer\"}}]}\n\n",
        );
        let error = openai_stream(std::io::Cursor::new(body), &mut |_| Ok(())).unwrap_err();
        assert!(error.contains("[DONE]"), "{error}");

        let body = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":9}}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"half\"}}\n\n",
        );
        let error = anthropic_stream(std::io::Cursor::new(body), &mut |_| Ok(())).unwrap_err();
        assert!(error.contains("message_stop"), "{error}");
    }

    #[test]
    fn model_bodies_are_byte_bounded() {
        assert_eq!(
            read_bounded_text_with_limit(std::io::Cursor::new("1234"), None, 4).unwrap(),
            "1234"
        );
        let error =
            read_bounded_text_with_limit(std::io::Cursor::new("12345"), None, 4).unwrap_err();
        assert!(error.contains("byte limit"), "{error}");
    }

    struct CancelAfterRead<'a> {
        inner: std::io::Cursor<Vec<u8>>,
        cancelled: &'a AtomicBool,
    }

    impl Read for CancelAfterRead<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let read = self.inner.read(buffer)?;
            self.cancelled.store(true, Ordering::SeqCst);
            Ok(read)
        }
    }

    #[test]
    fn cancellation_interrupts_body_and_stream_reads() {
        let before = AtomicBool::new(true);
        let error = read_bounded_text(std::io::Cursor::new("unused"), Some(&before)).unwrap_err();
        assert_eq!(error, "MODEL_CANCELLED");

        let during = AtomicBool::new(false);
        let reader = CancelAfterRead {
            inner: std::io::Cursor::new(b"partial".to_vec()),
            cancelled: &during,
        };
        let error = read_bounded_text(reader, Some(&during)).unwrap_err();
        assert_eq!(error, "MODEL_CANCELLED");

        let during = AtomicBool::new(false);
        let reader = CancelAfterRead {
            inner: std::io::Cursor::new(b"data: first\n".to_vec()),
            cancelled: &during,
        };
        let error = read_sse(
            BufReader::new(CancelledReader::new(reader, Some(&during))),
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(error.contains("MODEL_CANCELLED"), "{error}");
    }

    #[test]
    fn sse_lines_events_and_aggregate_streams_are_bounded() {
        let line = format!("data: {}\n", "x".repeat(40));
        let error =
            read_sse_with_limits(std::io::Cursor::new(line), |_| Ok(()), 20, 100, 100).unwrap_err();
        assert!(error.contains("line exceeds"), "{error}");

        let event = format!("data: {}\ndata: {}\n\n", "x".repeat(15), "y".repeat(15));
        let error = read_sse_with_limits(std::io::Cursor::new(event), |_| Ok(()), 100, 30, 100)
            .unwrap_err();
        assert!(error.contains("event exceeds"), "{error}");

        let stream = "data: ok\n\n".repeat(5);
        let error = read_sse_with_limits(std::io::Cursor::new(stream), |_| Ok(()), 100, 100, 30)
            .unwrap_err();
        assert!(error.contains("stream exceeds"), "{error}");
    }

    #[test]
    fn retry_backoff_stops_when_cancelled() {
        let cancelled = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let token = &cancelled;
            scope.spawn(move || {
                std::thread::sleep(Duration::from_millis(10));
                token.store(true, Ordering::SeqCst);
            });
            let error = pause_before_retry(1, Some(&cancelled)).unwrap_err();
            assert_eq!(error, "MODEL_CANCELLED");
        });
    }

    #[test]
    fn cancelled_error_response_is_not_retried() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::atomic::AtomicUsize;
        use std::sync::Arc;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/chat/completions", listener.local_addr().unwrap());
        let cancelled = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(AtomicUsize::new(0));
        let server_cancel = Arc::clone(&cancelled);
        let server_requests = Arc::clone(&requests);
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            server_requests.fetch_add(1, Ordering::SeqCst);
            server_cancel.store(true, Ordering::SeqCst);
            stream
                .write_all(
                    b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 2\r\nConnection: close\r\n\r\nno",
                )
                .unwrap();
        });
        let model = ChatModel {
            client: reqwest::blocking::Client::builder()
                .no_proxy()
                .build()
                .unwrap(),
            endpoint,
            model: "dummy-model".into(),
            key: "dummy-key".into(),
            protocol: crate::types::ModelProtocol::OpenAi,
            max_output_tokens: 128,
        };
        let error = model
            .complete_with(&[], &[], "auto", Some(&cancelled))
            .unwrap_err();
        server.join().unwrap();
        assert_eq!(error, "MODEL_CANCELLED");
        assert_eq!(requests.load(Ordering::SeqCst), 1);
    }
}
