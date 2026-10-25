use crate::config::{Config, JResult};
use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::time::Duration;

const MAX_ATTEMPTS: u64 = 3;

pub struct ChatModel {
    client: Client,
    endpoint: String,
    model: String,
    key: String,
}

impl ChatModel {
    pub fn new(cfg: &Config) -> JResult<Self> {
        let key = cfg
            .api_key
            .clone()
            .ok_or("set JANI_API_KEY or OPENAI_API_KEY to call a model")?;
        let client = Client::builder()
            .timeout(Duration::from_secs(180))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            client,
            endpoint: format!("{}/chat/completions", cfg.api_base),
            model: cfg.model.clone(),
            key,
        })
    }

    pub fn complete(&self, messages: &[Value], tools: &[Value]) -> JResult<Value> {
        let req = json!({"model": self.model, "messages": messages, "tools": tools, "tool_choice": "auto"});
        let mut attempt = 0u64;
        loop {
            attempt += 1;
            let resp = self
                .client
                .post(&self.endpoint)
                .bearer_auth(&self.key)
                .json(&req)
                .send()
                .map_err(|e| format!("model request failed: {e}"))?;
            let status = resp.status();
            let body = resp.text().map_err(|e| e.to_string())?;
            if status.is_success() {
                let v: Value =
                    serde_json::from_str(&body).map_err(|e| format!("invalid model JSON: {e}"))?;
                return v
                    .pointer("/choices/0/message")
                    .cloned()
                    .ok_or("response missing choices[0].message".into());
            }
            // Transient provider failures are retried with a short linear backoff. The
            // turn ceiling still bounds total run time; no monetary budget is implied.
            let retryable = matches!(status.as_u16(), 408 | 429 | 500 | 502 | 503 | 504);
            if retryable && attempt < MAX_ATTEMPTS {
                std::thread::sleep(Duration::from_secs(attempt));
                continue;
            }
            let excerpt = crate::process::truncate(&body, 900);
            return Err(format!("model HTTP {}: {excerpt}", status.as_u16()));
        }
    }
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
            "workspace_command",
            "Run an allowlisted test or git command permitted by the operator",
            json!({"type":"object","properties":{"argv":{"type":"array","items":{"type":"string"}},"timeout_secs":{"type":"integer","minimum":1,"maximum":300}},"required":["argv","timeout_secs"],"additionalProperties":false}),
        ),
    ]
}

fn function(name: &str, description: &str, parameters: Value) -> Value {
    json!({"type":"function","function":{"name":name,"description":description,"parameters":parameters}})
}
