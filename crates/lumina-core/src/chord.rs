//! Chord HTTP client for chat completions and tool operations with JWT authentication.
//!
//! # CHORD-04: Unified tool routing through Chord proxy
//!
//! All tool operations (list, call, discover) now route through the Chord proxy REST API
//! at `CHORD_PROXY_URL`, using the same JWT authentication and base URL as chat completions.
//!
//! - `tool_list()`     → POST `{base_url}/v1/tools/list`
//! - `tool_call()`     → POST `{base_url}/v1/tools/call`
//! - `tool_discover()` → POST `{base_url}/v1/tools/discover`
//!
//! The MCP transport (`mcp_client.rs`) is retained as an **emergency-only bypass**
//! and is no longer called in the normal tool path.

use crate::error::{LuminaError, Result};
use crate::tool_types::{ToolDefinition, ToolPermission};
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH, Duration};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    /// Content may be null when the model returns tool_calls instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Tool calls requested by the model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallMessage>>,
    /// For role="tool": ID of the tool call this is a response to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    /// Convenience constructor for user/assistant/system messages.
    pub fn text(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    /// Convenience constructor for a tool result message.
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".to_string(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

/// OpenAI-format tool call inside an assistant message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallMessage {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: ToolCallFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallFunction {
    pub name: String,
    pub arguments: String,
}

/// OpenAI-format tool definition for the `tools[]` array in a chat request.
#[derive(Debug, Clone, Serialize)]
pub struct ChordTool {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: ChordToolFunction,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChordToolFunction {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ChordTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChatCompletionChoice {
    pub message: ChatMessage,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChatCompletionResponse {
    pub choices: Vec<ChatCompletionChoice>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChordErrorResponse {
    pub error: String,
}

#[derive(Serialize)]
struct JwtHeader {
    alg: String,
    typ: String,
}

#[derive(Serialize)]
struct JwtPayload {
    sub: String,
    exp: u64,
}

pub struct ChordClient {
    client: reqwest::Client,
    base_url: String,
    jwt_secret: String,
}

impl ChordClient {
    /// Create a new Chord client
    pub fn new(base_url: String, jwt_secret: String) -> Self {
        // Timeout MUST exceed the agentic execution budget (AgenticRequest.timeout_secs,
        // default 60s). The agentic loop runs LLM inference + tool calls server-side and
        // can take 40s+ with a large tool catalog. A client timeout below the agentic
        // budget makes every tool-using turn fail with "Network error" and silently fall
        // back to the legacy loop (which lacks the full tool set). 120s leaves headroom
        // for the 60s budget plus the partial-response return.
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            base_url,
            jwt_secret,
        }
    }

    /// Generate a JWT token for Chord authentication
    /// Returns None if jwt_secret is empty (auth disabled)
    fn generate_jwt(&self) -> Option<Result<String>> {
        if self.jwt_secret.is_empty() {
            return None; // Skip auth if no secret
        }

        let result = || -> Result<String> {
            // JWT Header
            let header = JwtHeader {
                alg: "HS256".to_string(),
                typ: "JWT".to_string(),
            };

            // JWT Payload with 1 hour expiry
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| LuminaError::Chord(format!("System time error: {}", e)))?;

            let exp = now.as_secs() + 3600; // 1 hour from now

            let payload = JwtPayload {
                sub: "lumina".to_string(),
                exp,
            };

            // Encode header and payload as base64url
            let header_json = serde_json::to_string(&header)?;
            let payload_json = serde_json::to_string(&payload)?;

            let encoded_header = base64url_encode(header_json.as_bytes());
            let encoded_payload = base64url_encode(payload_json.as_bytes());

            // Create signing input
            let signing_input = format!("{}.{}", encoded_header, encoded_payload);

            // Sign with HMAC-SHA256
            let mut mac = HmacSha256::new_from_slice(self.jwt_secret.as_bytes())
                .map_err(|e| LuminaError::Chord(format!("Invalid JWT secret: {}", e)))?;

            mac.update(signing_input.as_bytes());
            let signature = mac.finalize().into_bytes();
            let encoded_signature = base64url_encode(&signature);

            // Combine all parts
            Ok(format!("{}.{}.{}", encoded_header, encoded_payload, encoded_signature))
        };

        Some(result())
    }

    /// Send a chat completion request to Chord
    /// Model name is hardcoded as "lumina"
    /// Send a single user message to Chord with a specific model alias.
    ///
    /// FORGE-02: `model` is now dynamic (e.g. "lumina-fast" or "lumina-deep")
    /// rather than hardcoded. Returns a ZeroizingString so response content
    /// is wiped from heap after use.
    pub async fn chat(
        &self,
        model: &str,
        message: &str,
    ) -> Result<crate::secure_string::ZeroizingString> {
        let messages = vec![ChatMessage::text("user", message)];
        self.chat_completion_with_model(model, messages).await
    }

    /// Send messages to Chord with a specific model alias.
    pub async fn chat_completion_with_model(
        &self,
        model: &str,
        messages: Vec<ChatMessage>,
    ) -> Result<crate::secure_string::ZeroizingString> {
        let request = ChatCompletionRequest {
            model: model.to_string(),
            messages,
            tools: None,
            stream: Some(false),
        };

        let url = format!("{}/v1/chat/completions", self.base_url);

        let mut req_builder = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&request);

        if let Some(jwt_result) = self.generate_jwt() {
            let jwt = jwt_result?;
            req_builder = req_builder.header("Authorization", format!("Bearer {}", jwt));
        }

        let response = req_builder.send().await?;
        let status = response.status();

        if status.is_success() {
            let completion: ChatCompletionResponse = response.json().await?;

            completion
                .choices
                .first()
                .map(|choice| crate::secure_string::ZeroizingString::new(
                    choice.message.content.clone().unwrap_or_default()
                ))
                .ok_or_else(|| LuminaError::Chord("No response choices returned".to_string()))
        } else if status == 401 {
            Err(LuminaError::Chord("Authentication failed - invalid JWT".to_string()))
        } else if status == 500 {
            Err(LuminaError::Chord("Chord server error".to_string()))
        } else {
            match response.json::<ChordErrorResponse>().await {
                Ok(error_resp) => Err(LuminaError::Chord(error_resp.error)),
                Err(_) => Err(LuminaError::Chord(format!("HTTP {} error", status))),
            }
        }
    }

    /// Send messages to Chord using the default "lumina" model alias.
    ///
    /// Retained for backward compatibility. Prefer `chat()` or
    /// `chat_completion_with_model()` for new code.
    pub async fn chat_completion(
        &self,
        messages: Vec<ChatMessage>,
    ) -> Result<crate::secure_string::ZeroizingString> {
        self.chat_completion_with_model("lumina", messages).await
    }

    // ── CHORD-04: Tool proxy methods ───────────────────────────────────────────

    /// Make an authenticated POST to a Chord tool endpoint.
    ///
    /// Shared by `tool_list`, `tool_call`, and `tool_discover`.
    async fn post_tool_endpoint(&self, path: &str, body: serde_json::Value) -> Result<serde_json::Value> {
        let url = format!("{}{}", self.base_url, path);
        let mut req_builder = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body);

        if let Some(jwt_result) = self.generate_jwt() {
            let jwt = jwt_result?;
            req_builder = req_builder.header("Authorization", format!("Bearer {}", jwt));
        }

        let response = req_builder.send().await?;
        let status = response.status();

        if status.is_success() {
            let value: serde_json::Value = response.json().await?;
            Ok(value)
        } else if status == 401 {
            Err(LuminaError::Chord("Authentication failed - invalid JWT".to_string()))
        } else {
            match response.json::<ChordErrorResponse>().await {
                Ok(e) => Err(LuminaError::Chord(format!("Tool endpoint error: {}", e.error))),
                Err(_) => Err(LuminaError::Chord(format!("HTTP {} error from tool endpoint", status))),
            }
        }
    }

    /// Parse a Chord tool-list response into `ToolDefinition`s.
    ///
    /// Chord returns `{"tools": [{"name": ..., "description": ..., "parameters": ..., "source": ...}]}`.
    fn parse_tool_list(value: serde_json::Value) -> Result<Vec<ToolDefinition>> {
        let tools = value["tools"]
            .as_array()
            .ok_or_else(|| LuminaError::Chord("tool_list response missing 'tools' array".to_string()))?;

        let mut defs = Vec::with_capacity(tools.len());
        for tool in tools {
            let name = tool["name"].as_str().unwrap_or("").to_string();
            if name.is_empty() {
                continue;
            }
            let description = tool["description"].as_str().unwrap_or("").to_string();
            let argument_schema = if tool["parameters"].is_object() {
                tool["parameters"].clone()
            } else {
                serde_json::json!({ "type": "object", "properties": {} })
            };
            defs.push(ToolDefinition::new(
                name,
                description,
                ToolPermission::ReadOnly,
                argument_schema,
            ));
        }
        Ok(defs)
    }

    /// Fetch the merged tool catalog from the Chord proxy.
    ///
    /// Returns the full list of tools available through Chord (all backends combined).
    /// Uses `POST {base_url}/v1/tools/list` with an empty body.
    pub async fn tool_list(&self) -> Result<Vec<ToolDefinition>> {
        let value = self.post_tool_endpoint("/v1/tools/list", serde_json::json!({})).await?;
        Self::parse_tool_list(value)
    }

    /// Execute a tool call through the Chord proxy.
    ///
    /// Sends `{"name": name, "arguments": args}` to `POST {base_url}/v1/tools/call`.
    /// Returns the string result content from Chord.
    pub async fn tool_call(&self, name: &str, args: serde_json::Value) -> Result<String> {
        let body = serde_json::json!({ "name": name, "arguments": args });
        let value = self.post_tool_endpoint("/v1/tools/call", body).await?;

        // Chord returns {"result": "..."} or {"content": "..."} or {"error": "..."}
        if let Some(err) = value["error"].as_str() {
            return Err(LuminaError::Chord(format!("Tool '{}' error: {}", name, err)));
        }

        let content = value["result"]
            .as_str()
            .or_else(|| value["content"].as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| value.to_string());

        Ok(content)
    }

    /// Discover tools via semantic search through the Chord proxy.
    ///
    /// Sends `{"query": query, "max_results": max}` to `POST {base_url}/v1/tools/discover`.
    /// Returns a ranked list of matching `ToolDefinition`s.
    pub async fn tool_discover(&self, query: &str, max: usize) -> Result<Vec<ToolDefinition>> {
        let body = serde_json::json!({ "query": query, "max_results": max });
        let value = self.post_tool_endpoint("/v1/tools/discover", body).await?;
        Self::parse_tool_list(value)
    }

    /// Send messages with tools and return the raw response choice (includes tool_calls).
    ///
    /// Used by the tool-calling loop (P1-13): the response may contain `tool_calls`
    /// instead of a text answer.
    pub async fn chat_with_tools(
        &self,
        model: &str,
        messages: Vec<ChatMessage>,
        tools: Option<Vec<ChordTool>>,
    ) -> Result<ChatMessage> {
        let request = ChatCompletionRequest {
            model: model.to_string(),
            messages,
            tools,
            stream: Some(false),
        };

        let url = format!("{}/v1/chat/completions", self.base_url);
        let mut req_builder = self.client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&request);

        if let Some(jwt_result) = self.generate_jwt() {
            req_builder = req_builder.header("Authorization", format!("Bearer {}", jwt_result?));
        }

        let response = req_builder.send().await?;
        let status = response.status();

        if status.is_success() {
            let completion: ChatCompletionResponse = response.json().await?;
            completion.choices.into_iter().next()
                .map(|c| c.message)
                .ok_or_else(|| LuminaError::Chord("No response choices returned".to_string()))
        } else if status == 401 {
            Err(LuminaError::Chord("Authentication failed - invalid JWT".to_string()))
        } else {
            match response.json::<ChordErrorResponse>().await {
                Ok(e) => Err(LuminaError::Chord(e.error)),
                Err(_) => Err(LuminaError::Chord(format!("HTTP {} error", status))),
            }
        }
    }
}

// ── AGENT-02: Agentic execution ───────────────────────────────────────────────

/// A security event emitted by one of the four Chord guards during the agentic loop.
///
/// Mirrors `chord_proxy::agentic::SecurityEvent` but is defined here independently
/// so `lumina-core` does not need to depend on the `chord-proxy` crate.  The fields
/// are identical and the JSON representation is compatible.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgenticSecurityEvent {
    /// Which guard fired: "argument", "result", "response", or "behavioral".
    pub guard_name: String,
    /// What was done: "blocked", "sanitized", or "warned".
    pub action: String,
    /// Tool that was involved (may be empty for behavioral anomalies).
    pub tool_name: String,
    /// Human-readable reason — never contains raw tool arguments or results.
    pub reason: String,
}

/// Metadata for a single curated research document recorded in the execution log.
///
/// HRNS-05 records one `research_source` step per curated document at the end of
/// a harness search phase. The fields carry ONLY public document metadata
/// (title, url, importance tag, and the BM25-compressed summary the model saw) —
/// never full document text. HRNS-07 reads these steps to ingest high-importance
/// findings into Engram as Semantic memories.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchSource {
    /// Document title.
    pub title: String,
    /// Source URL.
    pub url: String,
    /// Importance tag assigned by the harness: "very_high", "high", "fair", "low".
    pub importance: String,
    /// Compressed summary (BM25 top sentences) the model was shown. NOT full text.
    #[serde(default)]
    pub summary: String,
    /// Harness turn at which the document was curated (used for recency tie-breaks).
    #[serde(default)]
    pub added_at_turn: usize,
}

/// Metadata for a single step in the agentic execution log.
///
/// Mirrors `chord_proxy::agentic::ExecutionStep`.  MUST NOT contain tool arguments
/// or raw results — only timing and status metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgenticExecutionStep {
    /// Step type: "tool_call", "llm_response", "guard_block", "timeout", or
    /// "research_source" (HRNS-05: one per curated research document).
    pub step_type: String,
    /// Tool name involved (empty for LLM-only steps).
    #[serde(default)]
    pub tool_name: Option<String>,
    /// Wall-clock duration of this step in milliseconds.
    #[serde(default)]
    pub duration_ms: u64,
    /// Outcome: "ok", "blocked", "error", or "timeout".
    pub status: String,
    /// Human-readable error (never contains args or results).
    #[serde(default)]
    pub error_message: Option<String>,
    /// HRNS-05/07: curated research-document metadata, present only on
    /// `step_type == "research_source"` steps. Title/url/importance/summary only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research_source: Option<ResearchSource>,
}

/// Request sent to `POST /v1/agent/execute`.
///
/// Constructed by `agentic_execute()` from the caller-supplied parameters.
/// This type is only used internally (not exposed in the public API surface).
#[derive(Debug, Serialize)]
struct AgenticRequest {
    messages: Vec<AgenticMessage>,
    system_prompt: String,
    tools: Vec<AgenticToolDef>,
    permissions: Vec<String>,
    max_tool_calls: u8,
    timeout_secs: u32,
    user_id: String,
    model: String,
    /// RESP-04: request the SSE-streaming progress-event response from Chord so
    /// the client can react to tool dispatch the instant it happens.
    stream: bool,
}

/// A single message in the agentic context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgenticMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// Tool definition forwarded to Chord.
#[derive(Debug, Clone, Serialize)]
pub struct AgenticToolDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Response from `POST /v1/agent/execute`.
#[derive(Debug, Deserialize)]
struct AgenticResponse {
    response: String,
    #[serde(default)]
    execution_log: Vec<AgenticExecutionStep>,
    #[serde(default)]
    security_events: Vec<AgenticSecurityEvent>,
}

/// RESP-04: incremental parser for Chord's `/v1/agent/execute` SSE stream.
///
/// Accumulates raw response bytes, splits them into `\n\n`-delimited SSE frames,
/// and reconstructs the `(response, execution_log, security_events)` tuple from
/// the `ProgressEvent`s. Lenient: frames that don't parse are skipped. The
/// `on_tool` callback fires exactly once, on the first `tool_call_started` event,
/// so the caller (the Matrix bot) can post a tailored interim ack.
///
/// Extracted as a struct (rather than inlined in `agentic_execute`) so it is
/// unit-testable with hand-fed frames — no live Chord or LLM required.
struct AgenticSseState {
    /// Raw byte buffer. We split frames on the `\n\n` byte delimiter and only
    /// UTF-8-decode *complete* frames — a multibyte codepoint split across two
    /// network chunks stays intact in the buffer until its frame is whole,
    /// instead of being lossy-decoded per chunk into replacement characters.
    buf: Vec<u8>,
    response: Option<String>,
    exec_log: Vec<AgenticExecutionStep>,
    security_events: Vec<AgenticSecurityEvent>,
    signaled: bool,
}

impl AgenticSseState {
    fn new() -> Self {
        Self {
            buf: Vec::new(),
            response: None,
            exec_log: Vec::new(),
            security_events: Vec::new(),
            signaled: false,
        }
    }

    /// Append a raw byte chunk and process every complete (`\n\n`-terminated)
    /// frame it makes available. Bytes that don't yet form a complete frame
    /// stay buffered (so codepoints straddling a chunk boundary are never
    /// corrupted). A frame boundary is always a valid UTF-8 boundary because
    /// `\n` is ASCII, so decoding a complete frame can only fail if its own
    /// content is malformed — in which case the frame is skipped leniently.
    fn push_chunk(&mut self, chunk: &[u8], on_tool: &mut impl FnMut(&str)) {
        self.buf.extend_from_slice(chunk);
        while let Some(idx) = self.buf.windows(2).position(|w| w == b"\n\n") {
            let drained: Vec<u8> = self.buf.drain(..idx + 2).collect();
            // drained = frame bytes + the 2-byte "\n\n" delimiter.
            if let Ok(frame) = std::str::from_utf8(&drained[..idx]) {
                let frame = frame.to_string();
                self.handle_frame(&frame, on_tool);
            }
        }
    }

    /// Test-only convenience: feed a `&str` chunk as bytes.
    #[cfg(test)]
    fn push_str(&mut self, chunk: &str, on_tool: &mut impl FnMut(&str)) {
        self.push_chunk(chunk.as_bytes(), on_tool);
    }

    /// Parse one SSE frame (possibly multi-line `data:` payload) and fold the
    /// resulting `ProgressEvent` into the reconstructed state.
    fn handle_frame(&mut self, frame: &str, on_tool: &mut impl FnMut(&str)) {
        // Join all `data:` lines (SSE allows a payload split across lines).
        let mut payload = String::new();
        for line in frame.lines() {
            if let Some(rest) = line.strip_prefix("data:") {
                payload.push_str(rest.trim_start());
            }
        }
        if payload.is_empty() {
            return;
        }
        let v: serde_json::Value = match serde_json::from_str(&payload) {
            Ok(v) => v,
            Err(_) => return, // lenient: skip unparseable frames
        };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("tool_call_started") => {
                let name = v
                    .get("tool_name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                self.exec_log.push(AgenticExecutionStep {
                    step_type: "tool_call".into(),
                    tool_name: Some(name.clone()),
                    duration_ms: 0,
                    status: "ok".into(),
                    error_message: None,
                    research_source: None,
                });
                if !self.signaled {
                    self.signaled = true;
                    on_tool(&name);
                }
            }
            Some("tool_call_complete") => {
                let name = v.get("tool_name").and_then(|n| n.as_str()).unwrap_or("");
                let duration_ms = v.get("duration_ms").and_then(|d| d.as_u64()).unwrap_or(0);
                let status = v
                    .get("status")
                    .and_then(|s| s.as_str())
                    .unwrap_or("ok")
                    .to_string();
                // Best-effort: update the LAST matching not-yet-timed tool step.
                if let Some(step) = self
                    .exec_log
                    .iter_mut()
                    .rev()
                    .find(|s| s.duration_ms == 0 && s.tool_name.as_deref() == Some(name))
                {
                    step.duration_ms = duration_ms;
                    step.status = status;
                }
            }
            Some("security_event_occurred") => {
                self.security_events.push(AgenticSecurityEvent {
                    guard_name: v
                        .get("guard_name")
                        .and_then(|g| g.as_str())
                        .unwrap_or("")
                        .to_string(),
                    action: v
                        .get("action")
                        .and_then(|a| a.as_str())
                        .unwrap_or("")
                        .to_string(),
                    tool_name: v
                        .get("tool_name")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_string(),
                    reason: String::new(),
                });
            }
            Some("complete") => {
                self.response = Some(
                    v.get("response")
                        .and_then(|r| r.as_str())
                        .unwrap_or("")
                        .to_string(),
                );
            }
            _ => {} // "started" and unknown types carry no reconstructable state
        }
    }

    /// Finalize: succeed only if a `complete` event was seen.
    fn finish(
        self,
    ) -> Result<(String, Vec<AgenticExecutionStep>, Vec<AgenticSecurityEvent>)> {
        match self.response {
            Some(resp) => Ok((resp, self.exec_log, self.security_events)),
            None => Err(LuminaError::Chord(
                "agentic SSE ended without a complete event".to_string(),
            )),
        }
    }
}

impl ChordClient {
    /// Execute a full agentic turn on Chord.
    ///
    /// Packages `messages`, `system_prompt`, `tools`, and `permissions` into an
    /// [`AgenticRequest`] and POSTs it to `{base_url}/v1/agent/execute` with
    /// `stream: true`, then consumes the SSE progress stream (RESP-04). The
    /// `on_tool_started` callback fires once, the instant the first tool is
    /// dispatched server-side, so callers can post a tailored interim ack.
    ///
    /// Returns `(response_text, execution_log, security_events)` on success.
    ///
    /// # Errors
    /// Returns `Err` if Chord is unreachable, returns an error status, or the
    /// stream ends without a `complete` event. Callers **must** fall back to the
    /// legacy tool loop on error.
    pub async fn agentic_execute(
        &self,
        messages: Vec<AgenticMessage>,
        system_prompt: String,
        tools: Vec<AgenticToolDef>,
        permissions: Vec<String>,
        user_id: String,
        model: String,
        mut on_tool_started: impl FnMut(&str),
    ) -> Result<(String, Vec<AgenticExecutionStep>, Vec<AgenticSecurityEvent>)> {
        use futures::StreamExt;

        let request = AgenticRequest {
            messages,
            system_prompt,
            tools,
            permissions,
            max_tool_calls: 5,
            timeout_secs: 60,
            user_id,
            model,
            stream: true,
        };

        let url = format!("{}/v1/agent/execute", self.base_url);

        let mut req_builder = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&request);

        if let Some(jwt_result) = self.generate_jwt() {
            let jwt = jwt_result?;
            req_builder = req_builder.header("Authorization", format!("Bearer {}", jwt));
        }

        let response = req_builder.send().await?;
        let status = response.status();

        if status.is_success() {
            // RESP-04: consume the SSE byte stream incrementally.
            let mut state = AgenticSseState::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let bytes = chunk
                    .map_err(|e| LuminaError::Chord(format!("agentic stream error: {e}")))?;
                // Feed raw bytes — the parser buffers and decodes per complete
                // frame, so codepoints split across chunks are not corrupted.
                state.push_chunk(&bytes, &mut on_tool_started);
            }
            state.finish()
        } else if status == 401 {
            Err(LuminaError::Chord("Authentication failed - invalid JWT".to_string()))
        } else {
            match response.json::<ChordErrorResponse>().await {
                Ok(e) => Err(LuminaError::Chord(format!("Agentic execute error: {}", e.error))),
                Err(_) => Err(LuminaError::Chord(format!("HTTP {} from agentic execute", status))),
            }
        }
    }
}

/// Encode bytes as base64url (no padding)
fn base64url_encode(input: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(input)
}

// ── ESEC-02: KV cache isolation between users ──────────────────────────────────

/// Generate a KV cache-busting token for a user+session combination.
///
/// The token is an 8-hex-char prefix of SHA-256(user_id || "|" || session_id).
/// - Same user + same session → same token (stable, enables cache reuse within
///   a session — this is intentional and correct)
/// - Different user → different token (forces fresh KV cache allocation)
/// - Different session (after /new) → different token (fresh context slot)
///
/// The token is NOT sensitive — it reveals nothing about the user beyond that
/// they have a unique identity. The SHA-256 input (user_id) is already known to
/// the system. The token is NOT stored in logs or conversation history.
pub fn kv_cache_buster(user_id: &str, session_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(user_id.as_bytes());
    hasher.update(b"|");
    hasher.update(session_id.as_bytes());
    let digest = hasher.finalize();
    // First 4 bytes → 8 hex chars
    format!("{:02x}{:02x}{:02x}{:02x}", digest[0], digest[1], digest[2], digest[3])
}

/// Inject the KV cache-buster token into the first system message in a messages list.
///
/// Finds the first `role="system"` message and appends the token as:
/// `\n[session: {token}]`
///
/// This ensures the system prompt prefix is unique per user+session, so the
/// LLM allocates a fresh KV cache slot. The LLM ignores the token as noise;
/// it only affects the cache key.
///
/// If no system message exists, does nothing (token not injected into user messages).
pub fn inject_cache_buster_into_messages(
    messages: &mut Vec<ChatMessage>,
    user_id: &str,
    session_id: &str,
) {
    let token = kv_cache_buster(user_id, session_id);
    for msg in messages.iter_mut() {
        if msg.role == "system" {
            if let Some(ref mut content) = msg.content {
                content.push_str(&format!("\n[session: {token}]"));
            }
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── RESP-04: AgenticSseState (SSE reconstruction) ─────────────────────────

    #[test]
    fn test_sse_state_tool_dispatch_fires_callback_once() {
        let mut state = AgenticSseState::new();
        let mut calls: Vec<String> = Vec::new();
        let mut on_tool = |name: &str| calls.push(name.to_string());

        state.push_str("data: {\"type\":\"started\"}\n\n", &mut on_tool);
        state.push_str(
            "data: {\"type\":\"tool_call_started\",\"tool_name\":\"searxng_search\",\"step_number\":1}\n\n",
            &mut on_tool,
        );
        state.push_str(
            "data: {\"type\":\"complete\",\"response\":\"hi\"}\n\n",
            &mut on_tool,
        );

        assert_eq!(calls, vec!["searxng_search".to_string()], "callback fires once");
        let (resp, log, sec) = state.finish().expect("complete seen");
        assert_eq!(resp, "hi");
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].step_type, "tool_call");
        assert_eq!(log[0].tool_name.as_deref(), Some("searxng_search"));
        assert!(sec.is_empty());
    }

    #[test]
    fn test_sse_state_no_tool_never_fires_callback() {
        let mut state = AgenticSseState::new();
        let mut fired = false;
        let mut on_tool = |_: &str| fired = true;

        state.push_str("data: {\"type\":\"started\"}\n\n", &mut on_tool);
        state.push_str(
            "data: {\"type\":\"complete\",\"response\":\"just text\"}\n\n",
            &mut on_tool,
        );

        assert!(!fired, "callback must NOT fire when no tool ran");
        let (resp, log, _sec) = state.finish().expect("complete seen");
        assert_eq!(resp, "just text");
        assert!(log.is_empty(), "no tool steps");
    }

    #[test]
    fn test_sse_state_malformed_frame_is_skipped() {
        let mut state = AgenticSseState::new();
        let mut on_tool = |_: &str| {};

        // Garbage frame, then a valid complete.
        state.push_str("data: {not json\n\n", &mut on_tool);
        state.push_str(
            "data: {\"type\":\"complete\",\"response\":\"ok\"}\n\n",
            &mut on_tool,
        );
        let (resp, _log, _sec) = state.finish().expect("garbage skipped, complete honored");
        assert_eq!(resp, "ok");
    }

    #[test]
    fn test_sse_state_no_complete_event_is_error() {
        let mut state = AgenticSseState::new();
        let mut on_tool = |_: &str| {};
        state.push_str("data: {\"type\":\"started\"}\n\n", &mut on_tool);
        assert!(state.finish().is_err(), "missing complete ⇒ Err");
    }

    #[test]
    fn test_sse_state_frame_split_across_chunks_parses() {
        let mut state = AgenticSseState::new();
        let mut calls: Vec<String> = Vec::new();
        let mut on_tool = |name: &str| calls.push(name.to_string());

        // A single tool_call_started frame delivered in two pieces.
        state.push_str("data: {\"type\":\"tool_call_started\",", &mut on_tool);
        state.push_str(
            "\"tool_name\":\"utc_now\",\"step_number\":1}\n\n",
            &mut on_tool,
        );
        state.push_str(
            "data: {\"type\":\"complete\",\"response\":\"done\"}\n\n",
            &mut on_tool,
        );

        assert_eq!(calls, vec!["utc_now".to_string()]);
        let (resp, log, _sec) = state.finish().expect("complete seen");
        assert_eq!(resp, "done");
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn test_sse_state_multibyte_codepoint_split_across_chunks() {
        // Regression: a 3-byte UTF-8 codepoint ('…' = E2 80 A6) split across two
        // raw byte chunks must reconstruct intact — not become replacement chars.
        let mut state = AgenticSseState::new();
        let mut on_tool = |_: &str| {};
        let full = "data: {\"type\":\"complete\",\"response\":\"wait…done\"}\n\n";
        let bytes = full.as_bytes();
        let ell = full.find('…').unwrap();
        let split = ell + 1; // mid-codepoint byte boundary (invalid on its own)
        state.push_chunk(&bytes[..split], &mut on_tool);
        state.push_chunk(&bytes[split..], &mut on_tool);
        let (resp, _log, _sec) = state.finish().expect("complete seen");
        assert_eq!(resp, "wait…done", "multibyte char survived the chunk split");
        assert!(!resp.contains('\u{FFFD}'), "no replacement chars");
    }

    #[test]
    fn test_sse_state_tool_complete_updates_duration() {
        let mut state = AgenticSseState::new();
        let mut on_tool = |_: &str| {};
        state.push_str(
            "data: {\"type\":\"tool_call_started\",\"tool_name\":\"plane_list_issues\",\"step_number\":1}\n\n",
            &mut on_tool,
        );
        state.push_str(
            "data: {\"type\":\"tool_call_complete\",\"tool_name\":\"plane_list_issues\",\"duration_ms\":512,\"status\":\"ok\"}\n\n",
            &mut on_tool,
        );
        state.push_str(
            "data: {\"type\":\"complete\",\"response\":\"x\"}\n\n",
            &mut on_tool,
        );
        let (_resp, log, _sec) = state.finish().expect("complete");
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].duration_ms, 512);
    }

    #[test]
    fn test_base64url_encoding() {
        let input = b"hello world";
        let encoded = base64url_encode(input);
        assert!(!encoded.contains('='), "Base64url should not contain padding");
        assert!(!encoded.contains('+'), "Base64url should not contain +");
        assert!(!encoded.contains('/'), "Base64url should not contain /");
    }

    #[test]
    fn test_jwt_header_serialization() {
        let header = JwtHeader {
            alg: "HS256".to_string(),
            typ: "JWT".to_string(),
        };

        let json = serde_json::to_string(&header).unwrap();
        assert!(json.contains("\"alg\":\"HS256\""));
        assert!(json.contains("\"typ\":\"JWT\""));
    }

    #[test]
    fn test_jwt_payload_serialization() {
        let payload = JwtPayload {
            sub: "lumina".to_string(),
            exp: 1234567890,
        };

        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("\"sub\":\"lumina\""));
        assert!(json.contains("\"exp\":1234567890"));
    }

    #[test]
    fn test_jwt_generation_with_secret() {
        let client = ChordClient::new(
            "http://localhost:8099".to_string(),
            "test-secret".to_string(),
        );

        let jwt_result = client.generate_jwt();
        assert!(jwt_result.is_some());

        let jwt = jwt_result.unwrap().unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3, "JWT should have header.payload.signature");

        // Verify header
        let header_json = String::from_utf8(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(parts[0])
                .unwrap()
        ).unwrap();
        assert!(header_json.contains("HS256"));
        assert!(header_json.contains("JWT"));

        // Verify payload
        let payload_json = String::from_utf8(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(parts[1])
                .unwrap()
        ).unwrap();
        assert!(payload_json.contains("lumina"));
    }

    #[test]
    fn test_jwt_generation_without_secret() {
        let client = ChordClient::new(
            "http://localhost:8099".to_string(),
            "".to_string(), // Empty secret
        );

        let jwt_result = client.generate_jwt();
        assert!(jwt_result.is_none(), "Should skip JWT generation when secret is empty");
    }

    #[test]
    fn test_chat_message_serialization() {
        let message = ChatMessage::text("user", "Hello");

        let json = serde_json::to_string(&message).unwrap();
        assert!(json.contains("\"role\":\"user\""));
        assert!(json.contains("\"content\":\"Hello\""));
    }

    #[test]
    fn test_chat_completion_request_serialization() {
        let request = ChatCompletionRequest {
            model: "lumina".to_string(),
            messages: vec![ChatMessage::text("user", "Test")],
            tools: None,
            stream: Some(false),
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"model\":\"lumina\""));
        assert!(json.contains("\"stream\":false"));
    }

    #[test]
    fn test_chat_completion_response_deserialization() {
        let response_json = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Hello there!"
                }
            }]
        });

        let response: ChatCompletionResponse = serde_json::from_value(response_json).unwrap();
        assert_eq!(response.choices.len(), 1);
        assert_eq!(response.choices[0].message.content.as_deref(), Some("Hello there!"));
    }

    #[test]
    fn test_chord_error_response_deserialization() {
        let error_json = json!({
            "error": "authentication required"
        });

        let error: ChordErrorResponse = serde_json::from_value(error_json).unwrap();
        assert_eq!(error.error, "authentication required");
    }

    // ── ESEC-02: KV cache isolation tests ─────────────────────────────────────

    #[test]
    fn test_kv_cache_buster_different_users_get_different_tokens() {
        let token_a = kv_cache_buster("user-alice", "session-1");
        let token_b = kv_cache_buster("user-bob", "session-1");
        assert_ne!(token_a, token_b, "Different users must get different cache-buster tokens");
    }

    #[test]
    fn test_kv_cache_buster_same_user_same_session_stable() {
        let token1 = kv_cache_buster("user-alice", "session-abc");
        let token2 = kv_cache_buster("user-alice", "session-abc");
        assert_eq!(token1, token2, "Same user + same session must get same token (enables cache reuse)");
    }

    #[test]
    fn test_kv_cache_buster_different_sessions_get_different_tokens() {
        let token_s1 = kv_cache_buster("user-alice", "session-1");
        let token_s2 = kv_cache_buster("user-alice", "session-2");
        assert_ne!(token_s1, token_s2, "Different sessions must get different tokens");
    }

    #[test]
    fn test_kv_cache_buster_format() {
        let token = kv_cache_buster("user-alice", "session-1");
        assert_eq!(token.len(), 8, "Cache-buster token must be 8 chars (4 bytes → 8 hex): {token}");
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()), "Token must be hex: {token}");
    }

    #[test]
    fn test_inject_cache_buster_appends_to_system_message() {
        let mut messages = vec![
            ChatMessage::text("system", "You are Lumina."),
            ChatMessage::text("user", "Hello"),
        ];
        inject_cache_buster_into_messages(&mut messages, "user-alice", "session-1");

        let system_content = messages[0].content.as_deref().unwrap_or("");
        assert!(system_content.starts_with("You are Lumina."), "Original content preserved");
        assert!(system_content.contains("[session:"), "Cache-buster appended");
        assert!(system_content.contains(']'), "Cache-buster token closed");
    }

    #[test]
    fn test_inject_cache_buster_does_not_modify_user_messages() {
        let mut messages = vec![
            ChatMessage::text("system", "You are Lumina."),
            ChatMessage::text("user", "Hello"),
        ];
        inject_cache_buster_into_messages(&mut messages, "user-alice", "session-1");

        let user_content = messages[1].content.as_deref().unwrap_or("");
        assert_eq!(user_content, "Hello", "User message must not be modified");
    }

    #[test]
    fn test_inject_cache_buster_no_system_message_safe() {
        let mut messages = vec![
            ChatMessage::text("user", "Hello"),
        ];
        // Should not panic or modify anything when no system message exists
        inject_cache_buster_into_messages(&mut messages, "user-alice", "session-1");
        assert_eq!(messages[0].content.as_deref().unwrap_or(""), "Hello", "User message unchanged");
    }

    #[test]
    fn test_inject_cache_buster_different_users_get_different_tokens_in_system() {
        let mut msgs_alice = vec![ChatMessage::text("system", "Base prompt")];
        let mut msgs_bob = vec![ChatMessage::text("system", "Base prompt")];
        inject_cache_buster_into_messages(&mut msgs_alice, "user-alice", "session-1");
        inject_cache_buster_into_messages(&mut msgs_bob, "user-bob", "session-1");

        let alice_content = msgs_alice[0].content.as_deref().unwrap_or("");
        let bob_content = msgs_bob[0].content.as_deref().unwrap_or("");
        assert_ne!(alice_content, bob_content,
            "Different users must get different system prompts to force different KV cache slots");
    }

    #[tokio::test]
    async fn test_chord_client_creation() {
        let client = ChordClient::new(
            "http://localhost:8099".to_string(),
            "test-secret".to_string(),
        );

        assert_eq!(client.base_url, "http://localhost:8099");
        assert_eq!(client.jwt_secret, "test-secret");
    }

    // Mock server tests
    #[tokio::test]
    async fn test_successful_chat_completion_with_auth() {
        let mock_server = httpmock::MockServer::start();

        let mock = mock_server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/chat/completions")
                .header_exists("Authorization") // JWT should be present
                .header("Content-Type", "application/json");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(json!({
                    "choices": [{
                        "message": {
                            "role": "assistant",
                            "content": "Hello from mock server!"
                        }
                    }]
                }));
        });

        let client = ChordClient::new(mock_server.base_url(), "test-secret".to_string());

        let messages = vec![ChatMessage::text("user", "Hello")];

        let result = client.chat_completion(messages).await.unwrap();
        assert_eq!(result.as_str(), "Hello from mock server!");

        mock.assert();
    }

    #[tokio::test]
    async fn test_successful_chat_completion_without_auth() {
        let mock_server = httpmock::MockServer::start();

        let mock = mock_server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/chat/completions")
                .header("Content-Type", "application/json");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(json!({
                    "choices": [{
                        "message": {
                            "role": "assistant",
                            "content": "Hello from mock server!"
                        }
                    }]
                }));
        });

        let client = ChordClient::new(mock_server.base_url(), "".to_string()); // No secret

        let messages = vec![ChatMessage::text("user", "Hello")];

        let result = client.chat_completion(messages).await.unwrap();
        assert_eq!(result.as_str(), "Hello from mock server!");

        mock.assert();
    }

    #[tokio::test]
    async fn test_authentication_error() {
        let mock_server = httpmock::MockServer::start();

        let mock = mock_server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/chat/completions");
            then.status(401)
                .header("Content-Type", "application/json")
                .json_body(json!({
                    "error": "authentication required"
                }));
        });

        let client = ChordClient::new(mock_server.base_url(), "bad-secret".to_string());

        let messages = vec![ChatMessage::text("user", "Hello")];

        let result = client.chat_completion(messages).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), LuminaError::Chord(msg) if msg.contains("Authentication failed")));

        mock.assert();
    }

    #[tokio::test]
    async fn test_server_error() {
        let mock_server = httpmock::MockServer::start();

        let mock = mock_server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/chat/completions");
            then.status(500);
        });

        let client = ChordClient::new(mock_server.base_url(), "test-secret".to_string());

        let messages = vec![ChatMessage::text("user", "Hello")];

        let result = client.chat_completion(messages).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), LuminaError::Chord(msg) if msg.contains("server error")));

        mock.assert();
    }

    #[tokio::test]
    async fn test_malformed_json_response() {
        let mock_server = httpmock::MockServer::start();

        let mock = mock_server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/chat/completions");
            then.status(200)
                .header("Content-Type", "application/json")
                .body("invalid json");
        });

        let client = ChordClient::new(mock_server.base_url(), "test-secret".to_string());

        let messages = vec![ChatMessage::text("user", "Hello")];

        let result = client.chat_completion(messages).await;
        assert!(result.is_err());

        // Check if it's a parse error or network error (both are valid for malformed JSON)
        match result.unwrap_err() {
            LuminaError::Parse(_) | LuminaError::Network(_) => {
                // Either is acceptable - different HTTP clients handle malformed JSON differently
            }
            other => panic!("Expected Parse or Network error, got: {:?}", other),
        }

        mock.assert();
    }

    #[tokio::test]
    async fn test_empty_choices() {
        let mock_server = httpmock::MockServer::start();

        let mock = mock_server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/chat/completions");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(json!({
                    "choices": []
                }));
        });

        let client = ChordClient::new(mock_server.base_url(), "test-secret".to_string());

        let messages = vec![ChatMessage::text("user", "Hello")];

        let result = client.chat_completion(messages).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), LuminaError::Chord(msg) if msg.contains("No response choices")));

        mock.assert();
    }

    // ── CHORD-04: Tool proxy method tests ─────────────────────────────────────

    #[tokio::test]
    async fn test_tool_list_makes_correct_post_and_parses_response() {
        let mock_server = httpmock::MockServer::start();

        let mock = mock_server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/tools/list")
                .header_exists("Authorization")
                .header("Content-Type", "application/json");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(json!({
                    "tools": [
                        {
                            "name": "nexus_send",
                            "description": "Send a message to the Nexus inbox",
                            "parameters": {"type": "object", "properties": {"to": {"type": "string"}}},
                            "source": "terminus"
                        },
                        {
                            "name": "engram_query",
                            "description": "Query Engram memory store",
                            "parameters": {"type": "object", "properties": {}},
                            "source": "terminus"
                        }
                    ]
                }));
        });

        let client = ChordClient::new(mock_server.base_url(), "test-secret".to_string());
        let tools = client.tool_list().await.unwrap();

        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "nexus_send");
        assert_eq!(tools[1].name, "engram_query");
        assert!(tools[0].description.contains("Nexus inbox"));
        mock.assert();
    }

    #[tokio::test]
    async fn test_tool_call_sends_correct_body_and_returns_result() {
        let mock_server = httpmock::MockServer::start();

        let mock = mock_server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/tools/call")
                .header_exists("Authorization")
                .header("Content-Type", "application/json")
                .json_body(json!({
                    "name": "nexus_send",
                    "arguments": {"to": "axon", "message": "hello"}
                }));
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(json!({"result": "Message sent to axon"}));
        });

        let client = ChordClient::new(mock_server.base_url(), "test-secret".to_string());
        let result = client.tool_call(
            "nexus_send",
            json!({"to": "axon", "message": "hello"}),
        ).await.unwrap();

        assert_eq!(result, "Message sent to axon");
        mock.assert();
    }

    #[tokio::test]
    async fn test_tool_call_accepts_content_field() {
        let mock_server = httpmock::MockServer::start();

        let mock = mock_server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/tools/call");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(json!({"content": "Tool output via content field"}));
        });

        let client = ChordClient::new(mock_server.base_url(), "".to_string());
        let result = client.tool_call("some_tool", json!({})).await.unwrap();
        assert_eq!(result, "Tool output via content field");
        mock.assert();
    }

    #[tokio::test]
    async fn test_tool_call_failure_returns_clean_lumina_error() {
        let mock_server = httpmock::MockServer::start();

        let mock = mock_server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/tools/call");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(json!({"error": "Tool 'unknown_tool' not found"}));
        });

        let client = ChordClient::new(mock_server.base_url(), "".to_string());
        let result = client.tool_call("unknown_tool", json!({})).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(&err, LuminaError::Chord(msg) if msg.contains("unknown_tool")));
        mock.assert();
    }

    #[tokio::test]
    async fn test_tool_discover_sends_query_and_max_results() {
        let mock_server = httpmock::MockServer::start();

        let mock = mock_server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/tools/discover")
                .header_exists("Authorization")
                .header("Content-Type", "application/json")
                .json_body(json!({
                    "query": "send matrix message",
                    "max_results": 5
                }));
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(json!({
                    "tools": [
                        {
                            "name": "matrix_send",
                            "description": "Send a Matrix message",
                            "parameters": {"type": "object", "properties": {}},
                            "source": "terminus"
                        }
                    ]
                }));
        });

        let client = ChordClient::new(mock_server.base_url(), "test-secret".to_string());
        let tools = client.tool_discover("send matrix message", 5).await.unwrap();

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "matrix_send");
        mock.assert();
    }

    #[tokio::test]
    async fn test_tool_list_jwt_auth_included() {
        let mock_server = httpmock::MockServer::start();

        let mock = mock_server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/tools/list")
                .header_exists("Authorization"); // JWT must be present
            then.status(200)
                .json_body(json!({"tools": []}));
        });

        let client = ChordClient::new(mock_server.base_url(), "my-secret".to_string());
        let tools = client.tool_list().await.unwrap();
        assert!(tools.is_empty());
        mock.assert();
    }

    #[tokio::test]
    async fn test_tool_discover_jwt_auth_included() {
        let mock_server = httpmock::MockServer::start();

        let mock = mock_server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/tools/discover")
                .header_exists("Authorization");
            then.status(200)
                .json_body(json!({"tools": []}));
        });

        let client = ChordClient::new(mock_server.base_url(), "my-secret".to_string());
        let tools = client.tool_discover("calendar", 3).await.unwrap();
        assert!(tools.is_empty());
        mock.assert();
    }

    #[tokio::test]
    async fn test_tool_list_auth_failure_returns_chord_error() {
        let mock_server = httpmock::MockServer::start();

        let mock = mock_server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/v1/tools/list");
            then.status(401).json_body(json!({"error": "unauthorized"}));
        });

        let client = ChordClient::new(mock_server.base_url(), "bad-secret".to_string());
        let result = client.tool_list().await;
        assert!(matches!(result, Err(LuminaError::Chord(msg)) if msg.contains("Authentication failed")));
        mock.assert();
    }

    #[tokio::test]
    async fn test_tool_list_empty_tools_array() {
        let mock_server = httpmock::MockServer::start();
        let mock = mock_server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/v1/tools/list");
            then.status(200).json_body(json!({"tools": []}));
        });

        let client = ChordClient::new(mock_server.base_url(), "".to_string());
        let tools = client.tool_list().await.unwrap();
        assert!(tools.is_empty());
        mock.assert();
    }

    #[tokio::test]
    async fn test_tool_list_skips_tools_with_empty_name() {
        let mock_server = httpmock::MockServer::start();
        let mock = mock_server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/v1/tools/list");
            then.status(200).json_body(json!({
                "tools": [
                    {"name": "", "description": "unnamed", "parameters": {}},
                    {"name": "valid_tool", "description": "has name", "parameters": {}}
                ]
            }));
        });

        let client = ChordClient::new(mock_server.base_url(), "".to_string());
        let tools = client.tool_list().await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "valid_tool");
        mock.assert();
    }

    // FORGE-02: dynamic model name tests
    #[tokio::test]
    async fn test_chat_with_dynamic_model() {
        let mock_server = httpmock::MockServer::start();
        let mock = mock_server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/chat/completions");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(json!({
                    "choices": [{"message": {"role": "assistant", "content": "fast response"}}]
                }));
        });

        let client = ChordClient::new(mock_server.base_url(), "".to_string());
        let result = client.chat("lumina-fast", "hello").await.unwrap();
        assert_eq!(result.as_str(), "fast response");
        mock.assert();
    }

    #[tokio::test]
    async fn test_chat_completion_with_model_deep() {
        let mock_server = httpmock::MockServer::start();
        let mock = mock_server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/chat/completions");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(json!({
                    "choices": [{"message": {"role": "assistant", "content": "deep response"}}]
                }));
        });

        let client = ChordClient::new(mock_server.base_url(), "".to_string());
        let messages = vec![ChatMessage::text("user".to_string(), "analyze this".to_string())];
        let result = client.chat_completion_with_model("lumina-deep", messages).await.unwrap();
        assert_eq!(result.as_str(), "deep response");
        mock.assert();
    }

    // ── AGENT-02: agentic_execute tests ──────────────────────────────────────

    #[tokio::test]
    async fn test_agentic_execute_success_returns_response_and_logs() {
        let mock_server = httpmock::MockServer::start();

        let mock = mock_server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/agent/execute")
                .header("Content-Type", "application/json");
            then.status(200)
                .header("Content-Type", "text/event-stream")
                .body(concat!(
                    "data: {\"type\":\"started\"}\n\n",
                    "data: {\"type\":\"tool_call_started\",\"tool_name\":\"utc_now\",\"step_number\":1}\n\n",
                    "data: {\"type\":\"tool_call_complete\",\"tool_name\":\"utc_now\",\"duration_ms\":15,\"status\":\"ok\"}\n\n",
                    "data: {\"type\":\"complete\",\"response\":\"The time is 12:00 UTC.\"}\n\n",
                ));
        });

        let client = ChordClient::new(mock_server.base_url(), "".to_string());
        let mut tool_signals: Vec<String> = Vec::new();
        let result = client.agentic_execute(
            vec![AgenticMessage { role: "user".into(), content: "What time is it?".into(), tool_call_id: None }],
            "You are Lumina.".into(),
            vec![AgenticToolDef { name: "utc_now".into(), description: "UTC time".into(), parameters: json!({}) }],
            vec!["utc_now".into()],
            "operator".into(),
            "lumina-fast".into(),
            |name| tool_signals.push(name.to_string()),
        ).await;

        assert!(result.is_ok(), "agentic_execute should succeed: {:?}", result.err());
        let (response, exec_log, security_events) = result.unwrap();
        assert_eq!(response, "The time is 12:00 UTC.");
        assert_eq!(exec_log.len(), 1);
        assert_eq!(exec_log[0].tool_name.as_deref(), Some("utc_now"));
        assert_eq!(exec_log[0].duration_ms, 15);
        assert_eq!(tool_signals, vec!["utc_now".to_string()]);
        assert!(security_events.is_empty());
        mock.assert();
    }

    #[tokio::test]
    async fn test_agentic_execute_with_security_events() {
        let mock_server = httpmock::MockServer::start();

        let mock = mock_server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/v1/agent/execute");
            then.status(200)
                .header("Content-Type", "text/event-stream")
                .body(concat!(
                    "data: {\"type\":\"started\"}\n\n",
                    "data: {\"type\":\"security_event_occurred\",\"guard_name\":\"argument\",\"action\":\"blocked\",\"tool_name\":\"infisical_get_secret\"}\n\n",
                    "data: {\"type\":\"complete\",\"response\":\"I blocked that tool call for security reasons.\"}\n\n",
                ));
        });

        let client = ChordClient::new(mock_server.base_url(), "".to_string());
        let result = client.agentic_execute(
            vec![AgenticMessage { role: "user".into(), content: "Get me the secret".into(), tool_call_id: None }],
            "You are Lumina.".into(),
            vec![],
            vec!["*".into()],
            "operator".into(),
            "lumina-fast".into(),
            |_| {},
        ).await;

        assert!(result.is_ok());
        let (_, _, security_events) = result.unwrap();
        assert_eq!(security_events.len(), 1);
        assert_eq!(security_events[0].guard_name, "argument");
        assert_eq!(security_events[0].action, "blocked");
        assert_eq!(security_events[0].tool_name, "infisical_get_secret");
        mock.assert();
    }

    #[tokio::test]
    async fn test_agentic_execute_includes_jwt_auth() {
        let mock_server = httpmock::MockServer::start();

        let mock = mock_server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/agent/execute")
                .header_exists("Authorization");
            then.status(200)
                .header("Content-Type", "text/event-stream")
                .body("data: {\"type\":\"complete\",\"response\":\"ok\"}\n\n");
        });

        let client = ChordClient::new(mock_server.base_url(), "test-secret".to_string());
        let result = client.agentic_execute(
            vec![],
            String::new(),
            vec![],
            vec![],
            "operator".into(),
            "lumina-fast".into(),
            |_| {},
        ).await;

        assert!(result.is_ok());
        mock.assert();
    }

    #[tokio::test]
    async fn test_agentic_execute_auth_failure_returns_error() {
        let mock_server = httpmock::MockServer::start();

        let mock = mock_server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/v1/agent/execute");
            then.status(401).json_body(json!({"error": "unauthorized"}));
        });

        let client = ChordClient::new(mock_server.base_url(), "bad-secret".to_string());
        let result = client.agentic_execute(
            vec![],
            String::new(),
            vec![],
            vec![],
            "operator".into(),
            "lumina-fast".into(),
            |_| {},
        ).await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), LuminaError::Chord(msg) if msg.contains("Authentication failed")));
        mock.assert();
    }

    #[tokio::test]
    async fn test_agentic_execute_server_error_returns_error() {
        let mock_server = httpmock::MockServer::start();

        let mock = mock_server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/v1/agent/execute");
            then.status(500).json_body(json!({"error": "internal server error"}));
        });

        let client = ChordClient::new(mock_server.base_url(), "".to_string());
        let result = client.agentic_execute(
            vec![],
            String::new(),
            vec![],
            vec![],
            "operator".into(),
            "lumina-fast".into(),
            |_| {},
        ).await;

        assert!(result.is_err());
        mock.assert();
    }

    #[tokio::test]
    async fn test_agentic_execute_empty_security_events_and_log_on_no_tools() {
        let mock_server = httpmock::MockServer::start();

        let mock = mock_server.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/v1/agent/execute");
            then.status(200)
                .header("Content-Type", "text/event-stream")
                .body(concat!(
                    "data: {\"type\":\"started\"}\n\n",
                    "data: {\"type\":\"complete\",\"response\":\"simple text response\"}\n\n",
                ));
        });

        let client = ChordClient::new(mock_server.base_url(), "".to_string());
        let result = client.agentic_execute(
            vec![AgenticMessage { role: "user".into(), content: "hello".into(), tool_call_id: None }],
            "You are Lumina.".into(),
            vec![],
            vec![],
            "operator".into(),
            "lumina-fast".into(),
            |_| {},
        ).await;

        assert!(result.is_ok());
        let (response, exec_log, security_events) = result.unwrap();
        assert_eq!(response, "simple text response");
        assert!(exec_log.is_empty());
        assert!(security_events.is_empty());
        mock.assert();
    }

    #[test]
    fn test_agentic_message_serialization() {
        let msg = AgenticMessage {
            role: "user".into(),
            content: "hello".into(),
            tool_call_id: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"role\":\"user\""));
        assert!(json.contains("\"content\":\"hello\""));
        // tool_call_id is None — should not appear (skip_serializing_if)
        assert!(!json.contains("tool_call_id"));
    }

    #[test]
    fn test_agentic_tool_def_serialization() {
        let def = AgenticToolDef {
            name: "utc_now".into(),
            description: "Returns UTC time".into(),
            parameters: json!({"type": "object", "properties": {}}),
        };
        let json = serde_json::to_string(&def).unwrap();
        assert!(json.contains("\"name\":\"utc_now\""));
        assert!(json.contains("\"description\""));
    }

    #[test]
    fn test_agentic_security_event_roundtrip() {
        let event = AgenticSecurityEvent {
            guard_name: "argument".into(),
            action: "blocked".into(),
            tool_name: "infisical_get_secret".into(),
            reason: "credential pattern".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: AgenticSecurityEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back.guard_name, "argument");
        assert_eq!(back.action, "blocked");
        assert_eq!(back.tool_name, "infisical_get_secret");
    }

    #[test]
    fn test_agentic_execution_step_no_args_or_results() {
        let step = AgenticExecutionStep {
            step_type: "tool_call".into(),
            tool_name: Some("utc_now".into()),
            duration_ms: 42,
            status: "ok".into(),
            error_message: None,
            research_source: None,
        };
        let json_val = serde_json::to_value(&step).unwrap();
        let obj = json_val.as_object().unwrap();
        assert!(!obj.contains_key("args"), "args must not be present in execution step");
        assert!(!obj.contains_key("result"), "result must not be present in execution step");
        assert!(!obj.contains_key("arguments"), "arguments must not be present in execution step");
    }
}