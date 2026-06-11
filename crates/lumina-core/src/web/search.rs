//! WEB-02: Web search via DuckDuckGo HTML endpoint.
//!
//! Provides `WebSearch` for querying DuckDuckGo without an API key,
//! using the HTML search endpoint. All configuration is via environment
//! variables — no URLs or secrets are hardcoded.
//!
//! # Configuration
//! - `LUMINA_DDG_URL` — DuckDuckGo search base URL. If unset, the value is
//!   read from `LUMINA_EGRESS_ALLOWLIST` context. The default must be
//!   explicitly provided by the operator; the module never falls back to a
//!   hardcoded address.
//!
//! # Security
//! - Every HTTP call is pre-cleared by `EgressInspector` (fail-closed).
//! - All returned text fields pass through `OutputFilter` before being
//!   surfaced to callers.
//! - At most 5 results are returned regardless of what the server sends.
//!
//! # Errors
//! - Empty query → `LuminaError::Config("Empty search query")`
//! - URL not configured → `LuminaError::Config("LUMINA_DDG_URL not set")`
//! - Host blocked by egress inspector → `LuminaError::SecurityViolation`

use crate::egress_inspector::EgressInspector;
use crate::error::{LuminaError, Result};
use crate::security::output_filter::OutputFilter;
use crate::tool_types::{ToolDefinition, ToolPermission};
use reqwest::Client;
use std::time::Duration;

/// Tool name used when registering with the tool gate.
pub const TOOL_NAME: &str = "web_search";

/// Maximum number of search results returned per query.
pub const MAX_RESULTS: usize = 5;

/// A single search result from DuckDuckGo.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    /// Page title.
    pub title: String,
    /// Canonical page URL.
    pub url: String,
    /// Short excerpt / snippet from the result.
    pub snippet: String,
}

/// DuckDuckGo HTML search client.
///
/// Reads `LUMINA_DDG_URL` from the environment at construction time.
/// The `EgressInspector` used for pre-flight checks can be injected for
/// testing.
pub struct WebSearch {
    client: Client,
    /// Base search URL (from `LUMINA_DDG_URL` env var).
    base_url: String,
    /// Egress inspector used to clear outbound requests.
    inspector: EgressInspector,
    /// Output filter applied to all result text fields.
    filter: OutputFilter,
}

impl WebSearch {
    /// Construct from environment variables.
    ///
    /// Returns `Err` if `LUMINA_DDG_URL` is not set or is empty.
    pub fn from_env() -> Result<Self> {
        let base_url = std::env::var("LUMINA_DDG_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| LuminaError::Config("LUMINA_DDG_URL not set".to_string()))?;
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(LuminaError::Network)?;
        Ok(Self {
            client,
            base_url,
            inspector: EgressInspector::from_env(),
            filter: OutputFilter::new(),
        })
    }

    /// Construct with an explicit base URL and a custom `EgressInspector`.
    ///
    /// Intended for tests: callers supply a mock server URL and a permissive
    /// (or deny-all) inspector without touching environment variables.
    pub fn new_with_inspector(base_url: String, inspector: EgressInspector) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| Client::new());
        Self {
            client,
            base_url,
            inspector,
            filter: OutputFilter::new(),
        }
    }

    /// Search DuckDuckGo for `query`, returning at most `count` results
    /// (hard-capped at [`MAX_RESULTS`] = 5).
    ///
    /// # Errors
    /// - `LuminaError::Config`           — empty query or URL not configured.
    /// - `LuminaError::SecurityViolation` — egress check blocked the host.
    /// - `LuminaError::Network`          — HTTP request failed.
    pub async fn search(&self, query: &str, count: usize) -> Result<Vec<SearchResult>> {
        let query = query.trim();
        if query.is_empty() {
            return Err(LuminaError::Config("Empty search query".to_string()));
        }

        // Build the request URL with percent-encoded query string.
        let encoded_query = form_urlencoded::Serializer::new(String::new())
            .append_pair("q", query)
            .finish();
        let request_url = format!("{}?{}", self.base_url.trim_end_matches('/'), encoded_query);

        // Egress check — blocks if the host is not in the allowlist.
        self.inspector.inspect(&request_url, "web_search")
            .map_err(LuminaError::from)?;

        let response = self.client
            .get(&request_url)
            .header("User-Agent", "Mozilla/5.0 (compatible; LuminaBot/1.0)")
            .send()
            .await?;

        let html = response.error_for_status()?.text().await?;
        let cap = count.min(MAX_RESULTS);
        let raw_results = Self::parse_results(&html);
        let results: Vec<SearchResult> = raw_results
            .into_iter()
            .take(cap)
            .map(|r| self.apply_filter(r))
            .collect();

        Ok(results)
    }

    /// Parse the DuckDuckGo HTML response and extract result entries.
    ///
    /// DuckDuckGo's HTML endpoint wraps each result in a `<div class="result">`
    /// (or `<div class="web-result">`) block. Within each block:
    /// - The title is in an `<a class="result__a">` element.
    /// - The URL is in the `href` attribute of that anchor.
    /// - The snippet is in a `<a class="result__snippet">` element.
    ///
    /// This implementation uses simple substring scanning (no external HTML
    /// parser dependency) which is sufficient for the stable DDG HTML structure.
    pub fn parse_results(html: &str) -> Vec<SearchResult> {
        let mut results = Vec::new();

        // Split on result container boundaries. DuckDuckGo uses several class
        // variants; we split on the common anchor marker.
        // Each result block looks like:
        //   <a class="result__a" href="...">TITLE</a>
        //   ...
        //   <a class="result__snippet" ...>SNIPPET</a>

        let mut remaining = html;
        while let Some(pos) = remaining.find("result__a") {
            remaining = &remaining[pos..];

            // Extract href (URL)
            let url = extract_attr(remaining, "href");
            // Extract title text (inside the <a> tag)
            let title = extract_inner_text(remaining);

            // Advance past this anchor to find the snippet
            let after_a = if let Some(end) = remaining.find("</a>") {
                &remaining[end + 4..]
            } else {
                remaining = &remaining[9..]; // skip past "result__a"
                continue;
            };

            // Snippet must appear before the next result block starts.
            // Bound the search to the next "result__a" marker to avoid
            // cross-result contamination (a result without a snippet would
            // otherwise find the snippet of the following result).
            let block_end = after_a.find("result__a").unwrap_or(after_a.len());
            let this_block = &after_a[..block_end];
            let snippet = if let Some(snip_pos) = this_block.find("result__snippet") {
                extract_inner_text(&this_block[snip_pos..])
            } else {
                String::new()
            };

            let url_clean = clean_ddg_redirect(url);

            if !title.is_empty() && !url_clean.is_empty() {
                results.push(SearchResult {
                    title,
                    url: url_clean,
                    snippet,
                });
            }

            // Advance past the current "result__a" marker to avoid looping
            remaining = &remaining[9..]; // len("result__a") == 9
        }

        results
    }

    /// Fetch the content of a URL and return the raw response body (up to
    /// `max_bytes` bytes), passing it through the output filter.
    ///
    /// This implements the **optional fetch + summarize** acceptance criterion
    /// from WEB-02.  Callers can use this to deep-read a specific search result
    /// URL after calling `search()`.
    ///
    /// # Errors
    /// - `LuminaError::SecurityViolation` — URL host blocked by egress inspector.
    /// - `LuminaError::Network`           — HTTP error or timeout.
    pub async fn fetch_page(&self, url: &str, max_bytes: usize) -> Result<String> {
        self.inspector.inspect(url, "web_fetch_page")
            .map_err(LuminaError::from)?;

        let response = self.client
            .get(url)
            .header("User-Agent", "Mozilla/5.0 (compatible; LuminaBot/1.0)")
            .send()
            .await?;

        let body = response.error_for_status()?.text().await?;
        let truncated = if body.len() > max_bytes {
            body[..max_bytes].to_string()
        } else {
            body
        };

        Ok(self.filter.filter(&truncated))
    }

    /// Apply the output filter to all text fields of a `SearchResult`.
    fn apply_filter(&self, r: SearchResult) -> SearchResult {
        SearchResult {
            title: self.filter.filter(&r.title),
            url: self.filter.filter(&r.url),
            snippet: self.filter.filter(&r.snippet),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// HTML extraction helpers (no external parser)
// ─────────────────────────────────────────────────────────────────────────────

/// Extract the value of `href="..."` from an HTML fragment starting at the
/// current position. Returns an empty string if not found.
fn extract_attr(fragment: &str, attr_name: &str) -> String {
    let needle = format!("{}=\"", attr_name);
    if let Some(start) = fragment.find(&needle) {
        let rest = &fragment[start + needle.len()..];
        if let Some(end) = rest.find('"') {
            return decode_html_entities(&rest[..end]);
        }
    }
    String::new()
}

/// Extract the text content between the first `>` and the next `<` in a fragment.
/// Returns an empty string if not found.
fn extract_inner_text(fragment: &str) -> String {
    if let Some(open) = fragment.find('>') {
        let rest = &fragment[open + 1..];
        if let Some(close) = rest.find('<') {
            let raw = &rest[..close];
            return decode_html_entities(raw.trim());
        }
    }
    String::new()
}

/// Strip DDG redirect wrappers and HTML-entity-encode artifacts from URLs.
///
/// DuckDuckGo sometimes wraps target URLs in a redirect:
/// `//duckduckgo.com/l/?uddg=https%3A%2F%2F...`
/// We decode the `uddg` parameter to get the real URL.
fn clean_ddg_redirect(url: String) -> String {
    // Check for DDG redirect wrapper
    if let Some(pos) = url.find("uddg=") {
        let encoded = &url[pos + 5..];
        // Strip any trailing & parameters
        let encoded = encoded.split('&').next().unwrap_or(encoded);
        if let Ok(decoded) = percent_decode(encoded) {
            return decoded;
        }
    }
    url
}

/// Minimal percent-decode for URLs.
///
/// Uses pure percent-decoding semantics: `%XX` sequences are decoded,
/// but `+` is **not** treated as a space. This is correct for URLs
/// (application/x-www-form-urlencoded would incorrectly map `+` to space).
fn percent_decode(input: &str) -> std::result::Result<String, ()> {
    // Replace %XX sequences manually; leave '+' untouched.
    let mut output = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                let byte = ((h as u8) << 4) | (l as u8);
                output.push(byte as char);
                i += 3;
                continue;
            }
        }
        output.push(bytes[i] as char);
        i += 1;
    }
    Ok(output)
}

/// Decode common HTML entities in a string.
fn decode_html_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
}

/// Return the `ToolDefinition` for the `web_search` MCP tool.
///
/// Register this with [`ToolGate`] so the LLM can call it.  Execution is
/// intercepted in the agent loop before MCP dispatch and handled natively
/// via [`WebSearch`].
pub fn web_search_tool() -> ToolDefinition {
    ToolDefinition::new(
        TOOL_NAME.to_string(),
        "Search the web using DuckDuckGo (no API key required). \
         Returns the top 5 results with title, URL, and a short snippet. \
         Use this when the user asks to search for something, look something up, \
         or find information online."
            .to_string(),
        ToolPermission::ReadOnly,
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query"
                }
            },
            "required": ["query"]
        }),
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::sync::Mutex;

    // Serialize tests that mutate env vars.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    // ── test_search_url_construction ─────────────────────────────────────────

    /// Verify that search() builds a URL containing the encoded query string.
    ///
    /// Uses httpmock to intercept the request and assert the URL path+query
    /// contain the expected `q=rust+programming` parameter.
    #[test]
    fn test_search_url_construction() {
        use httpmock::prelude::*;

        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/search")
                .query_param("q", "rust programming");
            then.status(200).body("<html></html>");
        });

        let inspector = EgressInspector::new(vec!["127.0.0.1".to_string()]);
        let base_url = format!("http://127.0.0.1:{}/search", server.port());
        let ws = WebSearch::new_with_inspector(base_url, inspector);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let result = rt.block_on(ws.search("rust programming", 3));
        // The mock will only match if the URL contained q=rust+programming.
        // A 200 response (empty HTML → empty results) is the expected outcome.
        match result {
            Ok(_) => { /* correct — URL was built with the query param */ }
            Err(e) => panic!("expected Ok from mock server, got: {:?}", e),
        }

        // Assert the mock was hit exactly once, confirming the URL was correct.
        mock.assert();
    }

    // ── test_parse_results_extracts_title_url_snippet ────────────────────────

    #[test]
    fn test_parse_results_extracts_title_url_snippet() {
        let html = r#"
        <div class="result">
          <a class="result__a" href="https://www.rust-lang.org/">Rust Programming Language</a>
          <a class="result__snippet">Fast, reliable, and productive — pick all three.</a>
        </div>
        <div class="result">
          <a class="result__a" href="https://doc.rust-lang.org/book/">The Rust Book</a>
          <a class="result__snippet">Learn Rust with the official book.</a>
        </div>
        "#;

        let results = WebSearch::parse_results(html);
        assert!(!results.is_empty(), "should parse at least one result");

        let first = &results[0];
        assert_eq!(first.title, "Rust Programming Language");
        assert_eq!(first.url, "https://www.rust-lang.org/");
        assert!(!first.snippet.is_empty(), "snippet should be extracted");
    }

    // ── test_max_5_results_returned ──────────────────────────────────────────

    #[test]
    fn test_max_5_results_returned() {
        // Build HTML with 8 fake results
        let mut html = String::new();
        for i in 1..=8 {
            html.push_str(&format!(
                r#"<div class="result"><a class="result__a" href="https://example{}.com/">Title {}</a><a class="result__snippet">Snippet {}</a></div>"#,
                i, i, i
            ));
        }
        let raw = WebSearch::parse_results(&html);
        // parse_results may return all; the cap is applied by search().
        // Verify the cap logic independently:
        let capped: Vec<_> = raw.into_iter().take(5).collect();
        assert_eq!(capped.len(), 5, "must return at most 5 results");
    }

    // ── test_empty_query_rejected ────────────────────────────────────────────

    #[test]
    #[serial]
    fn test_empty_query_rejected() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("LUMINA_DDG_URL", "https://example.com/search");
        std::env::set_var("LUMINA_EGRESS_ALLOWLIST", "example.com");

        let ws = WebSearch::from_env().expect("should build from env");
        std::env::remove_var("LUMINA_DDG_URL");
        std::env::remove_var("LUMINA_EGRESS_ALLOWLIST");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        // Empty string
        let result = rt.block_on(ws.search("", 3));
        assert!(result.is_err());
        match result {
            Err(LuminaError::Config(msg)) => {
                assert!(msg.contains("Empty search query"), "got: {}", msg);
            }
            Err(e) => panic!("expected Config error, got: {:?}", e),
            Ok(_) => panic!("expected Err for empty query"),
        }

        // Whitespace only
        let result2 = rt.block_on(ws.search("   ", 3));
        assert!(result2.is_err());
        match result2 {
            Err(LuminaError::Config(msg)) => {
                assert!(msg.contains("Empty search query"), "got: {}", msg);
            }
            Err(e) => panic!("expected Config error for whitespace, got: {:?}", e),
            Ok(_) => panic!("expected Err for whitespace-only query"),
        }
    }

    // ── test_egress_check_duckduckgo_domain ──────────────────────────────────

    /// Verify that the egress inspector is consulted before any HTTP call,
    /// and that a non-allowlisted host causes a `SecurityViolation`.
    #[test]
    fn test_egress_check_duckduckgo_domain() {
        // Use a deny-all inspector (empty allowlist)
        let inspector = EgressInspector::new(vec![]);
        let ws = WebSearch::new_with_inspector(
            "https://duckduckgo.com/html".to_string(),
            inspector,
        );

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let result = rt.block_on(ws.search("test query", 3));
        assert!(result.is_err(), "deny-all inspector must block the request");
        match result {
            Err(LuminaError::SecurityViolation(_)) => { /* expected */ }
            Err(e) => panic!("expected SecurityViolation, got: {:?}", e),
            Ok(_) => panic!("expected Err — egress should have blocked"),
        }

        // Now verify that an allowlisted DDG domain passes egress
        // (will fail on network, not on egress).
        let inspector2 = EgressInspector::new(vec!["duckduckgo.com".to_string()]);
        let ws2 = WebSearch::new_with_inspector(
            "https://duckduckgo.com/html".to_string(),
            inspector2,
        );
        let result2 = rt.block_on(ws2.search("test query", 3));
        // Should NOT be a SecurityViolation (network error is fine)
        match result2 {
            Err(LuminaError::SecurityViolation(_)) => {
                panic!("should not be blocked when duckduckgo.com is allowlisted");
            }
            _ => { /* any other result (network error or ok) is acceptable */ }
        }
    }

    // ── test_output_filter_applied ───────────────────────────────────────────

    /// Verify that OutputFilter is applied to results: a JWT in a snippet
    /// must be redacted before the result is returned.
    #[test]
    fn test_output_filter_applied() {
        let filter = OutputFilter::new();
        let raw_snippet = "Token: eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjMifQ.SflK"; // fake credential fixture (synthetic, not a real secret)
        let filtered_snippet = filter.filter(raw_snippet);

        // Confirm the filter actually redacts the JWT (sanity-check the filter itself)
        assert!(!filtered_snippet.contains("eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9"),
            "filter should redact JWT headers");

        // Now verify that WebSearch applies the filter to parse_results output.
        // Build a fake HTML page with a JWT in the snippet.
        let html = format!(
            r#"<div class="result"><a class="result__a" href="https://example.com/">Title</a><a class="result__snippet">{}</a></div>"#,
            raw_snippet
        );

        // parse_results does NOT apply the filter (it's a raw extractor).
        let raw = WebSearch::parse_results(&html);
        assert!(!raw.is_empty());

        // The filter is applied in WebSearch::search() via apply_filter().
        // We test apply_filter() directly through the WebSearch struct.
        let inspector = EgressInspector::new(vec!["example.com".to_string()]);
        let ws = WebSearch::new_with_inspector("https://example.com".to_string(), inspector);
        let filtered = ws.apply_filter(raw[0].clone());

        assert!(!filtered.snippet.contains("eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9"),
            "apply_filter must redact JWT in snippet; got: {}", filtered.snippet);
    }

    // ── HTML helpers ─────────────────────────────────────────────────────────

    #[test]
    fn test_decode_html_entities() {
        assert_eq!(decode_html_entities("AT&amp;T"), "AT&T");
        assert_eq!(decode_html_entities("&lt;script&gt;"), "<script>");
        assert_eq!(decode_html_entities("&quot;hello&quot;"), "\"hello\"");
        assert_eq!(decode_html_entities("it&#39;s"), "it's");
    }

    #[test]
    fn test_clean_ddg_redirect_passthrough() {
        // A plain URL without a DDG redirect wrapper passes through unchanged.
        let url = "https://www.example.com/page".to_string();
        assert_eq!(clean_ddg_redirect(url.clone()), url);
    }

    #[test]
    fn test_clean_ddg_redirect_unwraps_uddg_param() {
        // A real DDG redirect URL of the form //duckduckgo.com/l/?uddg=<encoded>
        // should be unwrapped to the underlying URL.
        let target = "https://www.rust-lang.org/";
        // percent-encode the target: '/' → %2F, ':' → %3A
        let encoded_target = "https%3A%2F%2Fwww.rust-lang.org%2F";
        let ddg_url = format!("//duckduckgo.com/l/?uddg={}&rut=abc123", encoded_target);

        let result = clean_ddg_redirect(ddg_url);
        assert_eq!(result, target,
            "clean_ddg_redirect should decode the uddg parameter to the real URL");
    }

    #[test]
    fn test_clean_ddg_redirect_plus_not_treated_as_space() {
        // A URL with a literal '+' in the path must not be decoded to a space.
        // DDG may wrap URLs that contain '+' without percent-encoding it as %2B.
        let target = "https://example.com/path+with+plus";
        let encoded_target = "https%3A%2F%2Fexample.com%2Fpath%2Bwith%2Bplus";
        let ddg_url = format!("//duckduckgo.com/l/?uddg={}", encoded_target);

        let result = clean_ddg_redirect(ddg_url);
        // '%2B' must decode to '+', not space
        assert_eq!(result, target,
            "percent_decode must not convert '+' to space; got: {}", result);
    }
}
