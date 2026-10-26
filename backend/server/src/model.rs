use crate::config::{JResult, ModelChoice};
use crate::types::ModelProtocol;
use reqwest::blocking::Client;
use serde_json::{json, Map, Value};
use std::io::{BufRead, BufReader};
use std::time::Duration;

const MAX_ATTEMPTS: u64 = 3;
const ANTHROPIC_VERSION: &str = "2023-06-01";
const MAX_STREAM_BLOCKS: usize = 64;

#[derive(Debug)]
pub struct ModelReply {
    pub message: Value,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
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
        let client = Client::builder()
            .timeout(Duration::from_secs(180))
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

    pub fn complete(&self, messages: &[Value], tools: &[Value]) -> JResult<ModelReply> {
        let request = match self.protocol {
            ModelProtocol::OpenAi => {
                json!({"model": self.model, "messages": messages, "tools": tools, "tool_choice": "auto"})
            }
            ModelProtocol::Anthropic => {
                anthropic_request(&self.model, self.max_output_tokens, messages, tools)?
            }
        };
        let mut attempt = 0u64;
        loop {
            attempt += 1;
            let mut http = self.client.post(&self.endpoint);
            http = match self.protocol {
                ModelProtocol::OpenAi => http.bearer_auth(&self.key),
                ModelProtocol::Anthropic => http
                    .header("x-api-key", &self.key)
                    .header("anthropic-version", ANTHROPIC_VERSION),
            };
            let resp = http
                .json(&request)
                .send()
                .map_err(|e| format!("model request failed: {e}"))?;
            let status = resp.status();
            let body = resp.text().map_err(|e| e.to_string())?;
            if status.is_success() {
                let value: Value =
                    serde_json::from_str(&body).map_err(|e| format!("invalid model JSON: {e}"))?;
                return match self.protocol {
                    ModelProtocol::OpenAi => openai_reply(&value),
                    ModelProtocol::Anthropic => anthropic_reply(&value),
                };
            }
            // Transient provider failures are retried with a short linear backoff. The
            // turn ceiling still bounds total run time; no monetary budget is implied.
            let retryable = matches!(status.as_u16(), 408 | 429 | 500 | 502 | 503 | 504);
            if retryable && attempt < MAX_ATTEMPTS {
                std::thread::sleep(Duration::from_secs(attempt));
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
    ) -> JResult<ModelReply> {
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
            attempt += 1;
            let mut http = self.client.post(&self.endpoint);
            http = match self.protocol {
                ModelProtocol::OpenAi => http.bearer_auth(&self.key),
                ModelProtocol::Anthropic => http
                    .header("x-api-key", &self.key)
                    .header("anthropic-version", ANTHROPIC_VERSION),
            };
            let resp = http
                .json(&request)
                .send()
                .map_err(|e| format!("model request failed: {e}"))?;
            let status = resp.status();
            if status.is_success() {
                let reader = BufReader::new(resp);
                return match self.protocol {
                    ModelProtocol::OpenAi => openai_stream(reader, on_delta),
                    ModelProtocol::Anthropic => anthropic_stream(reader, on_delta),
                };
            }
            let body = resp.text().map_err(|e| e.to_string())?;
            let retryable = matches!(status.as_u16(), 408 | 429 | 500 | 502 | 503 | 504);
            if retryable && attempt < MAX_ATTEMPTS {
                std::thread::sleep(Duration::from_secs(attempt));
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
    let message = value
        .pointer("/choices/0/message")
        .cloned()
        .ok_or("response missing choices[0].message")?;
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
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or("tool_use block missing name")?;
                let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
                calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": serde_json::to_string(&input).map_err(|e| e.to_string())?,
                    },
                }));
            }
            _ => {}
        }
    }
    let mut message = Map::new();
    message.insert("role".into(), json!("assistant"));
    message.insert("content".into(), json!(text));
    if !calls.is_empty() {
        message.insert("tool_calls".into(), Value::Array(calls));
    }
    let prompt_tokens = value
        .pointer("/usage/input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let completion_tokens = value
        .pointer("/usage/output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Ok(ModelReply {
        message: Value::Object(message),
        prompt_tokens,
        completion_tokens,
    })
}

fn read_sse<R: BufRead>(
    mut reader: R,
    mut on_data: impl FnMut(&str) -> JResult<()>,
) -> JResult<()> {
    let mut line = String::new();
    let mut data = String::new();
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|e| format!("model stream read failed: {e}"))?;
        if read == 0 {
            if !data.is_empty() {
                on_data(&data)?;
            }
            return Ok(());
        }
        let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
        if trimmed.is_empty() {
            if !data.is_empty() {
                on_data(&data)?;
                data.clear();
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
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
    read_sse(reader, |data| {
        if data.trim() == "[DONE]" {
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
    let mut message = Map::new();
    message.insert("role".into(), json!("assistant"));
    message.insert("content".into(), json!(text));
    let mut assembled = Vec::new();
    for call in calls {
        if call.name.is_empty() {
            continue;
        }
        let id = call.id.ok_or("streamed tool call is missing its id")?;
        assembled.push(json!({
            "id": id,
            "type": call.kind.unwrap_or_else(|| "function".into()),
            "function": {
                "name": call.name,
                "arguments": if call.arguments.is_empty() { "{}".to_string() } else { call.arguments },
            },
        }));
    }
    if !assembled.is_empty() {
        message.insert("tool_calls".into(), Value::Array(assembled));
    }
    Ok(ModelReply {
        message: Value::Object(message),
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
                calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": serde_json::to_string(&input).map_err(|e| e.to_string())?,
                    },
                }));
            }
            _ => {}
        }
    }
    let mut message = Map::new();
    message.insert("role".into(), json!("assistant"));
    message.insert("content".into(), json!(text));
    if !calls.is_empty() {
        message.insert("tool_calls".into(), Value::Array(calls));
    }
    Ok(ModelReply {
        message: Value::Object(message),
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

pub fn specs() -> Vec<Value> {
    vec![
        function(
            "skill_read",
            "Read a discovered SKILL.md for workflow guidance",
            json!({"type":"object","properties":{"name":{"type":"string"}},"required":["name"],"additionalProperties":false}),
        ),
        function(
            "workspace_list",
            "List non-sensitive files in the assigned worktree",
            json!({"type":"object","properties":{"directory":{"type":"string","description":"relative directory or '.'"}},"required":["directory"],"additionalProperties":false}),
        ),
        function(
            "workspace_read",
            "Read a UTF-8 source file from the assigned worktree",
            json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}),
        ),
        function(
            "workspace_write",
            "Write one source file within the operator-approved edit scope",
            json!({"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"],"additionalProperties":false}),
        ),
        function(
            "native_tool",
            "Call one of Jani's 12 native indexing, context, planning, pruning, checkpoint, verification, impact, scheduling, workspace, evidence, optimization or simulation primitives. Never executes arbitrary tests or commits external effects.",
            json!({"type":"object","properties":{
                "tool":{"type":"string"},
                "operation":{"type":"string"},
                "args":{"type":"array","items":{"type":"string"}}
            },"required":["tool","operation","args"],"additionalProperties":false}),
        ),
        function(
            "workspace_command",
            "Run an allowlisted test or git command permitted by the operator",
            json!({"type":"object","properties":{"argv":{"type":"array","items":{"type":"string"}},"timeout_secs":{"type":"integer","minimum":1,"maximum":300}},"required":["argv","timeout_secs"],"additionalProperties":false}),
        ),
    ]
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
                "function": {"name": "workspace_read", "arguments": "{\"path\":\"src/main.rs\"}"},
            }]}),
            json!({"role": "tool", "tool_call_id": "toolu_1", "content": "fn main() {}"}),
        ];
        let request = anthropic_request(
            "claude-sonnet-5-5",
            8192,
            &messages,
            &[tool("workspace_read")],
        )
        .unwrap();
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
        assert_eq!(request["tools"][0]["name"], "workspace_read");
        assert_eq!(request["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(request["tool_choice"]["type"], "auto");
    }

    #[test]
    fn anthropic_reply_becomes_an_openai_style_message() {
        let value = json!({
            "content": [
                {"type": "text", "text": "Here is the file."},
                {"type": "tool_use", "id": "toolu_2", "name": "workspace_read", "input": {"path": "src/lib.rs"}}
            ],
            "usage": {"input_tokens": 12, "output_tokens": 7}
        });
        let reply = anthropic_reply(&value).unwrap();
        assert_eq!(reply.message["role"], "assistant");
        assert_eq!(reply.message["content"], "Here is the file.");
        assert_eq!(reply.message["tool_calls"][0]["id"], "toolu_2");
        assert_eq!(
            reply.message["tool_calls"][0]["function"]["name"],
            "workspace_read"
        );
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
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"workspace_read\",\"arguments\":\"{\\\"path\\\":\"}}]}}]}\n\n",
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
        assert_eq!(
            reply.message["tool_calls"][0]["function"]["name"],
            "workspace_read"
        );
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
            "data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"workspace_read\",\"input\":{}}}\n\n",
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
}
