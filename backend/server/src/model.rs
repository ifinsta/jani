use crate::config::{Config, JResult};
use crate::types::ModelProtocol;
use reqwest::blocking::Client;
use serde_json::{json, Map, Value};
use std::time::Duration;

const MAX_ATTEMPTS: u64 = 3;
const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct ModelReply {
    pub message: Value,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
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
    pub fn new(cfg: &Config) -> JResult<Self> {
        let key = cfg.api_key.clone().ok_or_else(|| {
            format!(
                "set JANI_API_KEY, OPENAI_API_KEY, or a settings apiKey/apiKeyEnv to call model '{}'",
                cfg.model
            )
        })?;
        let client = Client::builder()
            .timeout(Duration::from_secs(180))
            .build()
            .map_err(|e| e.to_string())?;
        let base = cfg.api_base.trim_end_matches('/');
        let endpoint = match cfg.protocol {
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
            model: cfg.model.clone(),
            key,
            protocol: cfg.protocol,
            max_output_tokens: cfg.max_output_tokens,
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
}
