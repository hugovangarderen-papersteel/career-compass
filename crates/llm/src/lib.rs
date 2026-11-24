//! Claude API client: one forced tool call per request (structured output), prompt caching on
//! the static system block and on the conversation so far, and the cost of every call.
//! The caller checks the monthly cap before calling and logs the returned usage after (spec §10b).

use serde::Serialize;
use serde_json::{json, Value};
use std::time::Duration;

/// Models in use (Hugo's ruling 2026-11-24): Sonnet for the conversation and playback,
/// Haiku only for bulk extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Model {
    Sonnet,
    Haiku,
}

/// USD per million tokens: (input, 5-minute cache write, cache hit, output).
/// Source: https://platform.claude.com/docs/en/about-claude/pricing (read 2026-11-24); cache hit
/// = 10 % of input, as in the operator's own verified meter.
const SONNET_PRICE: [f64; 4] = [2.0, 2.5, 0.20, 10.0];
const HAIKU_PRICE: [f64; 4] = [0.10, 0.125, 0.01, 0.50];
/// Haiku 5.5 prompts above 100k input tokens bill at the long-context rate.
const HAIKU_LONG_PRICE: [f64; 4] = [0.50, 0.625, 0.05, 2.50];
const HAIKU_LONG_FROM: u64 = 100_000;

impl Model {
    pub fn id(self) -> &'static str {
        match self {
            Model::Sonnet => "claude-sonnet-5-5",
            Model::Haiku => "claude-haiku-5-5",
        }
    }
    fn price(self, prompt_tokens: u64) -> [f64; 4] {
        match self {
            Model::Sonnet => SONNET_PRICE,
            Model::Haiku if prompt_tokens > HAIKU_LONG_FROM => HAIKU_LONG_PRICE,
            Model::Haiku => HAIKU_PRICE,
        }
    }
    /// Upper-bound cost of a call before it runs: every prompt character counted as a
    /// token at the uncached rate, plus the full output budget.
    pub fn estimate_usd(self, prompt_chars: usize, max_tokens: u32) -> f64 {
        let p = self.price(prompt_chars as u64);
        (prompt_chars as f64 * p[0] + max_tokens as f64 * p[3]) / 1e6
    }
}

/// Token counts as the API reports them; `input` excludes the cached parts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
}

impl Usage {
    pub fn prompt_tokens(&self) -> u64 {
        self.input + self.cache_write + self.cache_read
    }
    pub fn cost_usd(&self, model: Model) -> f64 {
        let p = model.price(self.prompt_tokens());
        (self.input as f64 * p[0]
            + self.cache_write as f64 * p[1]
            + self.cache_read as f64 * p[2]
            + self.output as f64 * p[3])
            / 1e6
    }
    fn from_json(u: &Value) -> Usage {
        let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
        Usage {
            input: n("input_tokens"),
            output: n("output_tokens"),
            cache_write: n("cache_creation_input_tokens"),
            cache_read: n("cache_read_input_tokens"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone)]
pub struct Msg {
    pub role: Role,
    pub text: String,
}

/// One forced tool call: the model must answer by filling `tool_schema`.
#[derive(Debug, Clone)]
pub struct ToolRequest {
    pub model: Model,
    /// Same for every call of this kind: cached.
    pub system_static: String,
    /// Changes per user or per call (her confirmed documents, the current phase): not cached.
    pub system_dynamic: String,
    pub messages: Vec<Msg>,
    pub tool_name: String,
    pub tool_description: String,
    pub tool_schema: Value,
    pub max_tokens: u32,
}

impl ToolRequest {
    pub fn prompt_chars(&self) -> usize {
        self.system_static.len()
            + self.system_dynamic.len()
            + self.tool_schema.to_string().len()
            + self.messages.iter().map(|m| m.text.len()).sum::<usize>()
    }
    pub fn estimate_usd(&self) -> f64 {
        self.model.estimate_usd(self.prompt_chars(), self.max_tokens)
    }

    /// The Messages API body. Consecutive turns of one role are merged and the list always
    /// starts with a user turn, as the API requires.
    pub fn body(&self) -> Value {
        let mut msgs: Vec<(Role, String)> = vec![];
        for m in &self.messages {
            match msgs.last_mut() {
                Some((r, t)) if *r == m.role => {
                    t.push_str("\n\n");
                    t.push_str(&m.text);
                }
                _ => msgs.push((m.role, m.text.clone())),
            }
        }
        if msgs.first().map(|m| m.0) != Some(Role::User) {
            msgs.insert(0, (Role::User, "(Het gesprek begint.)".into()));
        }
        let last = msgs.len() - 1;
        let messages: Vec<Value> = msgs
            .into_iter()
            .enumerate()
            .map(|(i, (r, t))| {
                let mut block = json!({"type": "text", "text": t});
                if i == last {
                    // Second cache breakpoint: the whole conversation so far is reused next turn.
                    block["cache_control"] = json!({"type": "ephemeral"});
                }
                json!({"role": if r == Role::User { "user" } else { "assistant" }, "content": [block]})
            })
            .collect();
        let mut system =
            vec![json!({"type": "text", "text": self.system_static, "cache_control": {"type": "ephemeral"}})];
        if !self.system_dynamic.trim().is_empty() {
            system.push(json!({"type": "text", "text": self.system_dynamic}));
        }
        json!({
            "model": self.model.id(),
            "max_tokens": self.max_tokens,
            "system": system,
            "messages": messages,
            // Structured output: the 5.5 models refuse a forced tool_choice, but always answer
            // in this schema. `tool_name` stays ours, for logs and errors.
            // Low effort: short, conversational turns; the default thinking ate the token budget.
            "output_config": {"effort": "low", "format": {"type": "json_schema", "schema": strict(&self.tool_schema)}},
        })
    }
}

/// The schema as the structured-output API takes it: every object closed
/// (`additionalProperties: false`), numeric bounds dropped (callers clamp afterwards).
pub fn strict(schema: &Value) -> Value {
    match schema {
        Value::Object(o) => {
            let mut out = serde_json::Map::new();
            for (k, v) in o {
                if matches!(
                    k.as_str(),
                    "minimum" | "maximum" | "exclusiveMinimum" | "exclusiveMaximum"
                ) {
                    continue;
                }
                let v = if k == "properties" {
                    Value::Object(v.as_object().map_or_else(serde_json::Map::new, |p| {
                        p.iter().map(|(n, s)| (n.clone(), strict(s))).collect()
                    }))
                } else {
                    strict(v)
                };
                out.insert(k.clone(), v);
            }
            if o.get("type") == Some(&json!("object")) {
                out.insert("additionalProperties".into(), json!(false));
                // Optional fields blow up the compiled grammar (the API caps them), so every field
                // is required; one she did not mention comes back empty (arrays) or null.
                let was: Vec<&Value> = o
                    .get("required")
                    .and_then(Value::as_array)
                    .map_or(vec![], |r| r.iter().collect());
                if let Some(Value::Object(props)) = out.get_mut("properties") {
                    for (n, s) in props.iter_mut() {
                        if !was.contains(&&json!(n)) && s.get("type") != Some(&json!("array")) {
                            *s = json!({"anyOf": [s.clone(), {"type": "null"}]});
                        }
                    }
                    let all: Vec<Value> = props.keys().map(|k| json!(k)).collect();
                    out.insert("required".into(), Value::Array(all));
                }
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(strict).collect()),
        v => v.clone(),
    }
}

#[derive(Debug)]
pub enum LlmError {
    /// Network, timeout or an API error status (after one retry on overload).
    Unavailable(String),
    /// The API answered but not with the tool call we forced (cut off, refused, malformed).
    BadAnswer(String, Usage),
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LlmError::Unavailable(s) => write!(f, "model unavailable: {s}"),
            LlmError::BadAnswer(s, _) => write!(f, "unusable model answer: {s}"),
        }
    }
}

impl std::error::Error for LlmError {}

/// Pulls the forced tool's input and the usage out of a Messages API response.
pub fn parse_response(v: &Value, tool: &str) -> Result<(Value, Usage), LlmError> {
    let usage = Usage::from_json(&v["usage"]);
    if v["stop_reason"] == "max_tokens" {
        return Err(LlmError::BadAnswer("answer cut off at max_tokens".into(), usage));
    }
    v["content"]
        .as_array()
        .and_then(|c| c.iter().find(|b| b["type"] == "text"))
        .and_then(|b| serde_json::from_str::<Value>(b["text"].as_str()?).ok())
        .filter(Value::is_object)
        .map(|out| (out, usage))
        .ok_or_else(|| LlmError::BadAnswer(format!("no {tool} json, stop_reason {}", v["stop_reason"]), usage))
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    key: String,
}

impl Client {
    /// `base` is `https://api.anthropic.com` in production, a local mock in tests.
    pub fn new(base: &str, key: &str) -> Client {
        let http = reqwest::Client::builder()
            // The Cloudflare tunnel cuts a request at ~100 s; stay well inside it.
            .timeout(Duration::from_secs(80))
            .build()
            .expect("reqwest client");
        Client {
            http,
            base: base.trim_end_matches('/').to_string(),
            key: key.to_string(),
        }
    }

    pub async fn call(&self, req: &ToolRequest) -> Result<(Value, Usage), LlmError> {
        let body = req.body();
        let mut last = String::new();
        for attempt in 0..2 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            let res = self
                .http
                .post(format!("{}/v1/messages", self.base))
                .header("x-api-key", &self.key)
                .header("anthropic-version", "2023-06-01")
                .json(&body)
                .send()
                .await;
            let res = match res {
                Ok(r) => r,
                Err(e) => {
                    last = if e.is_timeout() {
                        "timeout".into()
                    } else {
                        "network error".into()
                    };
                    continue;
                }
            };
            let status = res.status();
            let v: Value = res.json().await.unwrap_or(Value::Null);
            if status.is_success() {
                return parse_response(&v, &req.tool_name);
            }
            let why: String = v["error"]["message"].as_str().unwrap_or("").chars().take(300).collect();
            last = format!("{status} {} {why}", v["error"]["type"].as_str().unwrap_or(""));
            // Retry only overload and server errors; a 4xx is our fault and will not improve.
            if !(status.as_u16() == 429 || status.is_server_error()) {
                break;
            }
        }
        Err(LlmError::Unavailable(last))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(msgs: Vec<Msg>) -> ToolRequest {
        ToolRequest {
            model: Model::Sonnet,
            system_static: "vast".into(),
            system_dynamic: "".into(),
            messages: msgs,
            tool_name: "t".into(),
            tool_description: "d".into(),
            tool_schema: json!({"type": "object"}),
            max_tokens: 100,
        }
    }

    #[test]
    fn body_starts_with_user_merges_roles_and_caches() {
        let b = req(vec![
            Msg {
                role: Role::Assistant,
                text: "Hoi".into(),
            },
            Msg {
                role: Role::User,
                text: "a".into(),
            },
            Msg {
                role: Role::User,
                text: "b".into(),
            },
        ])
        .body();
        let m = b["messages"].as_array().unwrap();
        assert_eq!(m.len(), 3);
        assert_eq!(m[0]["role"], "user");
        assert_eq!(m[2]["content"][0]["text"], "a\n\nb");
        assert_eq!(m[2]["content"][0]["cache_control"]["type"], "ephemeral");
        assert!(m[1]["content"][0].get("cache_control").is_none());
        assert_eq!(b["system"].as_array().unwrap().len(), 1, "empty dynamic block left out");
        assert_eq!(b["system"][0]["cache_control"]["type"], "ephemeral");
        assert!(b.get("tool_choice").is_none());
        assert_eq!(b["output_config"]["format"]["type"], "json_schema");
    }

    #[test]
    fn strict_closes_objects_and_drops_bounds() {
        let s = strict(&json!({"type": "object", "properties": {
            "properties": {"type": "string"},
            "xs": {"type": "array", "items": {"type": "object", "properties": {
                "rank": {"type": "integer", "minimum": 1, "maximum": 5}}}}}}));
        assert_eq!(s["additionalProperties"], false);
        assert_eq!(s["required"], json!(["properties", "xs"]), "every field required");
        assert_eq!(
            s["properties"]["properties"]["anyOf"],
            json!([{"type": "string"}, {"type": "null"}]),
            "a field named properties, optional so nullable"
        );
        assert_eq!(s["properties"]["xs"]["type"], "array", "an optional list stays a list");
        let item = &s["properties"]["xs"]["items"];
        assert_eq!(item["additionalProperties"], false);
        assert_eq!(
            item["properties"]["rank"]["anyOf"][0],
            json!({"type": "integer"}),
            "bounds dropped"
        );
    }

    #[test]
    fn parses_json_answer_and_usage() {
        let v = json!({"stop_reason": "end_turn", "content": [{"type": "text", "text": "{\"message\": \"Hoi\"}"}],
            "usage": {"input_tokens": 10, "output_tokens": 20, "cache_creation_input_tokens": 1000, "cache_read_input_tokens": 2000}});
        let (input, u) = parse_response(&v, "t").unwrap();
        assert_eq!(input["message"], "Hoi");
        assert_eq!(u.prompt_tokens(), 3010);
        // Sonnet: 10*2 + 1000*2.5 + 2000*0.2 + 20*10 = 3120 per Mtok
        assert!((u.cost_usd(Model::Sonnet) - 0.00312).abs() < 1e-12);
        let cut = json!({"stop_reason": "max_tokens", "content": [], "usage": {}});
        assert!(matches!(parse_response(&cut, "t"), Err(LlmError::BadAnswer(..))));
    }

    #[test]
    fn haiku_long_context_tier() {
        let short = Usage {
            input: 1_000_000,
            ..Default::default()
        };
        assert!(
            (short.cost_usd(Model::Haiku) - 0.50).abs() < 1e-9,
            "over 100k → long rate"
        );
        let small = Usage {
            input: 50_000,
            ..Default::default()
        };
        assert!((small.cost_usd(Model::Haiku) - 0.005).abs() < 1e-9);
    }
}
