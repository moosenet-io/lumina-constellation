//! Tool priority routing: Chord (MCP proxy) vs internal implementations.
//!
//! [`ToolResolver`] holds the tool catalog (fetched at startup via
//! `ChordClient::tool_list()`) and an alias map.  At tool-dispatch time it
//! decides whether to call a tool through the Chord proxy or fall back to
//! the internal Rust implementation.
//!
//! # CHORD-04: Chord-routed tool calls
//!
//! The "MCP" routes resolved by this module (`ToolRoute::Mcp`) are now executed
//! via `ChordClient::tool_call()` rather than `McpTransport::call_tool()`.
//! The `ToolResolver` itself is unchanged — it produces routing decisions; the
//! caller (agent loop) is responsible for dispatching `ToolRoute::Mcp` through
//! the Chord client.
//!
//! # Priority strategies (`TOOL_PRIORITY` env var)
//!
//! | Value           | Behaviour |
//! |-----------------|-----------|
//! | `mcp_first`     | Try Chord (or alias) first; fall back to internal on failure. **Default.** |
//! | `internal_first`| Use internal implementations; call Chord only for tools with no internal version. |
//! | `mcp_only`      | Never execute internal tools. |
//! | `internal_only` | Never call Chord. |
//!
//! # Alias map
//!
//! Maps an internal tool name to the MCP tool name that supersedes it:
//!
//! ```text
//! web_search     → searxng_search       (MCP has SearXNG; internal has DuckDuckGo)
//! web_browse     → lumina_web_fetch     (MCP has lumina_web_fetch)
//! news_headlines → news_headlines       (same name; prefers MCP copy)
//! news_search    → news_search          (same name)
//! calendar       → google_calendar_today
//! email          → google_email_inbox
//! ```
//!
//! If the alias target is not present in the current MCP catalog, the alias is
//! silently skipped and routing falls through to the internal implementation.
//!
//! # Tool list ordering
//!
//! [`ToolResolver::ordered_definitions`] returns MCP tools first (or internal
//! first for `internal_first` / `internal_only`), with deduplication by name
//! (first occurrence wins).  LLMs tend to prefer tools listed earlier, so
//! putting MCP tools first biases selection toward the richer mcp-host catalog.

use crate::tool_types::ToolDefinition;
use std::collections::{HashMap, HashSet};

// ── Static alias table ─────────────────────────────────────────────────────

/// Internal tool name → preferred MCP tool name.
static ALIASES: &[(&str, &str)] = &[
    ("web_search", "searxng_search"),
    ("web_browse", "lumina_web_fetch"),
    ("news_headlines", "news_headlines"),
    ("news_search", "news_search"),
    ("news_topic", "news_topic"),
    ("calendar", "google_calendar_today"),
    ("email", "google_email_inbox"),
    ("stock_quote", "meridian_market_data"),
    ("market_summary", "meridian_market_data"),
];

// ── ToolPriority ───────────────────────────────────────────────────────────

/// Routing strategy for tool dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolPriority {
    /// Try MCP (or alias) first; internal is the fallback.
    McpFirst,
    /// Use internal implementations first; MCP for tools with no internal version.
    InternalFirst,
    /// MCP only — never execute internal tools.
    McpOnly,
    /// Internal only — never call MCP.
    InternalOnly,
}

impl ToolPriority {
    /// Read from `TOOL_PRIORITY` env var.  Defaults to `McpFirst`.
    pub fn from_env() -> Self {
        match std::env::var("TOOL_PRIORITY")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "internal_first" => ToolPriority::InternalFirst,
            "mcp_only"       => ToolPriority::McpOnly,
            "internal_only"  => ToolPriority::InternalOnly,
            _                => ToolPriority::McpFirst,
        }
    }
}

// ── ToolRoute ──────────────────────────────────────────────────────────────

/// The resolved routing decision for a single tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolRoute {
    /// Call the MCP tool with this name (may differ from the requested name via alias).
    Mcp(String),
    /// Execute the internal Rust implementation.
    Internal,
    /// Neither MCP nor internal can handle this tool.
    NotFound,
}

// ── ToolResolver ──────────────────────────────────────────────────────────

/// Routes tool calls between the MCP catalog and internal implementations.
pub struct ToolResolver {
    /// Set of tool names available on the MCP server.
    mcp_names: HashSet<String>,
    /// MCP tool definitions (in registration order).
    mcp_defs: Vec<ToolDefinition>,
    /// Alias map: internal_name → mcp_name.
    aliases: HashMap<String, String>,
    /// Routing strategy.
    pub priority: ToolPriority,
}

impl ToolResolver {
    /// Build from the MCP tool list returned by `tools/list`.
    pub fn new(mcp_defs: Vec<ToolDefinition>, priority: ToolPriority) -> Self {
        let mcp_names: HashSet<String> = mcp_defs.iter().map(|d| d.name.clone()).collect();
        let aliases: HashMap<String, String> = ALIASES
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Self { mcp_names, mcp_defs, aliases, priority }
    }

    /// Build an empty resolver (no MCP tools, internal-only effectively).
    pub fn empty() -> Self {
        Self::new(vec![], ToolPriority::InternalOnly)
    }

    /// Resolve where to route `tool_name`.
    ///
    /// `mcp_available` should be `false` when the MCP transport is down,
    /// forcing fallback to internal regardless of priority.
    pub fn resolve(&self, tool_name: &str, mcp_available: bool) -> ToolRoute {
        match self.priority {
            ToolPriority::InternalOnly => return ToolRoute::Internal,
            ToolPriority::McpOnly => {
                if !mcp_available { return ToolRoute::NotFound; }
                return self.try_mcp_route(tool_name).unwrap_or(ToolRoute::NotFound);
            }
            ToolPriority::McpFirst => {
                if mcp_available {
                    if let Some(route) = self.try_mcp_route(tool_name) {
                        return route;
                    }
                }
                return ToolRoute::Internal;
            }
            ToolPriority::InternalFirst => {
                // Always return Internal — internal implementations run first.
                // Callers that have no internal handler should check NotFound and then try MCP.
                return ToolRoute::Internal;
            }
        }
    }

    /// Resolve a tool name for MCP-only dispatch (internal_first fallthrough).
    ///
    /// Used when an internal implementation was not found and the caller wants
    /// to try MCP as a last resort.
    pub fn resolve_mcp_fallback(&self, tool_name: &str) -> ToolRoute {
        if matches!(self.priority, ToolPriority::InternalOnly) {
            return ToolRoute::NotFound;
        }
        self.try_mcp_route(tool_name).unwrap_or(ToolRoute::NotFound)
    }

    /// Return a combined, deduplicated tool list for the LLM's `tools[]` array.
    ///
    /// Ordering depends on priority:
    /// - `mcp_first` / `mcp_only`: MCP tools first, then internal
    /// - `internal_first` / `internal_only`: internal tools first, then MCP
    ///
    /// Names are deduplicated; the first occurrence wins.
    pub fn ordered_definitions(
        &self,
        internal_defs: &[ToolDefinition],
    ) -> Vec<ToolDefinition> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut out: Vec<ToolDefinition> = Vec::new();

        let add_list = |list: &[ToolDefinition], seen: &mut HashSet<String>, out: &mut Vec<ToolDefinition>| {
            for d in list {
                if seen.insert(d.name.clone()) {
                    out.push(d.clone());
                }
            }
        };

        match self.priority {
            ToolPriority::McpFirst | ToolPriority::McpOnly => {
                add_list(&self.mcp_defs, &mut seen, &mut out);
                if !matches!(self.priority, ToolPriority::McpOnly) {
                    add_list(internal_defs, &mut seen, &mut out);
                }
            }
            ToolPriority::InternalFirst | ToolPriority::InternalOnly => {
                add_list(internal_defs, &mut seen, &mut out);
                if !matches!(self.priority, ToolPriority::InternalOnly) {
                    add_list(&self.mcp_defs, &mut seen, &mut out);
                }
            }
        }
        out
    }

    /// Whether the named tool exists in the MCP catalog.
    pub fn has_mcp_tool(&self, name: &str) -> bool {
        self.mcp_names.contains(name)
    }

    /// Number of MCP tools in the catalog.
    pub fn mcp_tool_count(&self) -> usize {
        self.mcp_defs.len()
    }

    // ── Private helpers ────────────────────────────────────────────────────

    /// Try to produce an MCP route for `tool_name`:
    /// 1. Direct match in MCP catalog.
    /// 2. Alias lookup → target must also exist in MCP catalog.
    fn try_mcp_route(&self, tool_name: &str) -> Option<ToolRoute> {
        // Direct match
        if self.mcp_names.contains(tool_name) {
            return Some(ToolRoute::Mcp(tool_name.to_string()));
        }
        // Alias
        if let Some(mcp_name) = self.aliases.get(tool_name) {
            if self.mcp_names.contains(mcp_name.as_str()) {
                return Some(ToolRoute::Mcp(mcp_name.clone()));
            }
        }
        None
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use crate::tool_types::ToolPermission;

    fn make_def(name: &str) -> ToolDefinition {
        ToolDefinition::read_only(
            name.to_string(),
            format!("Tool {name}"),
            serde_json::json!({"type":"object","properties":{}}),
        )
    }

    fn resolver_with(mcp_names: &[&str], priority: ToolPriority) -> ToolResolver {
        let defs = mcp_names.iter().map(|n| make_def(n)).collect();
        ToolResolver::new(defs, priority)
    }

    // ── resolve: mcp_first ────────────────────────────────────────────────

    #[test]
    fn test_mcp_first_direct_match() {
        let r = resolver_with(&["searxng_search", "news_headlines"], ToolPriority::McpFirst);
        assert_eq!(
            r.resolve("searxng_search", true),
            ToolRoute::Mcp("searxng_search".to_string())
        );
    }

    #[test]
    fn test_mcp_first_alias_web_search_to_searxng() {
        let r = resolver_with(&["searxng_search"], ToolPriority::McpFirst);
        assert_eq!(
            r.resolve("web_search", true),
            ToolRoute::Mcp("searxng_search".to_string())
        );
    }

    #[test]
    fn test_mcp_first_alias_web_browse_to_lumina_web_fetch() {
        let r = resolver_with(&["lumina_web_fetch"], ToolPriority::McpFirst);
        assert_eq!(
            r.resolve("web_browse", true),
            ToolRoute::Mcp("lumina_web_fetch".to_string())
        );
    }

    #[test]
    fn test_mcp_first_falls_back_to_internal_when_no_mcp_match() {
        let r = resolver_with(&["some_other_tool"], ToolPriority::McpFirst);
        assert_eq!(r.resolve("web_search", true), ToolRoute::Internal);
    }

    #[test]
    fn test_mcp_first_falls_back_to_internal_when_mcp_unavailable() {
        let r = resolver_with(&["searxng_search"], ToolPriority::McpFirst);
        assert_eq!(r.resolve("searxng_search", false), ToolRoute::Internal);
    }

    #[test]
    fn test_mcp_first_alias_skipped_when_target_missing() {
        // web_search alias → searxng_search, but searxng_search not in catalog
        let r = resolver_with(&["other_tool"], ToolPriority::McpFirst);
        assert_eq!(r.resolve("web_search", true), ToolRoute::Internal);
    }

    // ── resolve: other priorities ─────────────────────────────────────────

    #[test]
    fn test_internal_only_always_internal() {
        let r = resolver_with(&["searxng_search", "google_calendar_today"], ToolPriority::InternalOnly);
        assert_eq!(r.resolve("searxng_search", true), ToolRoute::Internal);
        assert_eq!(r.resolve("web_search", true), ToolRoute::Internal);
    }

    #[test]
    fn test_mcp_only_direct_match() {
        let r = resolver_with(&["searxng_search"], ToolPriority::McpOnly);
        assert_eq!(
            r.resolve("searxng_search", true),
            ToolRoute::Mcp("searxng_search".to_string())
        );
    }

    #[test]
    fn test_mcp_only_not_found_when_mcp_unavailable() {
        let r = resolver_with(&["searxng_search"], ToolPriority::McpOnly);
        assert_eq!(r.resolve("searxng_search", false), ToolRoute::NotFound);
    }

    #[test]
    fn test_mcp_only_not_found_when_no_match() {
        let r = resolver_with(&["other_tool"], ToolPriority::McpOnly);
        assert_eq!(r.resolve("unknown_tool", true), ToolRoute::NotFound);
    }

    #[test]
    fn test_internal_first_returns_internal() {
        let r = resolver_with(&["searxng_search"], ToolPriority::InternalFirst);
        assert_eq!(r.resolve("searxng_search", true), ToolRoute::Internal);
        assert_eq!(r.resolve("web_search", true), ToolRoute::Internal);
    }

    // ── resolve_mcp_fallback ──────────────────────────────────────────────

    #[test]
    fn test_mcp_fallback_finds_direct() {
        let r = resolver_with(&["nexus_send"], ToolPriority::InternalFirst);
        assert_eq!(
            r.resolve_mcp_fallback("nexus_send"),
            ToolRoute::Mcp("nexus_send".to_string())
        );
    }

    #[test]
    fn test_mcp_fallback_not_found_when_internal_only() {
        let r = resolver_with(&["nexus_send"], ToolPriority::InternalOnly);
        assert_eq!(r.resolve_mcp_fallback("nexus_send"), ToolRoute::NotFound);
    }

    // ── ordered_definitions ───────────────────────────────────────────────

    #[test]
    fn test_mcp_first_ordering() {
        let r = resolver_with(&["mcp_a", "mcp_b"], ToolPriority::McpFirst);
        let internal = vec![make_def("int_x"), make_def("mcp_a")]; // mcp_a duplicate
        let list = r.ordered_definitions(&internal);
        assert_eq!(list[0].name, "mcp_a");
        assert_eq!(list[1].name, "mcp_b");
        assert_eq!(list[2].name, "int_x");
        assert_eq!(list.len(), 3); // mcp_a deduplicated
    }

    #[test]
    fn test_internal_first_ordering() {
        let r = resolver_with(&["mcp_a", "mcp_b"], ToolPriority::InternalFirst);
        let internal = vec![make_def("int_x"), make_def("mcp_a")];
        let list = r.ordered_definitions(&internal);
        assert_eq!(list[0].name, "int_x");
        assert_eq!(list[1].name, "mcp_a"); // internal wins dedup
        assert_eq!(list[2].name, "mcp_b");
        assert_eq!(list.len(), 3);
    }

    #[test]
    fn test_mcp_only_no_internal_tools() {
        let r = resolver_with(&["mcp_a"], ToolPriority::McpOnly);
        let internal = vec![make_def("int_x")];
        let list = r.ordered_definitions(&internal);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "mcp_a");
    }

    #[test]
    fn test_internal_only_no_mcp_tools() {
        let r = resolver_with(&["mcp_a"], ToolPriority::InternalOnly);
        let internal = vec![make_def("int_x")];
        let list = r.ordered_definitions(&internal);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "int_x");
    }

    // ── ToolPriority::from_env ─────────────────────────────────────────────

    #[test]
    #[serial]
    fn test_priority_from_env_defaults_to_mcp_first() {
        std::env::remove_var("TOOL_PRIORITY");
        assert_eq!(ToolPriority::from_env(), ToolPriority::McpFirst);
    }

    #[test]
    #[serial]
    fn test_priority_from_env_parses_internal_first() {
        std::env::set_var("TOOL_PRIORITY", "internal_first");
        assert_eq!(ToolPriority::from_env(), ToolPriority::InternalFirst);
        std::env::remove_var("TOOL_PRIORITY");
    }

    #[test]
    #[serial]
    fn test_priority_from_env_parses_mcp_only() {
        std::env::set_var("TOOL_PRIORITY", "mcp_only");
        assert_eq!(ToolPriority::from_env(), ToolPriority::McpOnly);
        std::env::remove_var("TOOL_PRIORITY");
    }

    #[test]
    #[serial]
    fn test_priority_from_env_parses_internal_only() {
        std::env::set_var("TOOL_PRIORITY", "internal_only");
        assert_eq!(ToolPriority::from_env(), ToolPriority::InternalOnly);
        std::env::remove_var("TOOL_PRIORITY");
    }

    // ── misc ──────────────────────────────────────────────────────────────

    #[test]
    fn test_has_mcp_tool() {
        let r = resolver_with(&["searxng_search"], ToolPriority::McpFirst);
        assert!(r.has_mcp_tool("searxng_search"));
        assert!(!r.has_mcp_tool("unknown_tool"));
    }

    #[test]
    fn test_mcp_tool_count() {
        let r = resolver_with(&["a", "b", "c"], ToolPriority::McpFirst);
        assert_eq!(r.mcp_tool_count(), 3);
    }

    #[test]
    fn test_empty_resolver() {
        let r = ToolResolver::empty();
        assert_eq!(r.resolve("web_search", true), ToolRoute::Internal);
        assert_eq!(r.mcp_tool_count(), 0);
    }
}
