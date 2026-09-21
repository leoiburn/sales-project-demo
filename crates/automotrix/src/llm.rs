//! WHAT: a thin client for the Claude Messages API.
//! WHY:  Rust has no official Anthropic SDK, so this talks raw HTTP to
//!       POST /v1/messages. It is deliberately small: request in, response out,
//!       no opinions. The agent loop lives in `engine`, not here.
//! HOW:  serde types mirror the wire format. Tool definitions carry
//!       `strict: true`, which is a top-level field on the tool (not on
//!       tool_choice) and requires `additionalProperties: false` plus
//!       `required` - with it, tool arguments are guaranteed to validate
//!       against the schema, which is what lets the tool layer trust its input
//!       enough to hit the database with it.
//!
//!       Model: claude-haiku-4-5. Note it does NOT accept output_config.effort,
//!       and it uses the older thinking shape (budget_tokens) rather than
//!       adaptive thinking - neither is used here.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const DEFAULT_MODEL: &str = "claude-haiku-4-5";
const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    api_key: String,
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
    /// Anything the API sends that we do not model (thinking, citations...).
    /// Kept so an unknown block never crashes the loop.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: Vec<Block>,
}

impl Message {
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: vec![Block::Text { text: text.into() }],
        }
    }

    pub fn assistant(content: Vec<Block>) -> Self {
        Self {
            role: "assistant".into(),
            content,
        }
    }

    /// Every tool_result for one assistant turn goes back in a SINGLE user
    /// message. Splitting them across messages quietly teaches the model to stop
    /// issuing parallel tool calls.
    pub fn tool_results(results: Vec<Block>) -> Self {
        Self {
            role: "user".into(),
            content: results,
        }
    }

    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| match b {
                Block::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string()
    }

    pub fn tool_uses(&self) -> Vec<(&str, &str, &serde_json::Value)> {
        self.content
            .iter()
            .filter_map(|b| match b {
                Block::ToolUse { id, name, input } => Some((id.as_str(), name.as_str(), input)),
                _ => None,
            })
            .collect()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    /// Guarantees the arguments validate against input_schema.
    pub strict: bool,
}

#[derive(Debug, Serialize)]
struct Request<'a> {
    model: &'a str,
    max_tokens: u32,
    system: &'a str,
    messages: &'a [Message],
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [ToolDef]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_config: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct Response {
    pub content: Vec<Block>,
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub model: String,
}

impl Response {
    pub fn wants_tools(&self) -> bool {
        self.stop_reason.as_deref() == Some("tool_use")
    }

    pub fn text(&self) -> String {
        Message {
            role: "assistant".into(),
            content: self.content.clone(),
        }
        .text()
    }
}

impl Client {
    pub fn new(api_key: String, model: Option<String>) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("http client"),
            api_key,
            model: model.unwrap_or_else(|| DEFAULT_MODEL.to_string()),
        }
    }

    pub fn from_env() -> Result<Self> {
        crate::load_env();
        let key = std::env::var("ANTHROPIC_API_KEY")
            .context("ANTHROPIC_API_KEY not set - add it to .env")?;
        if key.trim().is_empty() {
            bail!("ANTHROPIC_API_KEY is empty");
        }
        Ok(Self::new(key, std::env::var("ANTHROPIC_MODEL").ok()))
    }

    pub fn configured() -> bool {
        crate::load_env();
        std::env::var("ANTHROPIC_API_KEY")
            .map(|k| !k.trim().is_empty())
            .unwrap_or(false)
    }

    async fn send(&self, req: Request<'_>) -> Result<Response> {
        let res = self
            .http
            .post(API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .json(&req)
            .send()
            .await
            .context("request to the Claude API failed")?;

        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("Claude API returned {status}: {}", body.chars().take(600).collect::<String>());
        }
        serde_json::from_str(&body)
            .with_context(|| format!("could not parse the Claude API response: {}",
                                     body.chars().take(400).collect::<String>()))
    }

    pub async fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: Option<&[ToolDef]>,
        max_tokens: u32,
    ) -> Result<Response> {
        self.send(Request {
            model: &self.model,
            max_tokens,
            system,
            messages,
            tools,
            output_config: None,
        })
        .await
    }

    /// One call that must come back as JSON matching `schema`.
    ///
    /// output_config.format is the canonical parameter (the older `output_format`
    /// is deprecated). If this model rejects it, fall back to asking in the
    /// system prompt and parsing - the caller validates the result either way,
    /// so a rejected parameter degrades quality, never correctness.
    pub async fn complete_json(
        &self,
        system: &str,
        messages: &[Message],
        schema: &serde_json::Value,
        max_tokens: u32,
    ) -> Result<serde_json::Value> {
        let output_config = Some(serde_json::json!({
            "format": { "type": "json_schema", "schema": schema }
        }));

        let response = match self
            .send(Request {
                model: &self.model,
                max_tokens,
                system,
                messages,
                tools: None,
                output_config: output_config.clone(),
            })
            .await
        {
            Ok(r) => r,
            Err(e) if e.to_string().contains("400") => {
                tracing::warn!("structured output rejected, falling back to prompted JSON: {e}");
                self.send(Request {
                    model: &self.model,
                    max_tokens,
                    system,
                    messages,
                    tools: None,
                    output_config: None,
                })
                .await?
            }
            Err(e) => return Err(e),
        };

        let text = response.text();
        parse_json_lenient(&text)
            .ok_or_else(|| anyhow!("model did not return JSON: {}", text.chars().take(300).collect::<String>()))
    }
}

/// Models sometimes wrap JSON in prose or a code fence. Take the outermost
/// object rather than failing the whole summary over a stray backtick.
pub fn parse_json_lenient(text: &str) -> Option<serde_json::Value> {
    if let Ok(v) = serde_json::from_str(text.trim()) {
        return Some(v);
    }
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str(&text[start..=end]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fenced_json() {
        let v = parse_json_lenient("Here you go:\n```json\n{\"a\": 1}\n```").unwrap();
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn tool_result_block_round_trips() {
        let b = Block::ToolResult {
            tool_use_id: "toolu_1".into(),
            content: "ok".into(),
            is_error: false,
        };
        let json = serde_json::to_string(&b).unwrap();
        assert!(json.contains("\"type\":\"tool_result\""));
        // is_error omitted when false, which is what the API expects
        assert!(!json.contains("is_error"));
    }

    #[test]
    fn unknown_blocks_do_not_break_parsing() {
        let r: Response = serde_json::from_str(
            r#"{"content":[{"type":"thinking","thinking":"..."},{"type":"text","text":"hi"}],
                "stop_reason":"end_turn","model":"claude-haiku-4-5"}"#,
        )
        .unwrap();
        assert_eq!(r.text(), "hi");
    }
}
