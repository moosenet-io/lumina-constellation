//! WEB-01: Headless web client with content sanitization.
//!
//! Provides [`WebClient`] for fetching web pages, sanitizing HTML, and
//! extracting readable text content from arbitrary URLs.
//!
//! # Security
//! - Every URL is checked against the egress allowlist before the HTTP request
//!   is made.
//! - Response bodies are rejected if they exceed [`MAX_RESPONSE_BYTES`]; the
//!   limit is enforced *while streaming* so no oversized body is ever fully
//!   buffered in memory.
//! - Rate limiting: no more than [`RATE_LIMIT_REQUESTS`] fetches per
//!   [`RATE_LIMIT_WINDOW_SECS`]-second window per user.
//! - Fetched pages are cached in memory for [`CACHE_TTL_SECS`] seconds.
//! - All extracted text passes through the output filter before being returned.
//!
//! # Tools
//! Call [`web_browse_tool`] to obtain the `ToolDefinition` for the `web_browse`
//! MCP tool.

pub mod sanitizer;
pub mod extractor;
pub mod search;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;

use crate::egress_inspector::EgressInspector;
use crate::error::{LuminaError, Result};
use crate::security::output_filter::filter_output;
use crate::tool_types::{ToolDefinition, ToolPermission};

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

/// Maximum response body size (5 MB). Responses larger than this are rejected.
pub const MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;

/// HTTP request timeout.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// User-agent header sent with every request.
const USER_AGENT: &str = "Lumina/0.3 (Personal AI Assistant)";

/// Maximum number of fetches allowed per user per rate-limit window.
const RATE_LIMIT_REQUESTS: u32 = 10;

/// Rate-limit observation window in seconds.
const RATE_LIMIT_WINDOW_SECS: u64 = 60;

/// Page cache TTL (5 minutes).
pub const CACHE_TTL_SECS: u64 = 300;

/// Maximum redirect hops before aborting.
const MAX_REDIRECTS: usize = 10;

/// Tool name constant so callers can reference it without magic strings.
pub const TOOL_NAME: &str = "web_browse";

// Summarize heuristic tuning parameters.

/// Minimum paragraph length (characters) to be included in a heuristic summary.
const SUMMARIZE_MIN_PARAGRAPH_LEN: usize = 40;

/// Maximum number of paragraphs returned by the heuristic summarizer.
const SUMMARIZE_MAX_PARAGRAPHS: usize = 3;

/// Character limit for the fallback summary when no long paragraphs are found.
const SUMMARIZE_FALLBACK_CHARS: usize = 500;

// ─────────────────────────────────────────────────────────────────────────────
// WebPage
// ─────────────────────────────────────────────────────────────────────────────

/// A fetched web page with sanitized, extracted content.
#[derive(Debug, Clone)]
pub struct WebPage {
    /// Canonical URL after any redirects.
    pub url: String,
    /// Page title extracted from `<title>…</title>`.
    pub title: String,
    /// Readable plain-text content extracted from the HTML body.
    pub content: String,
    /// Value of the `Content-Type` response header.
    pub content_type: String,
    /// HTTP status code.
    pub status_code: u16,
    /// When this page was fetched.
    pub fetched_at: Instant,
}

// ─────────────────────────────────────────────────────────────────────────────
// Rate limiter (per-user sliding window)
// ─────────────────────────────────────────────────────────────────────────────

/// Per-user rate-limit state.
struct UserRateState {
    /// When the current observation window started.
    window_start: Instant,
    /// Number of requests issued in the current window.
    count: u32,
}

/// In-process per-user rate limiter.
///
/// Each user gets up to [`RATE_LIMIT_REQUESTS`] fetches per
/// [`RATE_LIMIT_WINDOW_SECS`] seconds. Calling [`RateLimiter::check`]
/// increments the counter and returns an error if the limit would be exceeded.
struct RateLimiter {
    users: Mutex<HashMap<String, UserRateState>>,
}

impl RateLimiter {
    fn new() -> Self {
        Self {
            users: Mutex::new(HashMap::new()),
        }
    }

    /// Check and record a request for `user_id`.
    ///
    /// Returns `Ok(())` if the request is within the rate limit, or
    /// `Err(LuminaError::SecurityViolation)` if the limit has been exceeded.
    ///
    /// Also evicts entries for users whose rate-limit window has expired so the
    /// map does not grow unbounded in a long-running server with many unique
    /// callers. Eviction is O(n) in the number of current entries and happens
    /// on every call; for typical deployments (dozens of users) this is
    /// negligible compared to I/O.
    ///
    /// # Mutex poison recovery
    /// If a previous thread panicked while holding this lock the poisoned guard
    /// is recovered via `into_inner()`. The state inside is still valid (no
    /// invariants were broken at the time of panic) and we prefer continuing to
    /// crash the whole server.
    fn check(&self, user_id: &str) -> Result<()> {
        // NOTE: unwrap_or_else(|e| e.into_inner()) intentionally recovers from
        // mutex poison. See doc comment above for rationale.
        let mut map = self.users.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();

        // Evict entries whose window has fully expired to prevent unbounded growth.
        map.retain(|_, state| {
            state.window_start.elapsed().as_secs() < RATE_LIMIT_WINDOW_SECS
        });

        let state = map.entry(user_id.to_string()).or_insert_with(|| UserRateState {
            window_start: now,
            count: 0,
        });

        // Reset window if it has passed (entry was not evicted, meaning elapsed
        // was at the boundary — handle the race conservatively).
        if state.window_start.elapsed().as_secs() >= RATE_LIMIT_WINDOW_SECS {
            state.window_start = now;
            state.count = 0;
        }

        if state.count >= RATE_LIMIT_REQUESTS {
            return Err(LuminaError::SecurityViolation(format!(
                "Rate limit exceeded: web_browse allows at most {} requests per {} seconds per user",
                RATE_LIMIT_REQUESTS, RATE_LIMIT_WINDOW_SECS
            )));
        }

        state.count += 1;
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Page cache
// ─────────────────────────────────────────────────────────────────────────────

struct CacheEntry {
    page: WebPage,
    inserted_at: Instant,
}

struct PageCache {
    entries: Mutex<HashMap<String, CacheEntry>>,
}

impl PageCache {
    fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Look up a URL. Returns `None` if the entry is absent or has expired.
    ///
    /// Mutex poison is recovered via `into_inner()`; a poisoned cache simply
    /// returns cache misses and continues operating.
    fn get(&self, url: &str) -> Option<WebPage> {
        // NOTE: unwrap_or_else(|e| e.into_inner()) intentionally recovers from
        // mutex poison — a cache miss is safe and correct on poison.
        let map = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        map.get(url).and_then(|entry| {
            if entry.inserted_at.elapsed().as_secs() < CACHE_TTL_SECS {
                Some(entry.page.clone())
            } else {
                None
            }
        })
    }

    /// Insert a page into the cache.
    ///
    /// Mutex poison is recovered via `into_inner()`; failing to cache a page
    /// is non-fatal (the page was already fetched successfully).
    fn insert(&self, url: &str, page: WebPage) {
        // NOTE: unwrap_or_else(|e| e.into_inner()) intentionally recovers from
        // mutex poison — failing to cache is non-fatal.
        let mut map = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        map.insert(
            url.to_string(),
            CacheEntry {
                page,
                inserted_at: Instant::now(),
            },
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// WebClient
// ─────────────────────────────────────────────────────────────────────────────

/// Headless web client with HTML sanitization and content extraction.
///
/// Wrap in an `Arc` to share across tasks.
pub struct WebClient {
    http: reqwest::Client,
    egress: Arc<EgressInspector>,
    rate_limiter: RateLimiter,
    cache: PageCache,
}

impl WebClient {
    /// Create a new `WebClient` with the provided egress inspector.
    pub fn new(egress: Arc<EgressInspector>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent(USER_AGENT)
            .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
            .build()
            .map_err(|e| LuminaError::Config(format!("Failed to build HTTP client: {}", e)))?;

        Ok(Self {
            http,
            egress,
            rate_limiter: RateLimiter::new(),
            cache: PageCache::new(),
        })
    }

    /// Create a `WebClient` with an egress inspector loaded from the environment.
    pub fn from_env() -> Result<Self> {
        Self::new(Arc::new(EgressInspector::from_env()))
    }

    // ── Public API ────────────────────────────────────────────────────────────

    /// Fetch a URL and return a sanitized [`WebPage`].
    ///
    /// - Validates the URL is non-empty.
    /// - Checks the egress allowlist.
    /// - Serves from cache if a fresh entry exists (cache lookup precedes rate-limit
    ///   check so that cache hits do not consume rate-limit tokens).
    /// - Enforces the per-user rate limit (only charged for real network fetches).
    /// - Rejects responses larger than [`MAX_RESPONSE_BYTES`].
    /// - For `text/html` responses: sanitizes HTML and extracts readable text.
    /// - For non-HTML content (PDF, image, etc.): returns the content type with empty
    ///   content rather than attempting garbage text extraction.
    /// - Passes extracted content through the output filter.
    ///
    /// # robots.txt
    /// robots.txt is intentionally not checked in this implementation. All fetches
    /// are performed as an authenticated personal assistant acting on behalf of the
    /// operator; the egress allowlist provides the equivalent access-control layer.
    /// A future revision may add robots.txt enforcement for public-web use cases.
    pub async fn fetch(&self, url: &str, user_id: &str) -> Result<WebPage> {
        // 1. Reject empty URLs.
        if url.trim().is_empty() {
            return Err(LuminaError::Config("URL must not be empty".to_string()));
        }

        // 2. Egress allowlist check.
        self.egress
            .inspect(url, TOOL_NAME)
            .map_err(LuminaError::from)?;

        // 3. Cache lookup — before rate-limit check so cache hits are free.
        //    We look up by the *requested* URL (pre-redirect). On insert we store
        //    under both the requested URL and the post-redirect final URL so that
        //    either form hits the cache on the next call.
        if let Some(cached) = self.cache.get(url) {
            log::debug!("web_browse cache hit: {}", url);
            return Ok(cached);
        }

        // 4. Rate limit check — only reached when the cache misses (real network fetch).
        self.rate_limiter.check(user_id)?;

        // 5. HTTP GET.
        let response = self.http.get(url).send().await.map_err(LuminaError::from)?;

        let status_code = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("text/html")
            .to_string();
        let final_url = response.url().to_string();

        // 6. Stream body — abort as soon as accumulated bytes exceed the limit
        //    so a multi-gigabyte body (with no Content-Length) never fully
        //    enters memory.  We allocate no more than MAX_RESPONSE_BYTES + the
        //    size of one chunk.
        let mut body_bytes: Vec<u8> = Vec::with_capacity(
            (MAX_RESPONSE_BYTES / 2).min(64 * 1024), // reasonable pre-alloc
        );
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(LuminaError::from)?;
            if body_bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(LuminaError::Config(format!(
                    "Response too large: exceeded limit of {} bytes",
                    MAX_RESPONSE_BYTES
                )));
            }
            body_bytes.extend_from_slice(&chunk);
        }

        // 7. Decode body.
        let raw_body = String::from_utf8_lossy(&body_bytes).into_owned();

        // 8. Content-type guard.
        //    Only attempt HTML title extraction and sanitization for text/html
        //    responses. For PDFs, images, binary data, or other non-HTML types
        //    the extraction pipeline produces garbage text; we skip it and return
        //    the content type string as the content instead, which gives the LLM
        //    enough context to explain the situation to the user.
        let is_html = content_type.to_lowercase().contains("text/html");

        let (title, content) = if is_html {
            // 8a. Extract title.
            let title = extractor::extract_title(&raw_body);

            // 8b. Sanitize → extract text.
            //     Note on unclosed tags: `remove_tags_with_content` requires a
            //     matching closing tag, so a malformed `<script>alert(1)` with no
            //     `</script>` will not be removed at the sanitize stage. However,
            //     `strip_all_tags` in the extractor then strips the lone open tag,
            //     leaving `alert(1)` as plain text. This is NOT an XSS vector
            //     because output is plain text fed to the LLM and never rendered
            //     as HTML. The residue is benign and fully visible to the model.
            let sanitized = sanitizer::sanitize(&raw_body);
            let raw_text = extractor::extract_text(&sanitized);

            // 8c. Output filter.
            let filtered = filter_output(&raw_text);
            (title, filtered.to_string())
        } else {
            // Non-HTML: skip extraction entirely. Return the content-type so the
            // agent can explain to the user what was found.
            (String::new(), format!("[Non-HTML content: {}]", content_type))
        };

        let page = WebPage {
            url: final_url.clone(),
            title,
            content,
            content_type,
            status_code,
            fetched_at: Instant::now(),
        };

        // 9. Cache under both the original requested URL and the final (post-redirect)
        //    URL. This ensures that:
        //    - A second fetch of the same original URL hits the cache (redirect case).
        //    - A direct fetch of the canonical URL also hits the cache.
        //    If `url == final_url` (no redirect) we insert once; the second insert
        //    is a no-op equivalent (same key, same entry) which is harmless.
        self.cache.insert(url, page.clone());
        if final_url != url {
            self.cache.insert(&final_url, page.clone());
        }

        Ok(page)
    }

    /// Extract readable text from an HTML string directly (no network call).
    ///
    /// Useful for processing HTML that has already been fetched or is available
    /// in memory.
    pub fn extract_text(&self, html: &str) -> String {
        let sanitized = sanitizer::sanitize(html);
        extractor::extract_text(&sanitized)
    }

    /// Produce a brief summary of extracted text.
    ///
    /// This is intentionally a lightweight heuristic — it returns the first
    /// `n` non-empty paragraphs of text. For LLM-based summarization, callers
    /// should pass `page.content` directly to the model.
    pub fn summarize(&self, text: &str) -> Result<String> {
        if text.trim().is_empty() {
            return Ok(String::new());
        }

        // Return the first SUMMARIZE_MAX_PARAGRAPHS meaningful paragraphs
        // (longer than SUMMARIZE_MIN_PARAGRAPH_LEN chars) as a summary.
        let paragraphs: Vec<&str> = text
            .split('\n')
            .map(|l| l.trim())
            .filter(|l| l.len() > SUMMARIZE_MIN_PARAGRAPH_LEN)
            .take(SUMMARIZE_MAX_PARAGRAPHS)
            .collect();

        if paragraphs.is_empty() {
            // Fall back to first SUMMARIZE_FALLBACK_CHARS chars of the full text.
            let truncated = text.chars().take(SUMMARIZE_FALLBACK_CHARS).collect::<String>();
            return Ok(truncated.trim().to_string());
        }

        Ok(paragraphs.join("\n\n"))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tool definition
// ─────────────────────────────────────────────────────────────────────────────

/// Return the `ToolDefinition` for the `web_browse` MCP tool.
pub fn web_browse_tool() -> ToolDefinition {
    ToolDefinition::new(
        TOOL_NAME.to_string(),
        "Fetch a web page and return its readable text content. \
         The URL must be in the egress allowlist. \
         Returns sanitized, extracted text suitable for summarization or analysis."
            .to_string(),
        ToolPermission::ReadOnly,
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch. Must use http:// or https:// scheme."
                },
                "user_id": {
                    "type": "string",
                    "description": "Identifier for rate-limiting (e.g. matrix user ID)."
                }
            },
            "required": ["url", "user_id"]
        }),
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;
    use std::sync::Arc;

    // ── helpers ───────────────────────────────────────────────────────────────

    /// Create a WebClient that allows only the given mock server host.
    fn client_for_mock(server: &MockServer) -> WebClient {
        let host = format!("127.0.0.1:{}", server.port());
        let egress = Arc::new(EgressInspector::new(vec![host, "127.0.0.1".to_string(), "localhost".to_string()]));
        WebClient::new(egress).expect("client creation must succeed")
    }

    // ── test_html_sanitization_strips_scripts (integration path through client) ─

    #[tokio::test]
    async fn test_html_sanitization_strips_scripts() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/page");
            then.status(200)
                .header("Content-Type", "text/html")
                .body("<html><body><script>alert('xss')</script><p>Safe content</p></body></html>");
        });

        let client = client_for_mock(&server);
        let url = format!("http://127.0.0.1:{}/page", server.port());
        let page = client.fetch(&url, "user1").await.expect("fetch must succeed");

        assert!(!page.content.contains("alert("), "script content must not appear in extracted text");
        assert!(!page.content.contains("<script"), "script tag must not appear");
        assert!(page.content.contains("Safe content"), "body content must be preserved");
    }

    // ── test_html_sanitization_strips_tracking_pixels ──────────────────────────

    #[tokio::test]
    async fn test_html_sanitization_strips_tracking_pixels() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/page");
            then.status(200)
                .header("Content-Type", "text/html")
                .body(r#"<html><body><p>Article</p><img src="https://track.example.com/p.gif" width="1" height="1" alt=""><p>End</p></body></html>"#);
        });

        let client = client_for_mock(&server);
        let url = format!("http://127.0.0.1:{}/page", server.port());
        let page = client.fetch(&url, "user1").await.expect("fetch must succeed");

        assert!(!page.content.contains("track.example.com"), "tracking pixel URL must not appear in content");
        assert!(page.content.contains("Article"), "article text preserved");
    }

    // ── test_text_extraction_from_complex_html ─────────────────────────────────

    #[tokio::test]
    async fn test_text_extraction_from_complex_html() {
        let server = MockServer::start();
        let html = r#"<html>
            <head><title>Test Page</title><style>body{margin:0}</style></head>
            <body>
              <nav><a href="/">Home</a></nav>
              <article>
                <h1>Article Title</h1>
                <p>First paragraph with <a href="https://example.com">a link</a>.</p>
                <ul><li>Item A</li><li>Item B</li></ul>
              </article>
              <footer>Footer text</footer>
              <script>var x = 1;</script>
            </body></html>"#;
        server.mock(|when, then| {
            when.method(GET).path("/complex");
            then.status(200).header("Content-Type", "text/html").body(html);
        });

        let client = client_for_mock(&server);
        let url = format!("http://127.0.0.1:{}/complex", server.port());
        let page = client.fetch(&url, "user1").await.expect("fetch must succeed");

        assert_eq!(page.title, "Test Page", "title extracted correctly");
        assert!(page.content.contains("Article Title"), "h1 preserved: {}", page.content);
        assert!(page.content.contains("First paragraph"), "p preserved: {}", page.content);
        assert!(page.content.contains("Item A"), "li preserved: {}", page.content);
        assert!(!page.content.contains("<script"), "script removed: {}", page.content);
        assert!(!page.content.contains("var x"), "script content removed: {}", page.content);
    }

    // ── test_egress_inspector_blocks_non_allowlisted ───────────────────────────

    #[tokio::test]
    async fn test_egress_inspector_blocks_non_allowlisted() {
        // Inspector with empty allowlist → deny everything
        let egress = Arc::new(EgressInspector::new(vec![]));
        let client = WebClient::new(egress).expect("client creation must succeed");

        let result = client.fetch("http://example.com/page", "user1").await;
        assert!(result.is_err(), "non-allowlisted URL must be blocked");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("egress") || err.contains("Security") || err.contains("allowlist"),
            "error message must mention egress/security: {}",
            err
        );
    }

    // ── test_max_response_size_enforced ───────────────────────────────────────

    #[tokio::test]
    async fn test_max_response_size_enforced() {
        let server = MockServer::start();
        // 5 MB + 1 byte body — just over the limit
        let oversized_body = vec![b'A'; MAX_RESPONSE_BYTES + 1];
        server.mock(|when, then| {
            when.method(GET).path("/big");
            then.status(200)
                .header("Content-Type", "text/html")
                .body(oversized_body);
        });

        let client = client_for_mock(&server);
        let url = format!("http://127.0.0.1:{}/big", server.port());
        let result = client.fetch(&url, "user1").await;

        assert!(result.is_err(), "oversized response must be rejected");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("too large") || err.contains("5") || err.contains("limit"),
            "error must mention size limit: {}",
            err
        );
    }

    // ── test_rate_limiting_enforced ────────────────────────────────────────────

    #[tokio::test]
    async fn test_rate_limiting_enforced() {
        let server = MockServer::start();
        // Endpoint that always returns a small page
        server.mock(|when, then| {
            when.method(GET).path_matches(regex::Regex::new(".*").unwrap());
            then.status(200)
                .header("Content-Type", "text/html")
                .body("<p>page</p>");
        });

        let client = client_for_mock(&server);
        let user = "rate_test_user";

        // First RATE_LIMIT_REQUESTS calls should succeed (cached after first)
        for i in 0..RATE_LIMIT_REQUESTS {
            let url = format!("http://127.0.0.1:{}/r{}", server.port(), i);
            let result = client.fetch(&url, user).await;
            assert!(result.is_ok(), "call {} should succeed: {:?}", i + 1, result);
        }

        // 11th call (on same user, same window) must be blocked
        let url = format!("http://127.0.0.1:{}/r_extra", server.port());
        let result = client.fetch(&url, user).await;
        assert!(result.is_err(), "11th call must be rate limited");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Rate limit") || err.contains("Security"),
            "error must indicate rate limit: {}",
            err
        );
    }

    // ── test_cache_returns_within_window ──────────────────────────────────────

    #[tokio::test]
    async fn test_cache_returns_within_window() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET).path("/cached");
            then.status(200)
                .header("Content-Type", "text/html")
                .body("<p>Cached content</p>");
        });

        let client = client_for_mock(&server);
        let url = format!("http://127.0.0.1:{}/cached", server.port());

        // First fetch — hits the server
        let page1 = client.fetch(&url, "user1").await.expect("first fetch must succeed");
        assert_eq!(mock.hits(), 1, "first fetch should hit server");

        // Second fetch — should come from cache, not the server
        let page2 = client.fetch(&url, "user1").await.expect("second fetch must succeed");
        assert_eq!(mock.hits(), 1, "second fetch must use cache (no new server hit)");

        assert_eq!(page1.content, page2.content, "cached content must match");
    }

    // ── test_empty_url_rejected ────────────────────────────────────────────────

    #[tokio::test]
    async fn test_empty_url_rejected() {
        let egress = Arc::new(EgressInspector::new(vec!["localhost".to_string()]));
        let client = WebClient::new(egress).expect("client creation must succeed");

        let result = client.fetch("", "user1").await;
        assert!(result.is_err(), "empty URL must be rejected");

        let result2 = client.fetch("   ", "user1").await;
        assert!(result2.is_err(), "whitespace-only URL must be rejected");
    }

    // ── test_redirect_chain_too_long ──────────────────────────────────────────

    #[tokio::test]
    async fn test_redirect_chain_too_long() {
        let server = MockServer::start();

        // Create a redirect loop: /r0 → /r1 → /r2 → … → /r11 (11 hops > MAX_REDIRECTS=10)
        for i in 0..=10 {
            let next = format!("http://127.0.0.1:{}/r{}", server.port(), i + 1);
            let path = format!("/r{}", i);
            server.mock(|when, then| {
                when.method(GET).path(path);
                then.status(302).header("Location", next);
            });
        }
        // The terminal endpoint — only reached if redirects are not limited
        server.mock(|when, then| {
            when.method(GET).path("/r11");
            then.status(200).header("Content-Type", "text/html").body("<p>End</p>");
        });

        let client = client_for_mock(&server);
        let url = format!("http://127.0.0.1:{}/r0", server.port());
        let result = client.fetch(&url, "user1").await;

        // reqwest's redirect policy returns an error when the chain is too long
        assert!(result.is_err(), "too many redirects must fail");
    }

    // ── extract_text and summarize unit tests ─────────────────────────────────

    #[test]
    fn test_extract_text_unit() {
        let egress = Arc::new(EgressInspector::new(vec![]));
        let client = WebClient::new(egress).expect("client creation");
        let html = "<p>Hello <strong>world</strong>.</p>";
        let result = client.extract_text(html);
        assert!(result.contains("Hello"), "text extracted: {}", result);
        assert!(result.contains("world"), "nested text extracted: {}", result);
        assert!(!result.contains('<'), "no HTML tags: {}", result);
    }

    #[test]
    fn test_summarize_returns_first_paragraphs() {
        let egress = Arc::new(EgressInspector::new(vec![]));
        let client = WebClient::new(egress).expect("client creation");
        let text = "Short.\n\nThis is a long enough paragraph that exceeds forty characters easily.\n\nAnother paragraph that also exceeds the minimum character threshold.";
        let summary = client.summarize(text).expect("summarize must succeed");
        assert!(!summary.is_empty(), "summary must not be empty");
        assert!(summary.contains("long enough paragraph"), "first long paragraph included");
    }

    #[test]
    fn test_summarize_empty_returns_empty() {
        let egress = Arc::new(EgressInspector::new(vec![]));
        let client = WebClient::new(egress).expect("client creation");
        let summary = client.summarize("").expect("summarize must succeed");
        assert!(summary.is_empty(), "empty text → empty summary");
    }

    // ── tool definition ────────────────────────────────────────────────────────

    #[test]
    fn test_web_browse_tool_definition() {
        let tool = web_browse_tool();
        assert_eq!(tool.name, TOOL_NAME);
        assert_eq!(tool.permission, ToolPermission::ReadOnly);
        assert!(tool.description.contains("web page"), "description mentions web page");
        let schema = &tool.argument_schema;
        assert!(schema["properties"]["url"].is_object(), "url property defined");
        assert!(schema["properties"]["user_id"].is_object(), "user_id property defined");
    }

    // ── WebClient::new ─────────────────────────────────────────────────────────

    #[test]
    fn test_web_client_new_succeeds() {
        let egress = Arc::new(EgressInspector::new(vec!["localhost".to_string()]));
        let result = WebClient::new(egress);
        assert!(result.is_ok(), "WebClient::new must succeed");
    }

    // ── rate limiter unit test ─────────────────────────────────────────────────

    #[test]
    fn test_rate_limiter_allows_up_to_limit() {
        let limiter = RateLimiter::new();
        for i in 0..RATE_LIMIT_REQUESTS {
            assert!(limiter.check("user").is_ok(), "call {} must succeed", i + 1);
        }
        assert!(
            limiter.check("user").is_err(),
            "call {} must be blocked",
            RATE_LIMIT_REQUESTS + 1
        );
    }

    #[test]
    fn test_rate_limiter_different_users_independent() {
        let limiter = RateLimiter::new();
        // Exhaust user_a
        for _ in 0..RATE_LIMIT_REQUESTS {
            limiter.check("user_a").expect("user_a allowed");
        }
        assert!(limiter.check("user_a").is_err(), "user_a exhausted");
        // user_b should still have a fresh window
        assert!(limiter.check("user_b").is_ok(), "user_b must not be affected");
    }

    // ── page cache unit test ───────────────────────────────────────────────────

    #[test]
    fn test_page_cache_stores_and_retrieves() {
        let cache = PageCache::new();
        let page = WebPage {
            url: "http://example.com/".to_string(),
            title: "Example".to_string(),
            content: "Hello world".to_string(),
            content_type: "text/html".to_string(),
            status_code: 200,
            fetched_at: Instant::now(),
        };
        cache.insert("http://example.com/", page.clone());
        let retrieved = cache.get("http://example.com/");
        assert!(retrieved.is_some(), "cache must return stored page");
        assert_eq!(retrieved.unwrap().content, "Hello world");
    }

    #[test]
    fn test_page_cache_miss_on_unknown_url() {
        let cache = PageCache::new();
        assert!(cache.get("http://not-stored.example.com/").is_none());
    }

    // ── test_cache_hit_after_redirect ─────────────────────────────────────────
    //
    // Regression test for the cache-key bug: when a URL redirects, the cache
    // must be keyed by the *original requested* URL so that a second fetch of
    // the same URL (which still redirects) is served from cache instead of
    // hitting the network again.

    #[tokio::test]
    async fn test_cache_hit_after_redirect() {
        let server = MockServer::start();

        // /redirect → 302 → /landing
        let redirect_mock = server.mock(|when, then| {
            when.method(GET).path("/redirect");
            then.status(302)
                .header("Location", format!("http://127.0.0.1:{}/landing", server.port()));
        });
        let landing_mock = server.mock(|when, then| {
            when.method(GET).path("/landing");
            then.status(200)
                .header("Content-Type", "text/html")
                .body("<p>Redirected content</p>");
        });

        let client = client_for_mock(&server);
        let original_url = format!("http://127.0.0.1:{}/redirect", server.port());

        // First fetch — follows redirect, hits the landing page
        let page1 = client
            .fetch(&original_url, "user1")
            .await
            .expect("first fetch must succeed");
        assert_eq!(redirect_mock.hits(), 1, "redirect should be followed once");
        assert_eq!(landing_mock.hits(), 1, "landing page should be fetched once");
        assert!(
            page1.content.contains("Redirected content"),
            "content from redirected page: {}",
            page1.content
        );

        // Second fetch of the *same original URL* — must hit the cache, NOT the server
        let page2 = client
            .fetch(&original_url, "user1")
            .await
            .expect("second fetch must succeed");
        assert_eq!(
            redirect_mock.hits(),
            1,
            "redirect endpoint must not be hit again (cache hit on original URL)"
        );
        assert_eq!(
            landing_mock.hits(),
            1,
            "landing page must not be re-fetched (cache hit)"
        );
        assert_eq!(
            page1.content, page2.content,
            "cached content must match original"
        );
    }

    // ── test_non_html_content_type_skips_extraction ────────────────────────────

    #[tokio::test]
    async fn test_non_html_content_type_skips_extraction() {
        let server = MockServer::start();
        // Simulate a PDF response (application/pdf)
        server.mock(|when, then| {
            when.method(GET).path("/doc.pdf");
            then.status(200)
                .header("Content-Type", "application/pdf")
                .body("%PDF-1.4 binary garbage \x00\x01\x02");
        });

        let client = client_for_mock(&server);
        let url = format!("http://127.0.0.1:{}/doc.pdf", server.port());
        let page = client
            .fetch(&url, "user1")
            .await
            .expect("fetch must succeed for non-HTML content");

        // Title must be empty (no HTML extraction attempted)
        assert!(
            page.title.is_empty(),
            "title must be empty for non-HTML response: '{}'",
            page.title
        );
        // Content must reflect the content-type, not garbled binary text
        assert!(
            page.content.contains("application/pdf"),
            "content must indicate non-HTML type: '{}'",
            page.content
        );
        // Binary garbage must not appear as plain text
        assert!(
            !page.content.contains("binary garbage"),
            "binary body must not be extracted as text: '{}'",
            page.content
        );
    }

    // ── test_web_browse_tool_permission_gated ─────────────────────────────────
    //
    // Verify that the web_browse ToolDefinition carries ReadOnly permission so
    // it is accepted by the ToolAllowlist when allowed with ReadOnly.

    #[test]
    fn test_web_browse_tool_is_read_only_gated() {
        use crate::tool_types::ToolAllowlist;

        let tool = web_browse_tool();
        assert_eq!(
            tool.permission,
            ToolPermission::ReadOnly,
            "web_browse must be ReadOnly so it can be registered in the read-only tool gate"
        );

        // Verify it passes through ToolAllowlist when allowed
        let mut allowlist = ToolAllowlist::new();
        allowlist.allow_tool(tool.name.clone(), ToolPermission::ReadOnly);
        assert!(
            allowlist.is_allowed(TOOL_NAME, &ToolPermission::ReadOnly),
            "web_browse must be allowed after registration"
        );
    }
}
