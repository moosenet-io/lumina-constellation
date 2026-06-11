//! MCP transport layer — SSH stdio (P1-10) and StreamableHTTP.
//!
//! # CHORD-04: Emergency-only bypass
//!
//! Under normal operation all tool calls route through the Chord proxy
//! (`ChordClient::tool_list`, `tool_call`, `tool_discover` in `chord.rs`).
//! This module is retained as an **emergency-only bypass** for situations where
//! the Chord proxy is unavailable and direct MCP access is required for recovery.
//! `MCP_URL` is NOT required in normal deployments.
//!
//! [`McpTransport`] is an enum that wraps two transport backends:
//! - **HTTP** (`MCP_URL` env var set) — FastMCP StreamableHTTP.
//!   Used for emergency direct access when Chord is down.
//! - **Stdio** (`TERMINUS_HOST` env var set) — SSH child process to `stdio.sh`.
//!   Fallback when HTTP is unavailable or unconfigured.
//!
//! Selection logic in [`McpTransport::connect`]:
//! 1. If `MCP_URL` is set → attempt HTTP transport.
//! 2. On HTTP failure, or if `MCP_URL` unset → attempt SSH stdio.
//! 3. Both fail → return the HTTP error (more informative).
//!
//! # HTTP transport protocol
//! FastMCP StreamableHTTP (MCP 2024-11-05):
//! - POST `/mcp` with JSON-RPC body.
//! - `initialize` response carries `Mcp-Session-Id` header.
//! - All subsequent requests include `Mcp-Session-Id`.
//! - `notifications/initialized` returns 200/202 with empty body.
//! - `tools/list` / `tools/call` return SSE-framed JSON (`data: {...}\n\n`)
//!   or plain JSON, depending on FastMCP version.

use crate::config::Config;
use crate::error::{LuminaError, Result};
use crate::tool_types::{ToolDefinition, ToolPermission, ToolResult};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

// ── JSON-RPC wire types ────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: u64,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    #[allow(dead_code)]
    jsonrpc: String,
    id: Option<u64>,
    result: Option<Value>,
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

// ── HTTP transport ─────────────────────────────────────────────────────────

/// FastMCP StreamableHTTP transport.
///
/// The URL is taken from the `MCP_URL` environment variable at connect time.
/// No URL is ever hardcoded here.
struct HttpTransport {
    url: String,
    session_id: String,
}

impl HttpTransport {
    fn connect(url: &str) -> Result<Self> {
        let mut t = Self {
            url: url.to_string(),
            session_id: String::new(),
        };

        let init_params = serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "clientInfo": { "name": "lumina-core", "version": "0.5.0" }
        });
        let (sid, _) = t.post_json("initialize", Some(init_params))?;
        t.session_id = sid.ok_or_else(|| {
            LuminaError::Config("MCP HTTP: server did not return Mcp-Session-Id".to_string())
        })?;

        // notifications/initialized has no response body — fire and ignore parse result
        let _ = t.post_json("notifications/initialized", None);
        Ok(t)
    }

    /// POST a JSON-RPC message to the MCP endpoint.
    /// Returns `(Option<new_session_id>, response_value)`.
    /// A `None` response value means the server returned an empty body (normal for notifications).
    fn post_json(&self, method: &str, params: Option<Value>) -> Result<(Option<String>, Value)> {
        let id = next_id();
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });
        let body_bytes = serde_json::to_vec(&body)
            .map_err(|e| LuminaError::Config(format!("MCP HTTP serialize: {e}")))?;

        // Synchronous HTTP call via block_in_place so we can use async reqwest
        // without requiring a separate blocking client feature.
        let url = self.url.clone();
        let session_id = self.session_id.clone();
        let expected_id = id;

        let (new_sid, resp_body) = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                let client = reqwest::Client::builder()
                    .timeout(Duration::from_secs(30))
                    .build()
                    .map_err(LuminaError::Network)?;

                let mut req = client
                    .post(&url)
                    .header("Content-Type", "application/json")
                    .header("Accept", "application/json, text/event-stream")
                    .body(body_bytes);
                if !session_id.is_empty() {
                    req = req.header("Mcp-Session-Id", &session_id);
                }

                let resp = req.send().await.map_err(LuminaError::Network)?;
                let new_sid = resp
                    .headers()
                    .get("Mcp-Session-Id")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);

                let text = resp.text().await.map_err(LuminaError::Network)?;
                Ok::<(Option<String>, String), LuminaError>((new_sid, text))
            })
        })?;

        // Parse response: strip SSE framing if present, then JSON decode.
        let json_str = strip_sse(&resp_body);
        if json_str.trim().is_empty() {
            return Ok((new_sid, Value::Null));
        }

        let rpc: JsonRpcResponse = serde_json::from_str(json_str)
            .map_err(|e| LuminaError::Config(format!("MCP HTTP parse: {e} — body: {}", &resp_body[..resp_body.len().min(200)])))?;

        if let Some(err) = rpc.error {
            return Err(LuminaError::Config(format!(
                "MCP JSON-RPC error {}: {}",
                err.code, err.message
            )));
        }

        // Verify response ID matches request ID (skip server notifications with id=null)
        if let Some(resp_id) = rpc.id {
            if resp_id != expected_id {
                return Err(LuminaError::Config(format!(
                    "MCP HTTP: response ID mismatch (expected {expected_id}, got {resp_id})"
                )));
            }
        }

        Ok((new_sid, rpc.result.unwrap_or(Value::Null)))
    }

    fn request(&self, method: &str, params: Option<Value>) -> Result<Value> {
        let (_, value) = self.post_json(method, params)?;
        Ok(value)
    }

    fn notify(&self, method: &str, params: Option<Value>) -> Result<()> {
        // Fire-and-forget; server may return empty body.
        let _ = self.post_json(method, params);
        Ok(())
    }
}

/// Strip Server-Sent Events framing: return the last `data: ` line's payload,
/// or the raw string if no `data: ` lines are found.
fn strip_sse(body: &str) -> &str {
    let mut last_data: Option<&str> = None;
    for line in body.lines() {
        if let Some(payload) = line.strip_prefix("data: ") {
            last_data = Some(payload);
        }
    }
    last_data.unwrap_or(body)
}

// ── SSH stdio transport ────────────────────────────────────────────────────

struct StdioTransport {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    /// Temp SSH key file; removed in Drop.
    key_path: Option<std::path::PathBuf>,
}

impl StdioTransport {
    fn connect(config: &Config) -> Result<Self> {
        let host = config.terminus_host();
        if host.is_empty() {
            return Err(LuminaError::Config(
                "TERMINUS_HOST not set — cannot connect to MCP via SSH stdio".to_string(),
            ));
        }

        let key_path = Self::write_temp_key()?;

        let mut child = Command::new("ssh")
            .args([
                "-i", &key_path.display().to_string(),
                "-o", "StrictHostKeyChecking=no",
                "-o", "BatchMode=yes",
                "-o", "ConnectTimeout=10",
                &host,
                "stdio.sh",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| LuminaError::Config(format!("SSH spawn failed: {e}")))?;

        let stdin = child.stdin.take()
            .ok_or_else(|| LuminaError::Config("No stdin on SSH child".to_string()))?;
        let stdout = child.stdout.take()
            .ok_or_else(|| LuminaError::Config("No stdout on SSH child".to_string()))?;

        let mut t = Self {
            child,
            stdin,
            reader: BufReader::new(stdout),
            key_path: Some(key_path),
        };

        let init_params = serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "clientInfo": { "name": "lumina-core", "version": "0.5.0" }
        });
        t.request("initialize", Some(init_params))?;
        t.notify("notifications/initialized", None)?;
        Ok(t)
    }

    fn request(&mut self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = next_id();
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.to_string(),
            params,
        };
        let mut line = serde_json::to_string(&req)
            .map_err(|e| LuminaError::Config(format!("JSON-RPC serialize: {e}")))?;
        line.push('\n');

        self.stdin.write_all(line.as_bytes())
            .map_err(|e| LuminaError::Config(format!("JSON-RPC write: {e}")))?;
        self.stdin.flush()
            .map_err(|e| LuminaError::Config(format!("JSON-RPC flush: {e}")))?;

        let mut buf = String::new();
        loop {
            buf.clear();
            let n = self.reader.read_line(&mut buf)
                .map_err(|e| LuminaError::Config(format!("JSON-RPC read: {e}")))?;
            if n == 0 {
                return Err(LuminaError::Config("MCP stdio: EOF from Terminus".to_string()));
            }
            let trimmed = buf.trim();
            if trimmed.is_empty() { continue; }

            let resp: JsonRpcResponse = serde_json::from_str(trimmed)
                .map_err(|e| LuminaError::Config(format!("JSON-RPC parse: {e}")))?;

            match resp.id {
                Some(resp_id) if resp_id == id => {
                    if let Some(err) = resp.error {
                        return Err(LuminaError::Config(format!(
                            "JSON-RPC error {}: {}", err.code, err.message
                        )));
                    }
                    return Ok(resp.result.unwrap_or(Value::Null));
                }
                None => continue,
                _ => continue,
            }
        }
    }

    fn notify(&mut self, method: &str, params: Option<Value>) -> Result<()> {
        let notif = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });
        let mut line = serde_json::to_string(&notif)
            .map_err(|e| LuminaError::Config(format!("JSON-RPC notify serialize: {e}")))?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes())
            .map_err(|e| LuminaError::Config(format!("JSON-RPC notify write: {e}")))?;
        self.stdin.flush()
            .map_err(|e| LuminaError::Config(format!("JSON-RPC notify flush: {e}")))?;
        Ok(())
    }

    fn write_temp_key() -> Result<std::path::PathBuf> {
        use std::os::unix::fs::OpenOptionsExt;

        let key_val = crate::vault::manager()
            .get("TERMINUS_SSH_KEY")
            .ok_or_else(|| LuminaError::Config(
                "TERMINUS_SSH_KEY not found in vault".to_string(),
            ))?
            .expose_secret()
            .to_string();

        let path = std::env::temp_dir().join(format!(
            "lumina-mcp-key-{}.pem",
            std::process::id()
        ));

        let mut f = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| LuminaError::Config(format!("Key file create: {e}")))?;

        f.write_all(key_val.as_bytes())
            .map_err(|e| LuminaError::Config(format!("Key file write: {e}")))?;
        Ok(path)
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        let _ = self.child.kill();
        if let Some(ref p) = self.key_path {
            let _ = std::fs::remove_file(p);
        }
    }
}

// ── Public enum ────────────────────────────────────────────────────────────

/// MCP transport — HTTP (preferred) or SSH stdio (fallback).
///
/// Consumers use [`McpTransport::connect`] and then call
/// [`list_tools`] / [`call_tool`].  The enum is transparent to callers.
pub enum McpTransport {
    Http(HttpTransport),
    Stdio(StdioTransport),
}

impl McpTransport {
    /// Connect to the MCP hub.
    ///
    /// Tries HTTP first (when `MCP_URL` env var is set), falls back to SSH stdio.
    /// Returns the first successful transport; errors from both are surfaced if
    /// all attempts fail.
    pub fn connect(config: &Config) -> Result<Self> {
        let mcp_url = std::env::var("MCP_URL")
            .ok()
            .filter(|s| !s.trim().is_empty());

        if let Some(url) = mcp_url {
            match HttpTransport::connect(&url) {
                Ok(t) => {
                    log::info!("MCP: connected via HTTP to {}", url);
                    return Ok(McpTransport::Http(t));
                }
                Err(e) => {
                    log::warn!("MCP: HTTP connect to {} failed ({}), trying SSH stdio", url, e);
                }
            }
        }

        StdioTransport::connect(config).map(|t| {
            log::info!("MCP: connected via SSH stdio to {}", config.terminus_host());
            McpTransport::Stdio(t)
        })
    }

    /// Which transport is active.
    pub fn transport_name(&self) -> &'static str {
        match self {
            McpTransport::Http(_) => "http",
            McpTransport::Stdio(_) => "stdio",
        }
    }

    fn request(&mut self, method: &str, params: Option<Value>) -> Result<Value> {
        match self {
            McpTransport::Http(t) => t.request(method, params),
            McpTransport::Stdio(t) => t.request(method, params),
        }
    }

    fn notify(&mut self, method: &str, params: Option<Value>) -> Result<()> {
        match self {
            McpTransport::Http(t) => t.notify(method, params),
            McpTransport::Stdio(t) => t.notify(method, params),
        }
    }
}

// ── Tool discovery ─────────────────────────────────────────────────────────

/// Deny-list prefixes for tools that are treated as destructive (filtered by default).
static DESTRUCTIVE_PREFIXES: &[&str] = &[
    "delete_", "remove_", "destroy_", "drop_", "truncate_",
    "kill_", "stop_", "disable_", "revoke_", "reset_",
    "write_", "create_", "update_", "insert_", "push_", "force_",
];

fn is_destructive(name: &str) -> bool {
    let lower = name.to_lowercase();
    DESTRUCTIVE_PREFIXES.iter().any(|p| lower.starts_with(p))
}

impl McpTransport {
    /// Call `tools/list` and return `ToolDefinition`s.
    ///
    /// Destructive-prefixed tools are filtered out.  All returned tools are
    /// marked `ReadOnly`.
    pub fn list_tools(&mut self) -> Result<Vec<ToolDefinition>> {
        let result = self.request("tools/list", None)?;

        let tools_array = result["tools"]
            .as_array()
            .ok_or_else(|| LuminaError::Config("tools/list: no 'tools' array".to_string()))?;

        let mut defs = Vec::new();
        for tool in tools_array {
            let name = tool["name"].as_str().unwrap_or("").to_string();
            if name.is_empty() || is_destructive(&name) {
                continue;
            }
            let description = tool["description"].as_str().unwrap_or("").to_string();
            let schema = tool["inputSchema"].clone();
            let argument_schema = if schema.is_null() || !schema.is_object() {
                serde_json::json!({ "type": "object", "properties": {} })
            } else {
                schema
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

    /// Call `tools/call` and return a [`ToolResult`].
    pub fn call_tool(
        &mut self,
        tool_call_id: &str,
        name: &str,
        arguments: &Value,
    ) -> Result<ToolResult> {
        let params = serde_json::json!({ "name": name, "arguments": arguments });

        match self.request("tools/call", Some(params)) {
            Ok(result) => {
                if result["isError"].as_bool().unwrap_or(false) {
                    let err_msg = result["content"]
                        .as_array()
                        .and_then(|a| a.iter().find_map(|i| i["text"].as_str()))
                        .unwrap_or("tool returned an error")
                        .to_string();
                    return Ok(ToolResult::error(
                        tool_call_id.to_string(),
                        name.to_string(),
                        err_msg,
                    ));
                }

                let content = result["content"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|item| item["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_else(|| result.to_string());

                Ok(ToolResult::success(
                    tool_call_id.to_string(),
                    name.to_string(),
                    content,
                ))
            }
            Err(e) => Ok(ToolResult::error(
                tool_call_id.to_string(),
                name.to_string(),
                e.to_string(),
            )),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_destructive_filters_correctly() {
        assert!(is_destructive("delete_file"));
        assert!(is_destructive("remove_user"));
        assert!(is_destructive("write_config"));
        assert!(is_destructive("force_restart"));
        assert!(!is_destructive("read_file"));
        assert!(!is_destructive("get_status"));
        assert!(!is_destructive("list_tools"));
    }

    #[test]
    fn test_strip_sse_extracts_data_line() {
        let sse = "event: message\ndata: {\"foo\":1}\n\n";
        assert_eq!(strip_sse(sse), "{\"foo\":1}");
    }

    #[test]
    fn test_strip_sse_returns_raw_for_plain_json() {
        let plain = r#"{"jsonrpc":"2.0","id":1,"result":{}}"#;
        assert_eq!(strip_sse(plain), plain);
    }

    #[test]
    fn test_strip_sse_picks_last_data_line() {
        let multi = "data: {\"partial\":true}\ndata: {\"final\":true}\n\n";
        assert_eq!(strip_sse(multi), "{\"final\":true}");
    }

    #[test]
    fn test_json_rpc_request_serializes_correctly() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: 42,
            method: "tools/list".to_string(),
            params: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"jsonrpc\":\"2.0\""));
        assert!(json.contains("\"id\":42"));
        assert!(!json.contains("\"params\""));
    }

    #[test]
    fn test_json_rpc_error_response_deserializes() {
        let json = r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"Not found"}}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.error.as_ref().unwrap().code, -32601);
    }

    #[test]
    fn test_tool_definition_from_mcp_response() {
        let response_json = serde_json::json!({
            "tools": [
                { "name": "read_file",   "description": "Read", "inputSchema": {"type":"object"} },
                { "name": "delete_file", "description": "Del",  "inputSchema": {"type":"object"} },
                { "name": "get_status",  "description": "Status" }
            ]
        });
        let tools_array = response_json["tools"].as_array().unwrap();
        let mut defs = Vec::new();
        for tool in tools_array {
            let name = tool["name"].as_str().unwrap_or("").to_string();
            if name.is_empty() || is_destructive(&name) { continue; }
            let schema = tool["inputSchema"].clone();
            let argument_schema = if schema.is_null() || !schema.is_object() {
                serde_json::json!({ "type": "object", "properties": {} })
            } else { schema };
            defs.push(ToolDefinition::new(
                name,
                tool["description"].as_str().unwrap_or("").to_string(),
                ToolPermission::ReadOnly,
                argument_schema,
            ));
        }
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0].name, "read_file");
        assert_eq!(defs[1].name, "get_status");
    }

    /// JSON-RPC framing round-trip via a Python echo peer (no SSH needed).
    #[test]
    fn test_json_rpc_framing_round_trip() {
        let echo_script = r#"
import sys, json
for line in sys.stdin:
    line = line.strip()
    if not line: continue
    req = json.loads(line)
    resp = {"jsonrpc":"2.0","id":req.get("id"),"result":{"tools":[]}}
    sys.stdout.write(json.dumps(resp)+'\n')
    sys.stdout.flush()
    break
"#;
        let mut child = match Command::new("python3")
            .arg("-c").arg(echo_script)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
            .spawn() {
            Ok(c) => c,
            Err(_) => return,
        };
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut t = StdioTransport {
            child,
            stdin,
            reader: BufReader::new(stdout),
            key_path: None,
        };
        let result = t.request("tools/list", None);
        assert!(result.is_ok());
        assert!(result.unwrap()["tools"].is_array());
    }
}
