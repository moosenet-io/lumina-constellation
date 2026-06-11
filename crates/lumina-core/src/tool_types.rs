//! GUARD-08: Tool call types and structures
//!
//! Defines OpenAI-compatible function calling structures for MCP tools.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Tool permission level
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ToolPermission {
    /// Read-only operations (safe)
    ReadOnly,
    /// Read-write operations (moderate risk)
    ReadWrite,
    /// Destructive operations (high risk)
    Destructive,
}

impl std::fmt::Display for ToolPermission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolPermission::ReadOnly => write!(f, "read-only"),
            ToolPermission::ReadWrite => write!(f, "read-write"),
            ToolPermission::Destructive => write!(f, "destructive"),
        }
    }
}

/// Tool definition with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Tool name (unique identifier)
    pub name: String,
    /// Human-readable description
    pub description: String,
    /// Permission level required
    pub permission: ToolPermission,
    /// JSON Schema for arguments
    pub argument_schema: serde_json::Value,
}

impl ToolDefinition {
    /// Create new tool definition
    pub fn new(
        name: String,
        description: String,
        permission: ToolPermission,
        argument_schema: serde_json::Value,
    ) -> Self {
        Self {
            name,
            description,
            permission,
            argument_schema,
        }
    }

    /// Create read-only tool
    pub fn read_only(name: String, description: String, argument_schema: serde_json::Value) -> Self {
        Self::new(name, description, ToolPermission::ReadOnly, argument_schema)
    }

    /// Create read-write tool
    pub fn read_write(name: String, description: String, argument_schema: serde_json::Value) -> Self {
        Self::new(name, description, ToolPermission::ReadWrite, argument_schema)
    }

    /// Create destructive tool
    pub fn destructive(name: String, description: String, argument_schema: serde_json::Value) -> Self {
        Self::new(name, description, ToolPermission::Destructive, argument_schema)
    }

    /// Convert to OpenAI-format ChordTool for the tools[] array in chat requests.
    pub fn to_chord_tool(&self) -> crate::chord::ChordTool {
        crate::chord::ChordTool {
            tool_type: "function".to_string(),
            function: crate::chord::ChordToolFunction {
                name: self.name.clone(),
                description: self.description.clone(),
                parameters: self.argument_schema.clone(),
            },
        }
    }
}

/// WEB-02: Return the `web_search` tool definition.
///
/// This tool definition is registered with the tool gate so the agent loop
/// can dispatch `web_search` calls to [`crate::web::search::WebSearch`].
///
/// Arguments schema:
/// - `query` (string, required) — the search query.
/// - `count` (integer, optional, 1–5) — number of results to return (default 3).
pub fn web_search_tool_definition() -> ToolDefinition {
    ToolDefinition::read_only(
        "web_search".to_string(),
        "Search the web using DuckDuckGo and return up to 5 results with title, URL, and snippet. \
         Uses the HTML endpoint — no API key required."
            .to_string(),
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query string."
                },
                "count": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 5,
                    "description": "Number of results to return (1–5, default 3)."
                }
            },
            "required": ["query"],
            "additionalProperties": false
        }),
    )
}

/// OpenAI-compatible function call structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Unique identifier for this call
    pub id: String,
    /// Type of tool call (always "function")
    #[serde(rename = "type")]
    pub call_type: String,
    /// Function call details
    pub function: FunctionCall,
}

impl ToolCall {
    /// Create new tool call
    pub fn new(id: String, name: String, arguments: String) -> Self {
        Self {
            id,
            call_type: "function".to_string(),
            function: FunctionCall {
                name,
                arguments,
            },
        }
    }
}

/// Function call details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    /// Name of the function to call
    pub name: String,
    /// JSON string of arguments
    pub arguments: String,
}

/// Tool execution result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// Tool call ID this result responds to
    pub tool_call_id: String,
    /// Function name that was called
    pub function_name: String,
    /// Result content (success or error)
    pub content: String,
    /// Whether the call succeeded
    pub success: bool,
    /// Optional error message
    pub error: Option<String>,
}

impl ToolResult {
    /// Create successful tool result
    pub fn success(tool_call_id: String, function_name: String, content: String) -> Self {
        Self {
            tool_call_id,
            function_name,
            content,
            success: true,
            error: None,
        }
    }

    /// Create error tool result.
    ///
    /// The `content` is the string the LLM actually sees in the tool-result
    /// message, so it is marked emphatically and names the failing tool —
    /// e.g. `TOOL FAILED: searxng_search — blocked for safety reasons`. This
    /// makes failure unambiguous so the model never mistakes an empty/error
    /// result for a real answer and fabricates substitute content.
    pub fn error(tool_call_id: String, function_name: String, error_message: String) -> Self {
        let content = format!(
            "TOOL FAILED: {} — {}. Do not invent or substitute information; \
             tell the user the tool did not succeed and offer to retry.",
            function_name, error_message
        );
        Self {
            tool_call_id,
            function_name,
            content,
            success: false,
            error: Some(error_message),
        }
    }
}

/// EDGE-01: WASM sandbox capability grant.
///
/// Each variant enables a specific class of host interaction.
/// Tools default to zero capabilities (deny by default).
/// Implements serde so capabilities can be loaded from `tools.toml` config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WasmCapability {
    /// Read/write access to specific filesystem paths (path-jailed)
    Filesystem { paths: Vec<std::path::PathBuf> },
    /// Outbound network access limited to specific hostnames
    Network { hosts: Vec<String> },
    /// Tool can produce stdout output (required to capture output)
    Stdout,
    /// Access to specific environment variable keys (values supplied at call time, never from host env)
    Env { keys: Vec<String> },
}

// ── Finance tool definitions ──────────────────────────────────────────────────

/// Returns a ready-to-use `ToolDefinition` for the `stock_quote` MCP tool.
///
/// This tool fetches a single stock/ETF quote (price, change, change_pct)
/// via the finance adapters (Alphavantage → Finnhub fallback).
///
/// API keys are resolved from environment variables at runtime and are
/// **never** included in tool arguments or logged.
pub fn stock_quote_tool() -> ToolDefinition {
    ToolDefinition::read_only(
        "stock_quote".to_string(),
        "Fetch a stock or ETF quote (price, change, change_pct) for the given symbol. \
         Tries Alphavantage first, falls back to Finnhub. \
         Results are cached for 5 minutes. \
         API keys are supplied via server environment variables — never pass keys as arguments."
            .to_string(),
        serde_json::json!({
            "type": "object",
            "properties": {
                "symbol": {
                    "type": "string",
                    "description": "Ticker symbol (e.g. \"AAPL\", \"SPY\"). Case-insensitive."
                }
            },
            "required": ["symbol"],
            "additionalProperties": false
        }),
    )
}

/// Returns a ready-to-use `ToolDefinition` for the `market_summary` MCP tool.
///
/// This tool fetches index proxy quotes for SPY (S&P 500), QQQ (NASDAQ 100),
/// and DIA (Dow Jones) as a quick market overview.
///
/// Results are cached for 15 minutes. API keys come from server environment
/// variables and are never logged.
pub fn market_summary_tool() -> ToolDefinition {
    ToolDefinition::read_only(
        "market_summary".to_string(),
        "Fetch a brief market summary: SPY (S&P 500), QQQ (NASDAQ 100), and DIA (Dow Jones) \
         index-proxy quotes. Results are cached for 15 minutes. \
         No arguments required."
            .to_string(),
        serde_json::json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false
        }),
    )
}

/// Tool allowlist configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolAllowlist {
    /// Map of tool name to allowed permission level
    pub tools: HashMap<String, ToolPermission>,
    /// Default behavior for unlisted tools
    pub default_deny: bool,
}

impl Default for ToolAllowlist {
    fn default() -> Self {
        Self {
            tools: HashMap::new(),
            default_deny: true, // ALL tools denied by default
        }
    }
}

/// WEB-07: Return the `skill_search` tool definition for use in the tool gate.
///
/// This is a read-only tool that queries the ClawHub registry for skills
/// matching a search query. It performs outbound HTTP only to the allowlisted
/// ClawHub domain and never installs or executes skill bytecode.
pub fn skill_search_tool_definition() -> ToolDefinition {
    ToolDefinition::read_only(
        "skill_search".to_string(),
        "Search the ClawHub skill registry for skills matching a query. \
         Returns a list of matching skills with safety classification. \
         Read-only — never installs or executes any skill."
            .to_string(),
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search terms to look up in the ClawHub registry (e.g. 'file watcher', 'HTTP client')"
                }
            },
            "required": ["query"],
            "additionalProperties": false
        }),
    )
}

impl ToolAllowlist {
    /// Create new allowlist
    pub fn new() -> Self {
        Self::default()
    }

    /// Add tool to allowlist
    pub fn allow_tool(&mut self, name: String, permission: ToolPermission) {
        self.tools.insert(name, permission);
    }

    /// Check if tool is allowed with given permission
    pub fn is_allowed(&self, tool_name: &str, required_permission: &ToolPermission) -> bool {
        if let Some(allowed_permission) = self.tools.get(tool_name) {
            self.permission_sufficient(allowed_permission, required_permission)
        } else {
            !self.default_deny
        }
    }

    /// Check if allowed permission is sufficient for required permission
    fn permission_sufficient(&self, allowed: &ToolPermission, required: &ToolPermission) -> bool {
        match (allowed, required) {
            (ToolPermission::Destructive, _) => true,
            (ToolPermission::ReadWrite, ToolPermission::ReadWrite) => true,
            (ToolPermission::ReadWrite, ToolPermission::ReadOnly) => true,
            (ToolPermission::ReadOnly, ToolPermission::ReadOnly) => true,
            _ => false,
        }
    }

    /// Remove tool from allowlist
    pub fn deny_tool(&mut self, tool_name: &str) {
        self.tools.remove(tool_name);
    }

    /// Get all allowed tools
    pub fn get_allowed_tools(&self) -> Vec<(&String, &ToolPermission)> {
        self.tools.iter().collect()
    }

    /// Load from TOML configuration
    pub fn from_toml(content: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(content)
    }

    /// Save to TOML configuration
    pub fn to_toml(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }
}

// ── WEB-09: Moltbook tool definitions ────────────────────────────────────────

/// Return the `moltbook_browse` tool definition.
///
/// Browse operation: read-only, gated behind `LUMINA_MOLTBOOK_ENABLED`.
pub fn moltbook_browse_tool() -> ToolDefinition {
    ToolDefinition::read_only(
        "moltbook_browse".to_string(),
        "Browse recent posts from the Moltbook community platform. \
         All content is sanitised through input_guard before being returned. \
         Requires LUMINA_MOLTBOOK_ENABLED=true."
            .to_string(),
        serde_json::json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false
        }),
    )
}

/// Return the `moltbook_post` tool definition.
///
/// Post operation: read-write, requires an admin approval token.
pub fn moltbook_post_tool() -> ToolDefinition {
    ToolDefinition::read_write(
        "moltbook_post".to_string(),
        "Post content to Moltbook on behalf of the configured account. \
         An admin approval token is mandatory — auto-posting is never permitted. \
         Content is sanitised through input_guard before being sent. \
         Requires LUMINA_MOLTBOOK_ENABLED=true."
            .to_string(),
        serde_json::json!({
            "type": "object",
            "properties": {
                "content": {
                    "type": "string",
                    "description": "The text content to post to Moltbook."
                },
                "approval_token": {
                    "type": "string",
                    "description": "Admin approval token authorising this post. Must not be empty."
                }
            },
            "required": ["content", "approval_token"],
            "additionalProperties": false
        }),
    )
}

/// Return the `moltbook_comment` tool definition.
///
/// Comment operation: read-write, requires an admin approval token.
pub fn moltbook_comment_tool() -> ToolDefinition {
    ToolDefinition::read_write(
        "moltbook_comment".to_string(),
        "Post a comment on an existing Moltbook post. \
         An admin approval token is mandatory — auto-commenting is never permitted. \
         Content is sanitised through input_guard before being sent. \
         Requires LUMINA_MOLTBOOK_ENABLED=true."
            .to_string(),
        serde_json::json!({
            "type": "object",
            "properties": {
                "post_id": {
                    "type": "string",
                    "description": "The ID of the Moltbook post to comment on."
                },
                "content": {
                    "type": "string",
                    "description": "The text content of the comment."
                },
                "approval_token": {
                    "type": "string",
                    "description": "Admin approval token authorising this comment. Must not be empty."
                }
            },
            "required": ["post_id", "content", "approval_token"],
            "additionalProperties": false
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_definition_creation() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"}
            }
        });

        let tool = ToolDefinition::read_only(
            "read_file".to_string(),
            "Read a file from disk".to_string(),
            schema,
        );

        assert_eq!(tool.name, "read_file");
        assert_eq!(tool.permission, ToolPermission::ReadOnly);
    }

    #[test]
    fn test_tool_call_creation() {
        let call = ToolCall::new(
            "call_123".to_string(),
            "read_file".to_string(),
            r#"{"path": "/tmp/test.txt"}"#.to_string(),
        );

        assert_eq!(call.id, "call_123");
        assert_eq!(call.function.name, "read_file");
        assert_eq!(call.call_type, "function");
    }

    #[test]
    fn test_tool_result_success() {
        let result = ToolResult::success(
            "call_123".to_string(),
            "read_file".to_string(),
            "file contents".to_string(),
        );

        assert!(result.success);
        assert!(result.error.is_none());
        assert_eq!(result.content, "file contents");
    }

    #[test]
    fn test_tool_result_error() {
        let result = ToolResult::error(
            "call_123".to_string(),
            "read_file".to_string(),
            "File not found".to_string(),
        );

        assert!(!result.success);
        assert!(result.error.is_some());
        assert!(result.content.contains("TOOL FAILED"));
        assert!(result.content.contains("read_file"));
        assert!(result.content.contains("File not found"));
    }

    #[test]
    fn test_tool_allowlist() {
        let mut allowlist = ToolAllowlist::new();

        // Initially empty - should deny all
        assert!(!allowlist.is_allowed("read_file", &ToolPermission::ReadOnly));

        // Add read-only tool
        allowlist.allow_tool("read_file".to_string(), ToolPermission::ReadOnly);
        assert!(allowlist.is_allowed("read_file", &ToolPermission::ReadOnly));
        assert!(!allowlist.is_allowed("read_file", &ToolPermission::ReadWrite));

        // Add read-write tool
        allowlist.allow_tool("write_file".to_string(), ToolPermission::ReadWrite);
        assert!(allowlist.is_allowed("write_file", &ToolPermission::ReadOnly));
        assert!(allowlist.is_allowed("write_file", &ToolPermission::ReadWrite));
        assert!(!allowlist.is_allowed("write_file", &ToolPermission::Destructive));

        // Add destructive tool
        allowlist.allow_tool("delete_file".to_string(), ToolPermission::Destructive);
        assert!(allowlist.is_allowed("delete_file", &ToolPermission::ReadOnly));
        assert!(allowlist.is_allowed("delete_file", &ToolPermission::ReadWrite));
        assert!(allowlist.is_allowed("delete_file", &ToolPermission::Destructive));
    }

    #[test]
    fn test_permission_hierarchy() {
        let allowlist = ToolAllowlist::new();

        // ReadOnly can only do ReadOnly
        assert!(allowlist.permission_sufficient(&ToolPermission::ReadOnly, &ToolPermission::ReadOnly));
        assert!(!allowlist.permission_sufficient(&ToolPermission::ReadOnly, &ToolPermission::ReadWrite));
        assert!(!allowlist.permission_sufficient(&ToolPermission::ReadOnly, &ToolPermission::Destructive));

        // ReadWrite can do ReadOnly and ReadWrite
        assert!(allowlist.permission_sufficient(&ToolPermission::ReadWrite, &ToolPermission::ReadOnly));
        assert!(allowlist.permission_sufficient(&ToolPermission::ReadWrite, &ToolPermission::ReadWrite));
        assert!(!allowlist.permission_sufficient(&ToolPermission::ReadWrite, &ToolPermission::Destructive));

        // Destructive can do everything
        assert!(allowlist.permission_sufficient(&ToolPermission::Destructive, &ToolPermission::ReadOnly));
        assert!(allowlist.permission_sufficient(&ToolPermission::Destructive, &ToolPermission::ReadWrite));
        assert!(allowlist.permission_sufficient(&ToolPermission::Destructive, &ToolPermission::Destructive));
    }

    #[test]
    fn test_allowlist_deny_tool() {
        let mut allowlist = ToolAllowlist::new();
        allowlist.allow_tool("test_tool".to_string(), ToolPermission::ReadOnly);

        assert!(allowlist.is_allowed("test_tool", &ToolPermission::ReadOnly));

        allowlist.deny_tool("test_tool");
        assert!(!allowlist.is_allowed("test_tool", &ToolPermission::ReadOnly));
    }

    #[test]
    fn test_get_allowed_tools() {
        let mut allowlist = ToolAllowlist::new();
        allowlist.allow_tool("tool1".to_string(), ToolPermission::ReadOnly);
        allowlist.allow_tool("tool2".to_string(), ToolPermission::ReadWrite);

        let tools = allowlist.get_allowed_tools();
        assert_eq!(tools.len(), 2);
    }

    #[test]
    fn test_toml_serialization() {
        let mut allowlist = ToolAllowlist::new();
        allowlist.allow_tool("read_file".to_string(), ToolPermission::ReadOnly);
        allowlist.allow_tool("write_file".to_string(), ToolPermission::ReadWrite);

        let toml_str = allowlist.to_toml().unwrap();
        assert!(toml_str.contains("read_file"));
        assert!(toml_str.contains("write_file"));

        let parsed = ToolAllowlist::from_toml(&toml_str).unwrap();
        assert_eq!(parsed.tools.len(), 2);
        assert!(parsed.is_allowed("read_file", &ToolPermission::ReadOnly));
        assert!(!parsed.is_allowed("unknown_tool", &ToolPermission::ReadOnly));
    }

    /// WEB-02: web_search_tool_definition returns a correctly formed ToolDefinition.
    #[test]
    fn test_web_search_tool_definition() {
        let def = web_search_tool_definition();
        assert_eq!(def.name, "web_search");
        assert_eq!(def.permission, ToolPermission::ReadOnly);
        // Schema must require 'query'
        let required = def.argument_schema
            .get("required")
            .and_then(|r| r.as_array())
            .expect("schema must have 'required' array");
        assert!(
            required.iter().any(|v| v.as_str() == Some("query")),
            "'query' must be in required: {:?}", required
        );
        // 'count' property must exist with integer type
        let count_type = def.argument_schema
            .pointer("/properties/count/type")
            .and_then(|v| v.as_str());
        assert_eq!(count_type, Some("integer"), "'count' must be integer type");
    }

    // ── Finance tool definitions ──────────────────────────────────────────────

    #[test]
    fn test_stock_quote_tool_definition() {
        let tool = stock_quote_tool();
        assert_eq!(tool.name, "stock_quote");
        assert_eq!(tool.permission, ToolPermission::ReadOnly);
        assert!(
            tool.description.contains("Alphavantage"),
            "description should mention Alphavantage"
        );
        assert!(
            tool.description.contains("Finnhub"),
            "description should mention Finnhub fallback"
        );
        // Schema requires 'symbol'
        let required = tool.argument_schema["required"].as_array().unwrap();
        assert!(
            required.iter().any(|v| v.as_str() == Some("symbol")),
            "schema must require 'symbol'"
        );
        assert_eq!(
            tool.argument_schema["additionalProperties"].as_bool(),
            Some(false),
            "schema must reject extra properties"
        );
    }

    #[test]
    fn test_market_summary_tool_definition() {
        let tool = market_summary_tool();
        assert_eq!(tool.name, "market_summary");
        assert_eq!(tool.permission, ToolPermission::ReadOnly);
        assert!(
            tool.description.contains("SPY") && tool.description.contains("QQQ"),
            "description should mention index symbols"
        );
        // No required arguments
        let required = tool.argument_schema["required"].as_array().unwrap();
        assert!(required.is_empty(), "market_summary takes no required args");
        assert_eq!(
            tool.argument_schema["additionalProperties"].as_bool(),
            Some(false),
        );
    }

    #[test]
    fn test_finance_tools_convert_to_chord_tools() {
        let sq = stock_quote_tool();
        let chord = sq.to_chord_tool();
        assert_eq!(chord.tool_type, "function");
        assert_eq!(chord.function.name, "stock_quote");

        let ms = market_summary_tool();
        let chord2 = ms.to_chord_tool();
        assert_eq!(chord2.function.name, "market_summary");
    }
}