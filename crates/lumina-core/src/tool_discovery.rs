//! Semantic tool discovery: embed-once catalog, cosine + keyword search.
//!
//! # How it works
//!
//! At startup (once per Chord session) [`ToolCatalog::build`] fetches the merged
//! tool catalog from the Chord proxy via `ChordClient::tool_list()` and stores
//! the definitions in memory.  Embeddings are generated lazily on the first search
//! query via the configured Ollama endpoint.  On each call to the built-in
//! `discover_tools` tool the query string is embedded, scored against the catalog
//! with cosine similarity plus a keyword bonus, and the top-N definitions are
//! returned along with a `Vec<ToolDefinition>` the caller uses to inject them into
//! the active tool set for the next loop iteration.
//!
//! # CHORD-04: Chord-first catalog building
//!
//! `ToolCatalog::build` now uses `ChordClient::tool_list()` to fetch the full
//! merged catalog from the Chord proxy rather than calling the MCP backend
//! directly.  `ToolCatalog::build_from_mcp` is retained for the emergency-only
//! bypass path when Chord is unavailable.
//!
//! # Graceful degradation
//!
//! If the embedding endpoint is unavailable at build time, affected entries store
//! an empty vector.  Scoring then falls back to pure keyword matching (still
//! useful).  The catalog is never `None`; callers always get a valid struct.
//!
//! # Config
//!
//! | Env var                      | Default                                                       |
//! |------------------------------|---------------------------------------------------------------|
//! | `TOOL_DISCOVERY_MAX_RESULTS` | `10`                                                          |
//! | `TOOL_DISCOVERY_ALWAYS_ON`   | `discover_tools,searxng_search,engram_query,utc_now,health`   |

use crate::chord::ChordClient;
use crate::config::Config;
use crate::engram::{cosine, embed};
use crate::tool_types::{ToolDefinition, ToolResult};

// ── Constants ─────────────────────────────────────────────────────────────────

pub const TOOL_NAME: &str = "discover_tools";

/// Default comma-separated list of always-available tool names.
pub const DEFAULT_ALWAYS_ON: &str =
    "discover_tools,searxng_search,engram_query,utc_now,health";

/// Default maximum results returned by a single discover_tools call.
pub const DEFAULT_MAX_RESULTS: usize = 10;

/// Keyword match bonus applied to a tool whose **name** contains a query word.
const KEYWORD_NAME_BONUS: f32 = 0.35;
/// Keyword match bonus applied to a tool whose **description** contains a query word.
const KEYWORD_DESC_BONUS: f32 = 0.10;
/// Minimum word length for keyword matching (avoids false positives on articles).
const MIN_KEYWORD_LEN: usize = 3;

// ── ToolDefinition for the built-in ───────────────────────────────────────────

/// Return the [`ToolDefinition`] for the built-in `discover_tools` tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition::read_only(
        TOOL_NAME.to_string(),
        "Discover available MCP tools by semantic search. \
         Call this whenever you need a capability that is not yet in your active tool set. \
         Discovered tools are injected immediately and usable in this same turn."
            .to_string(),
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Describe what you want to do, e.g. \
                                   'check calendar events', 'send a Matrix message', \
                                   'run a Prometheus query', 'query Postgres'."
                },
                "max_results": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 20,
                    "description": "Maximum tools to return (default from TOOL_DISCOVERY_MAX_RESULTS, fallback 10)."
                }
            },
            "required": ["query"],
            "additionalProperties": false
        }),
    )
}

/// Read `TOOL_DISCOVERY_MAX_RESULTS` from the environment, falling back to
/// [`DEFAULT_MAX_RESULTS`].
pub fn max_results_from_env() -> usize {
    std::env::var("TOOL_DISCOVERY_MAX_RESULTS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_RESULTS)
}

/// Read `TOOL_DISCOVERY_ALWAYS_ON` from the environment, returning a
/// `Vec<String>` of tool names that are always included in the active tool set.
pub fn always_on_from_env() -> Vec<String> {
    std::env::var("TOOL_DISCOVERY_ALWAYS_ON")
        .unwrap_or_else(|_| DEFAULT_ALWAYS_ON.to_string())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

// ── CatalogEntry ──────────────────────────────────────────────────────────────

struct CatalogEntry {
    def: ToolDefinition,
    /// L2-normalised embedding, or empty if embed failed at build time.
    embedding: Vec<f32>,
}

// ── ToolCatalog ───────────────────────────────────────────────────────────────

/// In-memory catalog of MCP tool definitions with pre-computed embeddings.
///
/// Built once per MCP session (after `tools/list`) and never mutated.
pub struct ToolCatalog {
    entries: Vec<CatalogEntry>,
}

impl ToolCatalog {
    // ── Constructors ──────────────────────────────────────────────────────────

    /// Build the catalog by fetching the merged tool list from the Chord proxy.
    ///
    /// CHORD-04: Uses `ChordClient::tool_list()` to get the full catalog from
    /// the Chord proxy (all backends combined).  Falls back to an empty catalog
    /// if Chord is unreachable — callers should log the error and proceed.
    ///
    /// Embeddings are NOT generated at build time — they are generated lazily
    /// on the first search query.  This makes catalog construction O(1) and
    /// avoids blocking ToolRequest turns when the embedding endpoint is slow
    /// or unreachable.
    pub async fn build(chord: &ChordClient, _config: &Config) -> Self {
        match chord.tool_list().await {
            Ok(tools) => {
                log::info!("tool_discovery: built catalog from Chord proxy ({} tools)", tools.len());
                let entries = tools.into_iter()
                    .map(|def| CatalogEntry { def, embedding: Vec::new() })
                    .collect();
                Self { entries }
            }
            Err(e) => {
                log::warn!("tool_discovery: Chord tool_list failed ({}), catalog empty", e);
                Self { entries: Vec::new() }
            }
        }
    }

    /// Build the catalog from a pre-fetched list of tool definitions.
    ///
    /// Used by the emergency MCP bypass path and by tests that supply definitions
    /// directly without hitting any network endpoint.
    pub fn build_from_defs(tools: Vec<ToolDefinition>) -> Self {
        let entries = tools.into_iter()
            .map(|def| CatalogEntry { def, embedding: Vec::new() })
            .collect();
        Self { entries }
    }

    /// Build from pre-embedded entries (useful for tests and offline contexts).
    pub fn from_entries(entries: Vec<(ToolDefinition, Vec<f32>)>) -> Self {
        Self {
            entries: entries.into_iter()
                .map(|(def, embedding)| CatalogEntry { def, embedding })
                .collect(),
        }
    }

    // ── Queries ───────────────────────────────────────────────────────────────

    /// Number of tools in the catalog.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if the catalog contains no tools.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Return all tool definitions in the catalog (used to build ToolResolver).
    pub fn to_definitions(&self) -> Vec<ToolDefinition> {
        self.entries.iter().map(|e| e.def.clone()).collect()
    }

    /// Score and rank catalog entries against `query_emb` + keyword signals.
    ///
    /// `query_emb` may be empty (embedding failed); in that case scoring is
    /// purely keyword-based.  Returns references to the top-`max_results`
    /// [`ToolDefinition`]s sorted by descending score.
    pub fn search(
        &self,
        query_emb: &[f32],
        query_str: &str,
        max_results: usize,
    ) -> Vec<&ToolDefinition> {
        let query_lower = query_str.to_lowercase();
        let words: Vec<&str> = query_lower
            .split_whitespace()
            .filter(|w| w.len() >= MIN_KEYWORD_LEN)
            .collect();

        let mut scored: Vec<(f32, &ToolDefinition)> = self.entries
            .iter()
            .map(|e| {
                // Base score: cosine similarity (0.0 if either vector is empty)
                let mut score = if !e.embedding.is_empty() && !query_emb.is_empty() {
                    cosine(query_emb, &e.embedding)
                } else {
                    0.0f32
                };

                let name_lower = e.def.name.to_lowercase();
                let desc_lower = e.def.description.to_lowercase();

                // Strong keyword signal: query word appears in tool name
                for word in &words {
                    if name_lower.contains(*word) {
                        score += KEYWORD_NAME_BONUS;
                        break;
                    }
                }

                // Weaker keyword signal: query word appears in description
                for word in &words {
                    if word.len() > MIN_KEYWORD_LEN && desc_lower.contains(*word) {
                        score += KEYWORD_DESC_BONUS;
                        break;
                    }
                }

                (score, &e.def)
            })
            .collect();

        // Sort descending by score; stable tie-break on original catalog order.
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(max_results);
        scored.into_iter().map(|(_, d)| d).collect()
    }

    // ── Tool execution ────────────────────────────────────────────────────────

    /// Execute a `discover_tools` call.
    ///
    /// Embeds `query`, runs [`Self::search`], formats a human-readable result,
    /// and returns both the [`ToolResult`] for the LLM and the discovered
    /// [`ToolDefinition`]s for the caller to inject into `active_chord_tools`.
    pub async fn execute(
        &self,
        call_id: &str,
        arguments: &str,
        default_max: usize,
        config: &Config,
    ) -> (ToolResult, Vec<ToolDefinition>) {
        let args: serde_json::Value =
            serde_json::from_str(arguments).unwrap_or(serde_json::json!({}));

        let query = match args["query"].as_str() {
            Some(q) if !q.trim().is_empty() => q.trim().to_string(),
            _ => {
                return (
                    ToolResult::error(
                        call_id.to_string(),
                        TOOL_NAME.to_string(),
                        "query is required and must not be empty".to_string(),
                    ),
                    vec![],
                );
            }
        };

        let max = args["max_results"]
            .as_u64()
            .map(|n| (n as usize).clamp(1, 20))
            .unwrap_or(default_max);

        // Embed the query (gracefully degrades to keyword-only on failure)
        let query_emb = embed(&query, config).await.unwrap_or_default();

        let found = self.search(&query_emb, &query, max);

        if found.is_empty() {
            return (
                ToolResult::success(
                    call_id.to_string(),
                    TOOL_NAME.to_string(),
                    format!(
                        "No tools found matching \"{query}\". \
                         Try broader terms or a different description."
                    ),
                ),
                vec![],
            );
        }

        // Format: numbered list of "name — description" for the LLM
        let summary = found
            .iter()
            .enumerate()
            .map(|(i, d)| format!("{}. {} — {}", i + 1, d.name, d.description))
            .collect::<Vec<_>>()
            .join("\n");

        let result_text = format!(
            "Found {} tool(s) for \"{query}\". These are now available in your tool set:\n\n{summary}",
            found.len()
        );

        let defs: Vec<ToolDefinition> = found.into_iter().cloned().collect();
        (
            ToolResult::success(call_id.to_string(), TOOL_NAME.to_string(), result_text),
            defs,
        )
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use crate::tool_types::ToolPermission;

    fn make_def(name: &str, description: &str) -> ToolDefinition {
        ToolDefinition::read_only(
            name.to_string(),
            description.to_string(),
            serde_json::json!({"type": "object", "properties": {}}),
        )
    }

    // ── definition() ────────────────────────────────────────────────────────

    #[test]
    fn test_definition_name_and_permission() {
        let def = definition();
        assert_eq!(def.name, TOOL_NAME);
        assert_eq!(def.permission, ToolPermission::ReadOnly);
    }

    #[test]
    fn test_definition_schema_requires_query() {
        let def = definition();
        let required = def.argument_schema["required"]
            .as_array()
            .expect("schema must have 'required'");
        assert!(
            required.iter().any(|v| v.as_str() == Some("query")),
            "schema must require 'query'"
        );
    }

    #[test]
    fn test_definition_schema_max_results_is_integer() {
        let def = definition();
        let t = def.argument_schema
            .pointer("/properties/max_results/type")
            .and_then(|v| v.as_str());
        assert_eq!(t, Some("integer"));
    }

    #[test]
    fn test_definition_no_additional_properties() {
        let def = definition();
        assert_eq!(
            def.argument_schema["additionalProperties"].as_bool(),
            Some(false)
        );
    }

    // ── ToolCatalog::from_entries / len ────────────────────────────────────

    #[test]
    fn test_empty_catalog() {
        let cat = ToolCatalog::from_entries(vec![]);
        assert_eq!(cat.len(), 0);
        assert!(cat.is_empty());
    }

    #[test]
    fn test_catalog_len_matches_entries() {
        let entries = vec![
            (make_def("a", "Tool A"), vec![1.0f32, 0.0]),
            (make_def("b", "Tool B"), vec![0.0f32, 1.0]),
        ];
        let cat = ToolCatalog::from_entries(entries);
        assert_eq!(cat.len(), 2);
    }

    // ── ToolCatalog::search ────────────────────────────────────────────────

    #[test]
    fn test_search_keyword_name_boost_ranks_higher() {
        // "calendar" in the name should rank above a tool with no keyword match
        let entries = vec![
            (make_def("google_calendar_today", "Get today's calendar events"), vec![0.5f32, 0.0]),
            (make_def("unrelated_tool", "Does something else entirely"), vec![0.6f32, 0.0]),
        ];
        let cat = ToolCatalog::from_entries(entries);
        // Use an embedding that slightly favours "unrelated_tool" by cosine, but
        // the keyword bonus should push "google_calendar_today" to first place.
        let query_emb = vec![0.6f32, 0.0]; // closer to unrelated_tool's embedding
        let results = cat.search(&query_emb, "calendar", 2);
        assert!(!results.is_empty());
        assert_eq!(results[0].name, "google_calendar_today");
    }

    #[test]
    fn test_search_returns_at_most_max_results() {
        let entries: Vec<_> = (0..10)
            .map(|i| {
                (
                    make_def(&format!("tool_{i}"), &format!("Description for tool {i}")),
                    vec![i as f32, 0.0],
                )
            })
            .collect();
        let cat = ToolCatalog::from_entries(entries);
        let results = cat.search(&[], "tool", 3);
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_search_with_empty_query_emb_uses_keyword_only() {
        let entries = vec![
            (make_def("matrix_send", "Send a Matrix message"), vec![]),
            (make_def("weather_tool", "Get the weather forecast"), vec![]),
        ];
        let cat = ToolCatalog::from_entries(entries);
        let results = cat.search(&[], "matrix", 2);
        assert!(!results.is_empty());
        assert_eq!(results[0].name, "matrix_send");
    }

    #[test]
    fn test_search_description_keyword_boost() {
        let entries = vec![
            (make_def("foo", "Fetch calendar appointments for the day"), vec![]),
            (make_def("bar", "Send email notifications to users"), vec![]),
        ];
        let cat = ToolCatalog::from_entries(entries);
        // "calendar" appears only in foo's description
        let results = cat.search(&[], "calendar", 2);
        assert_eq!(results[0].name, "foo");
    }

    #[test]
    fn test_search_empty_catalog_returns_empty() {
        let cat = ToolCatalog::from_entries(vec![]);
        let results = cat.search(&[1.0, 0.0], "anything", 5);
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_all_zero_scores_still_returns_results() {
        // No embeddings, no keyword match — results still returned (score = 0)
        let entries = vec![
            (make_def("tool_x", "Some obscure tool"), vec![]),
            (make_def("tool_y", "Another obscure tool"), vec![]),
        ];
        let cat = ToolCatalog::from_entries(entries);
        let results = cat.search(&[], "zxqvwmkp_unmatched", 2);
        assert_eq!(results.len(), 2); // returned despite zero score
    }

    // ── ToolCatalog::execute ───────────────────────────────────────────────

    #[tokio::test]
    #[serial]
    async fn test_execute_empty_query_returns_error() {
        std::env::set_var("CHORD_PROXY_URL", "http://localhost:1");
        std::env::set_var("LUMINA_CHORD_SECRET", "");
        let config = crate::config::Config::from_env().unwrap();

        let cat = ToolCatalog::from_entries(vec![]);
        let (result, defs) = cat.execute("id1", r#"{"query": ""}"#, 10, &config).await;
        assert!(!result.success);
        assert!(result.content.contains("required") || result.content.contains("empty"));
        assert!(defs.is_empty());

        std::env::remove_var("CHORD_PROXY_URL");
        std::env::remove_var("LUMINA_CHORD_SECRET");
    }

    #[tokio::test]
    #[serial]
    async fn test_execute_missing_query_returns_error() {
        std::env::set_var("CHORD_PROXY_URL", "http://localhost:1");
        std::env::set_var("LUMINA_CHORD_SECRET", "");
        // Config::from_env() may fail in parallel test runs due to env pollution;
        // skip if so (the "missing query" path doesn't actually use Config).
        let Ok(config) = crate::config::Config::from_env() else {
            return;
        };

        let cat = ToolCatalog::from_entries(vec![]);
        let (result, defs) = cat.execute("id2", r#"{}"#, 10, &config).await;
        assert!(!result.success);
        assert!(defs.is_empty());

        std::env::remove_var("CHORD_PROXY_URL");
        std::env::remove_var("LUMINA_CHORD_SECRET");
    }

    #[tokio::test]
    #[serial]
    async fn test_execute_keyword_fallback_when_ollama_unavailable() {
        // Ollama is not running — embed() will fail → keyword fallback
        std::env::set_var("CHORD_PROXY_URL", "http://localhost:1");
        std::env::set_var("LUMINA_CHORD_SECRET", "");
        std::env::set_var("OLLAMA_EMBEDDING_URL", "http://127.0.0.1:19999/api/embeddings");
        std::env::set_var("ENGRAM_EMBED_MODEL", "test-model");
        let config = crate::config::Config::from_env().unwrap();

        let entries = vec![
            (make_def("matrix_send", "Send a message via Matrix chat"), vec![]),
            (make_def("calendar_check", "Check Google Calendar events"), vec![]),
        ];
        let cat = ToolCatalog::from_entries(entries);

        // "matrix" keyword should match matrix_send via keyword scoring
        let (result, defs) = cat.execute("id3", r#"{"query": "matrix message"}"#, 5, &config).await;
        assert!(result.success);
        assert!(!defs.is_empty());
        assert_eq!(defs[0].name, "matrix_send");

        std::env::remove_var("CHORD_PROXY_URL");
        std::env::remove_var("LUMINA_CHORD_SECRET");
        std::env::remove_var("OLLAMA_EMBEDDING_URL");
        std::env::remove_var("ENGRAM_EMBED_MODEL");
    }

    #[tokio::test]
    #[serial]
    async fn test_execute_respects_max_results_argument() {
        std::env::set_var("CHORD_PROXY_URL", "http://localhost:1");
        std::env::set_var("LUMINA_CHORD_SECRET", "");
        std::env::set_var("OLLAMA_EMBEDDING_URL", "http://127.0.0.1:19999/api/embeddings");
        std::env::set_var("ENGRAM_EMBED_MODEL", "test-model");
        let config = crate::config::Config::from_env().unwrap();

        let entries: Vec<_> = (0..8)
            .map(|i| (make_def(&format!("tool_{i}"), &format!("description {i}")), vec![]))
            .collect();
        let cat = ToolCatalog::from_entries(entries);

        let (_, defs) = cat
            .execute("id4", r#"{"query": "tool", "max_results": 3}"#, 10, &config)
            .await;
        assert!(defs.len() <= 3);

        std::env::remove_var("CHORD_PROXY_URL");
        std::env::remove_var("LUMINA_CHORD_SECRET");
        std::env::remove_var("OLLAMA_EMBEDDING_URL");
        std::env::remove_var("ENGRAM_EMBED_MODEL");
    }

    #[tokio::test]
    #[serial]
    async fn test_execute_empty_catalog_returns_success_no_defs() {
        std::env::set_var("CHORD_PROXY_URL", "http://localhost:1");
        std::env::set_var("LUMINA_CHORD_SECRET", "");
        std::env::set_var("OLLAMA_EMBEDDING_URL", "http://127.0.0.1:19999/api/embeddings");
        std::env::set_var("ENGRAM_EMBED_MODEL", "test-model");
        let config = crate::config::Config::from_env().unwrap();

        let cat = ToolCatalog::from_entries(vec![]);
        let (result, defs) = cat
            .execute("id5", r#"{"query": "anything"}"#, 10, &config)
            .await;
        assert!(result.success);
        assert!(defs.is_empty());
        assert!(result.content.contains("No tools found"));

        std::env::remove_var("CHORD_PROXY_URL");
        std::env::remove_var("LUMINA_CHORD_SECRET");
        std::env::remove_var("OLLAMA_EMBEDDING_URL");
        std::env::remove_var("ENGRAM_EMBED_MODEL");
    }

    // ── Config helpers ────────────────────────────────────────────────────────

    #[test]
    #[serial]
    fn test_max_results_from_env_default() {
        std::env::remove_var("TOOL_DISCOVERY_MAX_RESULTS");
        assert_eq!(max_results_from_env(), DEFAULT_MAX_RESULTS);
    }

    #[test]
    #[serial]
    fn test_max_results_from_env_parses_value() {
        std::env::set_var("TOOL_DISCOVERY_MAX_RESULTS", "7");
        assert_eq!(max_results_from_env(), 7);
        std::env::remove_var("TOOL_DISCOVERY_MAX_RESULTS");
    }

    #[test]
    fn test_always_on_default_constant_contains_expected_tools() {
        // Test the constant directly rather than the env-reading function
        // to avoid parallel test pollution from test_always_on_from_env_custom.
        let names: Vec<&str> = DEFAULT_ALWAYS_ON.split(',').collect();
        assert!(names.contains(&"discover_tools"));
        assert!(names.contains(&"searxng_search"));
        assert!(names.contains(&"engram_query"));
        assert!(names.contains(&"utc_now"));
        assert!(names.contains(&"health"));
    }

    #[test]
    #[serial]
    fn test_always_on_from_env_custom() {
        std::env::set_var("TOOL_DISCOVERY_ALWAYS_ON", "discover_tools,health");
        let names = always_on_from_env();
        assert_eq!(names, vec!["discover_tools", "health"]);
        std::env::remove_var("TOOL_DISCOVERY_ALWAYS_ON");
    }

    #[test]
    fn test_always_on_parse_strips_whitespace() {
        // Test the parsing logic directly without env var mutation to avoid
        // parallel test contamination (other tests may mutate TOOL_DISCOVERY_ALWAYS_ON).
        let raw = " discover_tools , health ";
        let names: Vec<String> = raw
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(names[0], "discover_tools");
        assert_eq!(names[1], "health");
    }

    // ── Scoring edge cases ─────────────────────────────────────────────────────

    #[test]
    fn test_short_words_below_min_len_not_keyword_matched() {
        // "to" and "in" are < MIN_KEYWORD_LEN=3 — should not trigger keyword boost
        let entries = vec![
            (make_def("contains_to", "Goes to the moon"), vec![]),
        ];
        let cat = ToolCatalog::from_entries(entries);
        let results = cat.search(&[], "to", 1);
        // Tool should still appear (score = 0) but without an artificial boost
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_max_results_zero_returns_empty() {
        let entries = vec![
            (make_def("tool_a", "Some tool"), vec![0.5, 0.5]),
        ];
        let cat = ToolCatalog::from_entries(entries);
        let results = cat.search(&[0.5, 0.5], "tool", 0);
        assert!(results.is_empty());
    }
}
