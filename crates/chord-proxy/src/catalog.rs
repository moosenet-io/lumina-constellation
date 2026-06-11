//! Merged tool catalog: MCP tools from mcp-host + Rust fallback tools.
//!
//! The catalog is cached for CHORD_CATALOG_CACHE_SECS (default 5 minutes).
//! Rust tools always take a "fallback" position — MCP tools with the same name
//! win. The `source` field indicates which backend a tool comes from.

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// A tool definition in the merged catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolEntry {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    /// "mcp" or "chord" (Rust fallback)
    pub source: String,
}

impl ToolEntry {
    pub fn from_mcp(name: String, description: String, parameters: serde_json::Value) -> Self {
        Self { name, description, parameters, source: "mcp".into() }
    }

    pub fn from_rust(name: String, description: String, parameters: serde_json::Value) -> Self {
        Self { name, description, parameters, source: "chord".into() }
    }
}

/// Merged catalog with time-based caching.
pub struct ToolCatalog {
    tools: Vec<ToolEntry>,
    cached_at: Instant,
    ttl: Duration,
}

impl ToolCatalog {
    pub fn new(ttl_secs: u64) -> Self {
        Self {
            tools: Vec::new(),
            cached_at: Instant::now() - Duration::from_secs(ttl_secs + 1),
            ttl: Duration::from_secs(ttl_secs),
        }
    }

    pub fn is_stale(&self) -> bool {
        self.cached_at.elapsed() > self.ttl
    }

    /// Merge MCP tools and Rust fallback tools into this catalog.
    /// MCP tools take priority: if both have a tool with the same name, MCP wins.
    pub fn update(&mut self, mcp_tools: Vec<ToolEntry>, rust_tools: Vec<ToolEntry>) {
        let mut merged: Vec<ToolEntry> = Vec::with_capacity(mcp_tools.len() + rust_tools.len());

        // MCP tools first
        let mcp_names: std::collections::HashSet<String> =
            mcp_tools.iter().map(|t| t.name.clone()).collect();
        merged.extend(mcp_tools);

        // Rust fallback tools that don't conflict with MCP names
        for t in rust_tools {
            if !mcp_names.contains(&t.name) {
                merged.push(t);
            }
        }

        self.tools = merged;
        self.cached_at = Instant::now();
    }

    pub fn all(&self) -> &[ToolEntry] {
        &self.tools
    }

    pub fn find(&self, name: &str) -> Option<&ToolEntry> {
        self.tools.iter().find(|t| t.name == name)
    }

    /// Simple keyword-based discovery.
    /// Scores each tool by how many query words appear in its name or description.
    /// Returns up to `max` results sorted by relevance descending.
    pub fn discover(&self, query: &str, max: usize) -> Vec<ToolEntry> {
        let query_lower = query.to_lowercase();
        let words: Vec<&str> = query_lower.split_whitespace().collect();

        if words.is_empty() {
            return self.tools.iter().take(max).cloned().collect();
        }

        let mut scored: Vec<(usize, &ToolEntry)> = self
            .tools
            .iter()
            .filter_map(|t| {
                let haystack =
                    format!("{} {}", t.name.to_lowercase(), t.description.to_lowercase());
                let score = words.iter().filter(|w| haystack.contains(*w)).count();
                if score > 0 { Some((score, t)) } else { None }
            })
            .collect();

        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored.into_iter().take(max).map(|(_, t)| t.clone()).collect()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

/// Parse MCP tools/list result into ToolEntry vec.
pub fn parse_mcp_tools(result: &serde_json::Value) -> Vec<ToolEntry> {
    let tools = match result.get("tools").and_then(|t| t.as_array()) {
        Some(arr) => arr,
        None => return Vec::new(),
    };

    tools
        .iter()
        .filter_map(|t| {
            let name = t.get("name")?.as_str()?.to_string();
            let description = t
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string();
            let parameters = t
                .get("inputSchema")
                .or_else(|| t.get("parameters"))
                .cloned()
                .unwrap_or(serde_json::json!({"type": "object", "properties": {}}));
            Some(ToolEntry::from_mcp(name, description, parameters))
        })
        .collect()
}

/// Extract text content from an MCP tools/call result.
pub fn extract_tool_result(result: &serde_json::Value) -> String {
    // MCP response format: {"content": [{"type": "text", "text": "..."}]}
    if let Some(content) = result.get("content").and_then(|c| c.as_array()) {
        let parts: Vec<&str> = content
            .iter()
            .filter_map(|item| {
                if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                    item.get("text").and_then(|t| t.as_str())
                } else {
                    None
                }
            })
            .collect();
        if !parts.is_empty() {
            return parts.join("\n");
        }
    }
    // Fallback: serialize the entire result
    result.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_mcp_tool(name: &str, desc: &str) -> ToolEntry {
        ToolEntry::from_mcp(name.into(), desc.into(), serde_json::json!({}))
    }

    fn make_rust_tool(name: &str, desc: &str) -> ToolEntry {
        ToolEntry::from_rust(name.into(), desc.into(), serde_json::json!({}))
    }

    #[test]
    fn test_catalog_starts_stale() {
        let catalog = ToolCatalog::new(300);
        assert!(catalog.is_stale());
    }

    #[test]
    fn test_catalog_not_stale_after_update() {
        let mut catalog = ToolCatalog::new(300);
        catalog.update(vec![], vec![]);
        assert!(!catalog.is_stale());
    }

    #[test]
    fn test_mcp_tools_take_priority_over_rust() {
        let mut catalog = ToolCatalog::new(300);
        let mcp = vec![make_mcp_tool("web_search", "MCP web search")];
        let rust = vec![make_rust_tool("web_search", "Rust web search")];
        catalog.update(mcp, rust);

        assert_eq!(catalog.len(), 1);
        let t = catalog.find("web_search").unwrap();
        assert_eq!(t.source, "mcp");
        assert_eq!(t.description, "MCP web search");
    }

    #[test]
    fn test_rust_tools_fill_gaps_not_in_mcp() {
        let mut catalog = ToolCatalog::new(300);
        let mcp = vec![make_mcp_tool("mcp_tool", "only in MCP")];
        let rust = vec![
            make_rust_tool("mcp_tool", "should be shadowed"),
            make_rust_tool("rust_tool", "only in Rust"),
        ];
        catalog.update(mcp, rust);

        assert_eq!(catalog.len(), 2);
        assert_eq!(catalog.find("mcp_tool").unwrap().source, "mcp");
        assert_eq!(catalog.find("rust_tool").unwrap().source, "chord");
    }

    #[test]
    fn test_discover_by_keyword() {
        let mut catalog = ToolCatalog::new(300);
        catalog.update(
            vec![
                make_mcp_tool("calendar_today", "Get today's calendar events"),
                make_mcp_tool("email_inbox", "Read your email inbox"),
                make_mcp_tool("web_search", "Search the web"),
            ],
            vec![],
        );

        let results = catalog.discover("calendar events", 10);
        assert!(!results.is_empty());
        assert_eq!(results[0].name, "calendar_today");
    }

    #[test]
    fn test_discover_respects_max() {
        let mut catalog = ToolCatalog::new(300);
        let tools: Vec<ToolEntry> = (0..20)
            .map(|i| make_mcp_tool(&format!("tool_{i}"), "some tool"))
            .collect();
        catalog.update(tools, vec![]);

        let results = catalog.discover("tool", 5);
        assert_eq!(results.len(), 5);
    }

    #[test]
    fn test_discover_empty_query_returns_up_to_max() {
        let mut catalog = ToolCatalog::new(300);
        catalog.update(
            vec![make_mcp_tool("a", "alpha"), make_mcp_tool("b", "beta")],
            vec![],
        );
        let results = catalog.discover("", 10);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_parse_mcp_tools_valid() {
        let result = serde_json::json!({
            "tools": [
                {"name": "search", "description": "Search tool", "inputSchema": {"type": "object"}},
                {"name": "email", "description": "Email tool"}
            ]
        });
        let tools = parse_mcp_tools(&result);
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "search");
        assert_eq!(tools[0].source, "mcp");
        assert_eq!(tools[1].name, "email");
    }

    #[test]
    fn test_parse_mcp_tools_empty() {
        let result = serde_json::json!({"tools": []});
        let tools = parse_mcp_tools(&result);
        assert!(tools.is_empty());
    }

    #[test]
    fn test_parse_mcp_tools_missing_field() {
        let result = serde_json::json!({});
        let tools = parse_mcp_tools(&result);
        assert!(tools.is_empty());
    }

    #[test]
    fn test_extract_tool_result_text_content() {
        let result = serde_json::json!({
            "content": [
                {"type": "text", "text": "Result line 1"},
                {"type": "text", "text": "Result line 2"}
            ]
        });
        let text = extract_tool_result(&result);
        assert!(text.contains("Result line 1"));
        assert!(text.contains("Result line 2"));
    }

    #[test]
    fn test_extract_tool_result_non_text_content() {
        let result = serde_json::json!({
            "content": [{"type": "image", "data": "base64..."}]
        });
        // Falls back to JSON serialization
        let text = extract_tool_result(&result);
        assert!(!text.is_empty());
    }

    #[test]
    fn test_extract_tool_result_fallback_to_json() {
        let result = serde_json::json!({"some_field": "some_value"});
        let text = extract_tool_result(&result);
        assert!(text.contains("some_value"));
    }
}
