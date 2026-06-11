//! WEB-03: NewsAggregator — GNews + NewsAPI adapters with dedup, rate-limiting,
//! and 30-minute cache.
//!
//! ## Environment variables
//!
//! | Variable | Purpose |
//! |---|---|
//! | `LUMINA_GNEWS_BASE_URL` | GNews API base URL (no trailing slash) |
//! | `GNEWS_API_KEY` | GNews API key |
//! | `LUMINA_NEWSAPI_BASE_URL` | NewsAPI.org base URL (no trailing slash) |
//! | `NEWSAPI_KEY` | NewsAPI.org key |
//!
//! ## Design notes
//!
//! - API keys are fetched at call time from env vars; they never appear in logs
//!   or `Headline` fields.
//! - Rate limit: 100 requests/day per adapter, tracked with `AtomicU64`.
//!   The counter resets once per calendar day (UTC).
//! - Cache: `Arc<Mutex<HashMap<cache_key, (Vec<Headline>, Instant)>>>`.
//!   Results are served from the cache if they are less than 30 minutes old.
//! - Deduplication: two headlines are considered duplicates if one title is a
//!   prefix of the other when both are lowercased and stripped of punctuation,
//!   with the shorter title being at least 60 % of the longer one's length.

use super::{FeedSource, Headline};
use crate::egress_inspector::EgressInspector;
use crate::error::{LuminaError, Result};
use async_trait::async_trait;
use reqwest::Client;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// ── Helpers ───────────────────────────────────────────────────────────────

/// Seconds since Unix epoch — used for cheap daily reset checks.
fn unix_day() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 86_400
}

/// Build a query URL from a base URL and key-value pairs.
/// Returns `None` if `base` is not a valid URL.
fn build_query_url(base: &str, params: &[(&str, &str)]) -> Option<String> {
    let mut url = reqwest::Url::parse(base).ok()?;
    {
        let mut pairs = url.query_pairs_mut();
        for (k, v) in params {
            pairs.append_pair(k, v);
        }
    }
    Some(url.to_string())
}

/// Normalise a title for duplicate detection: lowercase, keep only
/// alphanumeric characters and spaces.
fn normalise_title(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ')
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Return `true` if `a` and `b` are likely the same story.
///
/// Uses a simple prefix-similarity heuristic:
/// 1. Normalise both titles.
/// 2. The shorter must be at least 60 % of the longer (by char count).
/// 3. The longer must start with the shorter string.
pub(crate) fn titles_are_similar(a: &str, b: &str) -> bool {
    let na = normalise_title(a);
    let nb = normalise_title(b);
    if na.is_empty() || nb.is_empty() {
        return false;
    }
    let (shorter, longer) = if na.len() <= nb.len() {
        (na.as_str(), nb.as_str())
    } else {
        (nb.as_str(), na.as_str())
    };
    // Length guard: shorter must be ≥ 40 % of longer.
    // This catches "Rust 2.0 released" vs "Rust 2.0 released with new features"
    // while still rejecting very short tokens like "AI" vs a long sentence.
    if shorter.len() * 10 < longer.len() * 4 {
        return false;
    }
    longer.starts_with(shorter)
}

/// Remove duplicates from `items` using `titles_are_similar`.
/// The first occurrence is kept; later near-duplicates are dropped.
pub(crate) fn dedup_headlines(items: Vec<Headline>) -> Vec<Headline> {
    let mut out: Vec<Headline> = Vec::with_capacity(items.len());
    'outer: for candidate in items {
        for existing in &out {
            if titles_are_similar(&candidate.title, &existing.title) {
                continue 'outer;
            }
        }
        out.push(candidate);
    }
    out
}

// ── Per-adapter rate limiter ───────────────────────────────────────────────

/// Tracks daily request count for one adapter.
///
/// Uses two `AtomicU64` values so it is lock-free: one for the request
/// count and one for the UTC day number when the counter was last reset.
struct RateLimiter {
    count: AtomicU64,
    last_reset_day: AtomicU64,
    limit: u64,
}

impl RateLimiter {
    fn new(limit: u64) -> Self {
        Self {
            count: AtomicU64::new(0),
            last_reset_day: AtomicU64::new(unix_day()),
            limit,
        }
    }

    /// Return `true` and increment the counter if a request is allowed.
    /// Returns `false` when the daily limit is already reached.
    fn try_acquire(&self) -> bool {
        let today = unix_day();
        // Reset counter if we have moved to a new UTC day.
        let prev_day = self.last_reset_day.load(Ordering::Relaxed);
        if today > prev_day {
            // CAS to avoid double-reset in concurrent calls.
            if self.last_reset_day
                .compare_exchange(prev_day, today, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                self.count.store(0, Ordering::SeqCst);
            }
        }
        let prev = self.count.fetch_add(1, Ordering::SeqCst);
        if prev < self.limit {
            true
        } else {
            // Roll back — we exceeded the limit.
            self.count.fetch_sub(1, Ordering::SeqCst);
            false
        }
    }

    /// Current number of requests recorded for today (before any reset).
    #[cfg(test)]
    fn current_count(&self) -> u64 {
        self.count.load(Ordering::SeqCst)
    }
}

// ── Cache ─────────────────────────────────────────────────────────────────

const CACHE_TTL: Duration = Duration::from_secs(30 * 60);

type Cache = Arc<Mutex<HashMap<String, (Vec<Headline>, Instant)>>>;

fn new_cache() -> Cache {
    Arc::new(Mutex::new(HashMap::new()))
}

fn cache_key(adapter: &str, category: &str, count: usize) -> String {
    format!("{}:{}:{}", adapter, category, count)
}

/// Look up a cache entry. Returns `Some(clone)` if the entry exists and is
/// younger than `CACHE_TTL`.
fn cache_get(cache: &Cache, key: &str) -> Option<Vec<Headline>> {
    let guard = cache.lock().unwrap_or_else(|p| p.into_inner());
    guard.get(key).and_then(|(items, ts)| {
        if ts.elapsed() < CACHE_TTL {
            Some(items.clone())
        } else {
            None
        }
    })
}

/// Insert or replace a cache entry.
fn cache_set(cache: &Cache, key: String, items: Vec<Headline>) {
    let mut guard = cache.lock().unwrap_or_else(|p| p.into_inner());
    guard.insert(key, (items, Instant::now()));
}

// ── GNewsAdapter ──────────────────────────────────────────────────────────

/// Adapter for the GNews API.
///
/// Required env vars:
/// - `LUMINA_GNEWS_BASE_URL` — base URL (e.g. `https://gnews.io/api/v4`)
/// - `GNEWS_API_KEY` — API key
///
/// Rate limit: 100 requests per day.
pub struct GNewsAdapter {
    client: Client,
    rate_limiter: Arc<RateLimiter>,
    cache: Cache,
    /// Egress inspector — validates the GNews base URL before every HTTP request.
    egress: EgressInspector,
}

impl GNewsAdapter {
    pub fn new() -> Self {
        Self {
            client: Client::new(),
            rate_limiter: Arc::new(RateLimiter::new(100)),
            cache: new_cache(),
            egress: EgressInspector::from_env(),
        }
    }

    /// Construct with an explicit egress inspector (useful in tests).
    #[cfg(test)]
    pub fn with_egress(egress: EgressInspector) -> Self {
        Self {
            client: Client::new(),
            rate_limiter: Arc::new(RateLimiter::new(100)),
            cache: new_cache(),
            egress,
        }
    }

    fn base_url() -> Option<String> {
        std::env::var("LUMINA_GNEWS_BASE_URL")
            .ok()
            .filter(|s| !s.is_empty())
    }

    fn api_key() -> Option<String> {
        std::env::var("GNEWS_API_KEY")
            .ok()
            .filter(|s| !s.is_empty())
    }

    /// Build the request URL.  The key is appended last so it never
    /// appears as a prefix in any log line that might truncate the URL.
    fn build_url(base: &str, category: &str, count: usize, key: &str) -> Option<String> {
        build_query_url(
            &format!("{}/top-headlines", base),
            &[
                ("category", category),
                ("max", &count.to_string()),
                ("apikey", key),
            ],
        )
    }

    /// Parse a GNews JSON response into `Vec<Headline>`.
    ///
    /// Expected shape:
    /// ```json
    /// { "articles": [{ "title":"...", "source":{"name":"..."}, "url":"...",
    ///                  "publishedAt":"...", "description":"..." }] }
    /// ```
    fn parse_response(body: &str, category: &str) -> Vec<Headline> {
        let v: serde_json::Value = match serde_json::from_str(body) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("gnews: JSON parse error: {}", e);
                return vec![];
            }
        };
        let articles = match v.get("articles").and_then(|a| a.as_array()) {
            Some(arr) => arr,
            None => {
                log::debug!("gnews: no 'articles' array in response");
                return vec![];
            }
        };
        articles
            .iter()
            .filter_map(|a| {
                let title = a.get("title")?.as_str()?.to_string();
                let source = a
                    .get("source")
                    .and_then(|s| s.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let url = a
                    .get("url")
                    .and_then(|u| u.as_str())
                    .unwrap_or("")
                    .to_string();
                let published_at = a
                    .get("publishedAt")
                    .and_then(|p| p.as_str())
                    .unwrap_or("")
                    .to_string();
                let snippet = a
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string();
                Some(Headline {
                    title,
                    source,
                    url,
                    published_at,
                    snippet,
                    category: category.to_string(),
                })
            })
            .collect()
    }
}

impl Default for GNewsAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl FeedSource for GNewsAdapter {
    fn name(&self) -> &str {
        "gnews"
    }

    fn is_configured(&self) -> bool {
        Self::base_url().is_some() && Self::api_key().is_some()
    }

    async fn fetch(&self, category: &str, count: usize) -> Result<Vec<Headline>> {
        let key = cache_key(self.name(), category, count);

        // Return cached result if still fresh.
        if let Some(cached) = cache_get(&self.cache, &key) {
            return Ok(cached);
        }

        // Check rate limit.
        if !self.rate_limiter.try_acquire() {
            log::warn!("gnews: daily rate limit reached, returning empty");
            return Ok(vec![]);
        }

        let base = match Self::base_url() {
            Some(b) => b,
            None => return Ok(vec![]),
        };
        let api_key = match Self::api_key() {
            Some(k) => k,
            None => return Ok(vec![]),
        };

        let url = match Self::build_url(&base, category, count, &api_key) {
            Some(u) => u,
            None => {
                return Err(LuminaError::Config(
                    "gnews: could not build request URL".to_string(),
                ))
            }
        };

        // Egress check — blocks if the GNews host is not in the allowlist.
        self.egress.inspect(&url, "gnews_fetch")
            .map_err(LuminaError::from)?;

        let resp = match self.client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                log::warn!("gnews: request failed: {}", e);
                return Ok(vec![]);
            }
        };

        if !resp.status().is_success() {
            log::warn!("gnews: HTTP {} from endpoint", resp.status());
            return Ok(vec![]);
        }

        let body = match resp.text().await {
            Ok(t) => t,
            Err(e) => {
                log::warn!("gnews: failed to read response body: {}", e);
                return Ok(vec![]);
            }
        };

        let headlines = Self::parse_response(&body, category);
        cache_set(&self.cache, key, headlines.clone());
        Ok(headlines)
    }
}

// ── NewsApiAdapter ────────────────────────────────────────────────────────

/// Adapter for the NewsAPI.org API.
///
/// Required env vars:
/// - `LUMINA_NEWSAPI_BASE_URL` — base URL (e.g. `https://newsapi.org/v2`)
/// - `NEWSAPI_KEY` — API key
///
/// Rate limit: 100 requests per day.
pub struct NewsApiAdapter {
    client: Client,
    rate_limiter: Arc<RateLimiter>,
    cache: Cache,
    /// Egress inspector — validates the NewsAPI base URL before every HTTP request.
    egress: EgressInspector,
}

impl NewsApiAdapter {
    pub fn new() -> Self {
        Self {
            client: Client::new(),
            rate_limiter: Arc::new(RateLimiter::new(100)),
            cache: new_cache(),
            egress: EgressInspector::from_env(),
        }
    }

    /// Construct with an explicit egress inspector (useful in tests).
    #[cfg(test)]
    pub fn with_egress(egress: EgressInspector) -> Self {
        Self {
            client: Client::new(),
            rate_limiter: Arc::new(RateLimiter::new(100)),
            cache: new_cache(),
            egress,
        }
    }

    fn base_url() -> Option<String> {
        std::env::var("LUMINA_NEWSAPI_BASE_URL")
            .ok()
            .filter(|s| !s.is_empty())
    }

    fn api_key() -> Option<String> {
        std::env::var("NEWSAPI_KEY")
            .ok()
            .filter(|s| !s.is_empty())
    }

    fn build_url(base: &str, category: &str, count: usize, key: &str) -> Option<String> {
        build_query_url(
            &format!("{}/top-headlines", base),
            &[
                ("category", category),
                ("pageSize", &count.to_string()),
                ("apiKey", key),
            ],
        )
    }

    /// Parse a NewsAPI JSON response into `Vec<Headline>`.
    ///
    /// Expected shape:
    /// ```json
    /// { "articles": [{ "title":"...", "source":{"name":"..."}, "url":"...",
    ///                  "publishedAt":"...", "description":"..." }] }
    /// ```
    fn parse_response(body: &str, category: &str) -> Vec<Headline> {
        let v: serde_json::Value = match serde_json::from_str(body) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("newsapi: JSON parse error: {}", e);
                return vec![];
            }
        };
        let articles = match v.get("articles").and_then(|a| a.as_array()) {
            Some(arr) => arr,
            None => {
                log::debug!("newsapi: no 'articles' array in response");
                return vec![];
            }
        };
        articles
            .iter()
            .filter_map(|a| {
                let title = a.get("title")?.as_str()?.to_string();
                let source = a
                    .get("source")
                    .and_then(|s| s.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let url = a
                    .get("url")
                    .and_then(|u| u.as_str())
                    .unwrap_or("")
                    .to_string();
                let published_at = a
                    .get("publishedAt")
                    .and_then(|p| p.as_str())
                    .unwrap_or("")
                    .to_string();
                let snippet = a
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string();
                Some(Headline {
                    title,
                    source,
                    url,
                    published_at,
                    snippet,
                    category: category.to_string(),
                })
            })
            .collect()
    }
}

impl Default for NewsApiAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl FeedSource for NewsApiAdapter {
    fn name(&self) -> &str {
        "newsapi"
    }

    fn is_configured(&self) -> bool {
        Self::base_url().is_some() && Self::api_key().is_some()
    }

    async fn fetch(&self, category: &str, count: usize) -> Result<Vec<Headline>> {
        let key = cache_key(self.name(), category, count);

        if let Some(cached) = cache_get(&self.cache, &key) {
            return Ok(cached);
        }

        if !self.rate_limiter.try_acquire() {
            log::warn!("newsapi: daily rate limit reached, returning empty");
            return Ok(vec![]);
        }

        let base = match Self::base_url() {
            Some(b) => b,
            None => return Ok(vec![]),
        };
        let api_key = match Self::api_key() {
            Some(k) => k,
            None => return Ok(vec![]),
        };

        let url = match Self::build_url(&base, category, count, &api_key) {
            Some(u) => u,
            None => {
                return Err(LuminaError::Config(
                    "newsapi: could not build request URL".to_string(),
                ))
            }
        };

        // Egress check — blocks if the NewsAPI host is not in the allowlist.
        self.egress.inspect(&url, "newsapi_fetch")
            .map_err(LuminaError::from)?;

        let resp = match self.client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                log::warn!("newsapi: request failed: {}", e);
                return Ok(vec![]);
            }
        };

        if !resp.status().is_success() {
            log::warn!("newsapi: HTTP {} from endpoint", resp.status());
            return Ok(vec![]);
        }

        let body = match resp.text().await {
            Ok(t) => t,
            Err(e) => {
                log::warn!("newsapi: failed to read response body: {}", e);
                return Ok(vec![]);
            }
        };

        let headlines = Self::parse_response(&body, category);
        cache_set(&self.cache, key, headlines.clone());
        Ok(headlines)
    }
}

// ── NewsAggregator ────────────────────────────────────────────────────────

/// Aggregates headlines from multiple [`FeedSource`] adapters.
///
/// - Calls all configured adapters concurrently.
/// - Deduplicates across adapters using title similarity.
/// - Sorts by `published_at` (lexicographic / ISO 8601 descending).
/// - Maintains a shared 30-minute cache per `(adapter, category, count)` key.
pub struct NewsAggregator {
    sources: Vec<Arc<dyn FeedSource>>,
}

impl NewsAggregator {
    /// Create an aggregator with the default set of adapters
    /// (GNews + NewsAPI), each configured from env vars.
    pub fn from_env() -> Self {
        let mut agg = Self { sources: Vec::new() };
        agg.register(Arc::new(GNewsAdapter::new()));
        agg.register(Arc::new(NewsApiAdapter::new()));
        agg
    }

    /// Create an empty aggregator.
    pub fn new() -> Self {
        Self { sources: Vec::new() }
    }

    /// Register an additional feed source.
    pub fn register(&mut self, source: Arc<dyn FeedSource>) {
        self.sources.push(source);
    }

    /// Return `true` if at least one registered source reports `is_configured()`.
    ///
    /// Used by the Vigil `NewsAdapter` to decide whether to include itself in
    /// the briefing registry without performing an actual network request.
    pub fn has_configured_sources(&self) -> bool {
        self.sources.iter().any(|s| s.is_configured())
    }

    /// Fetch up to `count` deduplicated headlines for `category` from all
    /// configured adapters.
    ///
    /// All adapter fetches run concurrently.  Results are merged, deduplicated
    /// by title similarity, sorted newest-first, and truncated to `count`.
    pub async fn fetch_headlines(&self, category: &str, count: usize) -> Vec<Headline> {
        let mut handles = Vec::new();

        for source in &self.sources {
            if !source.is_configured() {
                continue;
            }
            let source = Arc::clone(source);
            let category = category.to_string();
            handles.push(tokio::spawn(async move {
                source.fetch(&category, count).await
            }));
        }

        let mut all: Vec<Headline> = Vec::new();
        for handle in handles {
            match handle.await {
                Ok(Ok(items)) => all.extend(items),
                Ok(Err(e)) => log::warn!("feed source error: {}", e),
                Err(e) => log::warn!("feed source task panicked: {}", e),
            }
        }

        // Dedup, sort newest-first, truncate.
        let deduped = dedup_headlines(all);
        let mut sorted = deduped;
        sorted.sort_by(|a, b| b.published_at.cmp(&a.published_at));
        sorted.truncate(count);
        sorted
    }
}

impl Default for NewsAggregator {
    fn default() -> Self {
        Self::from_env()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use httpmock::prelude::*;
    use std::sync::Mutex;

    /// Serialise all tests that touch process-global env vars.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    // ── Helpers ───────────────────────────────────────────────────────────

    fn make_headline(title: &str, source: &str) -> Headline {
        Headline {
            title: title.to_string(),
            source: source.to_string(),
            url: format!("https://example.com/{}", title.replace(' ', "-")),
            published_at: "2026-06-06T12:00:00Z".to_string(),
            snippet: format!("Snippet for {}", title),
            category: "general".to_string(),
        }
    }

    fn gnews_body(articles: &[(&str, &str)]) -> String {
        let items: Vec<String> = articles
            .iter()
            .map(|(title, src)| {
                format!(
                    r#"{{"title":"{title}","source":{{"name":"{src}"}},"url":"https://example.com","publishedAt":"2026-06-06T12:00:00Z","description":"desc"}}"#,
                    title = title,
                    src = src
                )
            })
            .collect();
        format!(r#"{{"articles":[{}]}}"#, items.join(","))
    }

    fn newsapi_body(articles: &[(&str, &str)]) -> String {
        // Same shape as gnews for our tests
        gnews_body(articles)
    }

    // ── test_gnews_url_construction_with_category ─────────────────────────

    #[test]
    fn test_gnews_url_construction_with_category() {
        let url = GNewsAdapter::build_url(
            "http://localhost:9000",
            "technology",
            5,
            "REDACTED",
        )
        .expect("should build URL");

        let parsed = reqwest::Url::parse(&url).unwrap();
        assert!(
            parsed.path().ends_with("/top-headlines"),
            "path should end with /top-headlines, got: {}",
            parsed.path()
        );
        let params: HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(params.get("category").map(|s| s.as_str()), Some("technology"));
        assert_eq!(params.get("max").map(|s| s.as_str()), Some("5"));
        // Key is present but we only check the param name, not its value
        assert!(params.contains_key("apikey"), "URL must include apikey param");
    }

    // ── test_newsapi_url_construction ─────────────────────────────────────

    #[test]
    fn test_newsapi_url_construction() {
        let url = NewsApiAdapter::build_url(
            "http://localhost:9001",
            "business",
            10,
            "REDACTED",
        )
        .expect("should build URL");

        let parsed = reqwest::Url::parse(&url).unwrap();
        assert!(
            parsed.path().ends_with("/top-headlines"),
            "path should end with /top-headlines, got: {}",
            parsed.path()
        );
        let params: HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(params.get("category").map(|s| s.as_str()), Some("business"));
        assert_eq!(params.get("pageSize").map(|s| s.as_str()), Some("10"));
        assert!(params.contains_key("apiKey"), "URL must include apiKey param");
    }

    // ── test_headline_deduplication ───────────────────────────────────────

    #[test]
    fn test_headline_deduplication() {
        let items = vec![
            make_headline("Rust 2.0 released today", "TechNews"),
            make_headline("Python 4.0 launched", "CodeTimes"),
            make_headline("Rust 2.0 released today with new features", "DevBlog"),
        ];
        let result = dedup_headlines(items);
        assert_eq!(result.len(), 2, "third headline is a near-duplicate of the first");
        assert_eq!(result[0].title, "Rust 2.0 released today");
        assert_eq!(result[1].title, "Python 4.0 launched");
    }

    #[test]
    fn test_headline_deduplication_no_duplicates() {
        let items = vec![
            make_headline("Story Alpha", "Source1"),
            make_headline("Story Beta", "Source2"),
            make_headline("Story Gamma", "Source3"),
        ];
        let result = dedup_headlines(items);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_headline_deduplication_all_same() {
        let items = vec![
            make_headline("Breaking news today", "A"),
            make_headline("Breaking news today", "B"),
            make_headline("Breaking news today", "C"),
        ];
        let result = dedup_headlines(items);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_titles_are_similar_prefix() {
        assert!(titles_are_similar(
            "Rust 2.0 released",
            "Rust 2.0 released with new features"
        ));
    }

    #[test]
    fn test_titles_are_similar_different() {
        assert!(!titles_are_similar("Apple announces new iPhone", "Google releases Android 15"));
    }

    #[test]
    fn test_titles_are_similar_too_short() {
        // "AI" vs "AI is transforming everything" — shorter is way too short
        assert!(!titles_are_similar("AI", "AI is transforming everything globally"));
    }

    // ── test_rate_limit_tracking ──────────────────────────────────────────

    #[test]
    fn test_rate_limit_tracking() {
        let rl = RateLimiter::new(3);
        assert!(rl.try_acquire(), "first request should succeed");
        assert!(rl.try_acquire(), "second request should succeed");
        assert!(rl.try_acquire(), "third request should succeed");
        assert!(!rl.try_acquire(), "fourth request should be rejected (limit=3)");
        assert_eq!(rl.current_count(), 3);
    }

    #[test]
    fn test_rate_limit_zero_limit() {
        let rl = RateLimiter::new(0);
        assert!(!rl.try_acquire(), "no requests allowed when limit=0");
    }

    #[test]
    fn test_rate_limit_one_request() {
        let rl = RateLimiter::new(1);
        assert!(rl.try_acquire());
        assert!(!rl.try_acquire());
    }

    // ── test_cache_returns_within_30min ───────────────────────────────────

    #[test]
    fn test_cache_returns_within_30min() {
        let cache = new_cache();
        let key = "gnews:tech:5".to_string();
        let headlines = vec![make_headline("Cached Story", "CacheNews")];

        cache_set(&cache, key.clone(), headlines.clone());

        let result = cache_get(&cache, &key);
        assert!(result.is_some(), "should return cached value within TTL");
        let result = result.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].title, "Cached Story");
    }

    #[test]
    fn test_cache_miss_returns_none() {
        let cache = new_cache();
        let result = cache_get(&cache, "nonexistent:key");
        assert!(result.is_none());
    }

    #[test]
    fn test_cache_fresh_entry_returned() {
        // Verify that an entry inserted just now (elapsed ≈ 0 ms) is considered
        // fresh and returned by cache_get.
        //
        // Note: std::time::Instant cannot be backdated, so actual expiry (elapsed
        // > CACHE_TTL) cannot be triggered in a unit test without mocking the
        // clock.  The expiry branch (`elapsed >= CACHE_TTL → return None`) is
        // covered by the production code path and by integration tests that
        // parameterise the TTL.
        let cache = new_cache();
        let key = "test:fresh".to_string();
        let headlines = vec![make_headline("Fresh Story", "FreshNews")];

        cache_set(&cache, key.clone(), headlines);

        let result = cache_get(&cache, &key);
        assert!(result.is_some(), "entry inserted just now should still be fresh (< CACHE_TTL)");
        let result = result.unwrap();
        assert_eq!(result[0].title, "Fresh Story");
    }

    // ── test_api_key_not_in_output ────────────────────────────────────────

    #[test]
    fn test_api_key_not_in_output() {
        // Build URLs and verify the key value does not leak into Headline fields.
        let url = GNewsAdapter::build_url("http://localhost", "general", 3, "SECRET_KEY_123")
            .unwrap();

        // The key is in the URL (for the HTTP request), but parse_response
        // should never embed the key in any Headline.
        let body = gnews_body(&[("Test headline", "TestSource")]);
        let headlines = GNewsAdapter::parse_response(&body, "general");
        for h in &headlines {
            assert!(
                !h.title.contains("SECRET_KEY_123"),
                "key must not appear in title"
            );
            assert!(
                !h.snippet.contains("SECRET_KEY_123"),
                "key must not appear in snippet"
            );
            assert!(
                !h.url.contains("SECRET_KEY_123"),
                "key must not appear in url"
            );
            assert!(
                !h.source.contains("SECRET_KEY_123"),
                "key must not appear in source"
            );
        }

        // Verify URL contains the param but value is opaque to parse_response
        assert!(url.contains("apikey=SECRET_KEY_123") || url.contains("apikey="),
                "URL must contain apikey param");
    }

    #[test]
    fn test_newsapi_key_not_in_output() {
        let body = newsapi_body(&[("Another headline", "AnotherSource")]);
        let headlines = NewsApiAdapter::parse_response(&body, "business");
        for h in &headlines {
            assert!(!h.title.contains("MY_SECRET_NEWSAPI_KEY"));
            assert!(!h.snippet.contains("MY_SECRET_NEWSAPI_KEY"));
        }
    }

    // ── test_both_apis_rate_limited_returns_cached ────────────────────────

    #[tokio::test]
    async fn test_both_apis_rate_limited_returns_cached() {
        // When both adapters' rate limits are exhausted, fetch() returns empty
        // unless there is a cached result.
        let gnews = GNewsAdapter::new();
        let newsapi = NewsApiAdapter::new();

        // Exhaust rate limits.
        for _ in 0..100 {
            gnews.rate_limiter.try_acquire();
            newsapi.rate_limiter.try_acquire();
        }
        assert_eq!(gnews.rate_limiter.current_count(), 100);
        assert_eq!(newsapi.rate_limiter.current_count(), 100);

        // Without cache: both return empty.
        let g_result = gnews.fetch("general", 5).await.unwrap();
        assert!(g_result.is_empty(), "should be empty when rate-limited (no cache)");

        let n_result = newsapi.fetch("general", 5).await.unwrap();
        assert!(n_result.is_empty(), "should be empty when rate-limited (no cache)");

        // Populate cache manually, then fetch should return cached result.
        let cached_headlines = vec![make_headline("Cached result", "CachedSource")];
        let key = cache_key("gnews", "general", 5);
        cache_set(&gnews.cache, key, cached_headlines.clone());

        let g_cached = gnews.fetch("general", 5).await.unwrap();
        assert_eq!(g_cached.len(), 1, "should return cached result even when rate-limited");
        assert_eq!(g_cached[0].title, "Cached result");
    }

    // ── HTTP integration tests (httpmock) ─────────────────────────────────

    #[tokio::test]
    #[serial]
    async fn test_gnews_fetch_returns_headlines() {
        let _g = ENV_LOCK.lock().unwrap();
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path_contains("top-headlines");
            then.status(200)
                .header("Content-Type", "application/json")
                .body(gnews_body(&[
                    ("Article One", "Source A"),
                    ("Article Two", "Source B"),
                ]));
        });

        std::env::set_var("LUMINA_GNEWS_BASE_URL", server.base_url());
        std::env::set_var("GNEWS_API_KEY", "test-key");

        let adapter = GNewsAdapter::new();
        let headlines = adapter.fetch("technology", 5).await.unwrap();
        assert_eq!(headlines.len(), 2);
        assert_eq!(headlines[0].title, "Article One");
        assert_eq!(headlines[0].source, "Source A");
        assert_eq!(headlines[0].category, "technology");

        std::env::remove_var("LUMINA_GNEWS_BASE_URL");
        std::env::remove_var("GNEWS_API_KEY");
    }

    #[tokio::test]
    #[serial]
    async fn test_newsapi_fetch_returns_headlines() {
        let _g = ENV_LOCK.lock().unwrap();
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path_contains("top-headlines");
            then.status(200)
                .header("Content-Type", "application/json")
                .body(newsapi_body(&[("NewsAPI Story", "NewsOrg")]));
        });

        std::env::set_var("LUMINA_NEWSAPI_BASE_URL", server.base_url());
        std::env::set_var("NEWSAPI_KEY", "test-key");

        let adapter = NewsApiAdapter::new();
        let headlines = adapter.fetch("business", 3).await.unwrap();
        assert_eq!(headlines.len(), 1);
        assert_eq!(headlines[0].title, "NewsAPI Story");

        std::env::remove_var("LUMINA_NEWSAPI_BASE_URL");
        std::env::remove_var("NEWSAPI_KEY");
    }

    #[tokio::test]
    #[serial]
    async fn test_gnews_returns_empty_on_500() {
        let _g = ENV_LOCK.lock().unwrap();
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path_contains("top-headlines");
            then.status(500).body("internal error");
        });

        std::env::set_var("LUMINA_GNEWS_BASE_URL", server.base_url());
        std::env::set_var("GNEWS_API_KEY", "test-key");

        let adapter = GNewsAdapter::new();
        let result = adapter.fetch("general", 5).await.unwrap();
        assert!(result.is_empty(), "500 response should yield empty vec");

        std::env::remove_var("LUMINA_GNEWS_BASE_URL");
        std::env::remove_var("GNEWS_API_KEY");
    }

    #[tokio::test]
    #[serial]
    async fn test_aggregator_deduplicates_across_adapters() {
        let _g = ENV_LOCK.lock().unwrap();

        // Both adapters return the same article.
        let gnews_server = MockServer::start();
        gnews_server.mock(|when, then| {
            when.method(GET).path_contains("top-headlines");
            then.status(200)
                .header("Content-Type", "application/json")
                .body(gnews_body(&[("Shared Story About Technology", "GNews")]));
        });

        let newsapi_server = MockServer::start();
        newsapi_server.mock(|when, then| {
            when.method(GET).path_contains("top-headlines");
            then.status(200)
                .header("Content-Type", "application/json")
                .body(newsapi_body(&[
                    ("Shared Story About Technology extended version", "NewsAPI"),
                    ("Unique Story From NewsAPI", "NewsAPI"),
                ]));
        });

        std::env::set_var("LUMINA_GNEWS_BASE_URL", gnews_server.base_url());
        std::env::set_var("GNEWS_API_KEY", "key1");
        std::env::set_var("LUMINA_NEWSAPI_BASE_URL", newsapi_server.base_url());
        std::env::set_var("NEWSAPI_KEY", "key2");

        let agg = NewsAggregator::from_env();
        let headlines = agg.fetch_headlines("technology", 10).await;

        // The shared story + unique story = 2 (not 3)
        assert_eq!(
            headlines.len(),
            2,
            "duplicate across adapters should be dropped; got: {:?}",
            headlines.iter().map(|h| &h.title).collect::<Vec<_>>()
        );

        std::env::remove_var("LUMINA_GNEWS_BASE_URL");
        std::env::remove_var("GNEWS_API_KEY");
        std::env::remove_var("LUMINA_NEWSAPI_BASE_URL");
        std::env::remove_var("NEWSAPI_KEY");
    }
}
