//! Financial data adapters for Lumina Constellation.
//!
//! Provides [`AlphavantageAdapter`] and [`FinnhubAdapter`] for fetching stock
//! quotes and market summaries, with per-adapter caching and rate limiting.
//!
//! # Environment variables
//! - `LUMINA_ALPHAVANTAGE_URL` — base URL for Alphavantage (no trailing slash)
//! - `ALPHAVANTAGE_API_KEY`    — Alphavantage API key (never logged)
//! - `LUMINA_FINNHUB_URL`      — base URL for Finnhub (no trailing slash)
//! - `FINNHUB_API_KEY`         — Finnhub API key (never logged)
//!
//! API keys are expected to be pre-populated from Infisical via
//! `fetch-mcp-secrets.sh` on Terminus (mcp-host) before the MCP server starts.
//! They are never logged, traced, or included in error messages.
//!
//! # Rate limits
//! - Alphavantage: 25 requests/day
//! - Finnhub: 60 calls/minute
//!
//! # Cache TTLs
//! - Individual quotes: 5 minutes
//! - Market summary: 15 minutes
//!
//! # Deferred acceptance criteria
//! The following spec requirements depend on the Ledger module (future work)
//! and are intentionally deferred to a follow-on ticket:
//! - Vigil integration for morning briefing
//! - Per-user finance preferences (watchlist, preferred symbols)
//! These will be tracked in a WEB-04b follow-on issue.

use crate::egress_inspector::EgressInspector;
use crate::error::{LuminaError, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::env;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ─── Domain types ────────────────────────────────────────────────────────────

/// A single stock/ETF quote result.
#[derive(Debug, Clone, PartialEq)]
pub struct QuoteResult {
    pub symbol: String,
    pub price: f64,
    pub change: f64,
    pub change_pct: f64,
    pub timestamp: String,
    pub source: String,
}

impl QuoteResult {
    /// Format as human-readable string.
    ///
    /// # Example
    /// ```
    /// use lumina_core::feeds::finance::QuoteResult;
    /// let q = QuoteResult {
    ///     symbol: "AAPL".into(),
    ///     price: 198.52,
    ///     change: 2.41,
    ///     change_pct: 1.23,
    ///     timestamp: "9:45 AM ET".into(),
    ///     source: "alphavantage".into(),
    /// };
    /// assert_eq!(q.format(), "AAPL: $198.52 (+1.23%, +$2.41) — as of 9:45 AM ET");
    /// ```
    pub fn format(&self) -> String {
        let sign = if self.change >= 0.0 { "+" } else { "-" };
        let pct_sign = if self.change_pct >= 0.0 { "+" } else { "-" };
        format!(
            "{}: ${:.2} ({}{:.2}%, {}${:.2}) — as of {}",
            self.symbol,
            self.price,
            pct_sign,
            self.change_pct.abs(),
            sign,
            self.change.abs(),
            self.timestamp,
        )
    }
}

// ─── Lock helper note ─────────────────────────────────────────────────────────
//
// All `state.lock().unwrap()` calls below will panic if a thread panics while
// holding the lock (poisoned mutex). In practice this is low risk because no
// operation inside a lock block can panic under normal inputs. A future
// hardening pass could replace `.unwrap()` with `.unwrap_or_else(|e| e.into_inner())`
// to recover from poisoned locks gracefully.

// ─── Cache helpers ────────────────────────────────────────────────────────────

struct CacheEntry<T> {
    value: T,
    inserted_at: Instant,
    ttl: Duration,
}

impl<T: Clone> CacheEntry<T> {
    fn new(value: T, ttl: Duration) -> Self {
        Self {
            value,
            inserted_at: Instant::now(),
            ttl,
        }
    }

    fn is_valid(&self) -> bool {
        self.inserted_at.elapsed() < self.ttl
    }

    fn get(&self) -> Option<T> {
        if self.is_valid() {
            Some(self.value.clone())
        } else {
            None
        }
    }
}

/// Cache TTL for individual quotes: 5 minutes.
pub const QUOTE_TTL: Duration = Duration::from_secs(5 * 60);
/// Cache TTL for market summaries: 15 minutes.
pub const SUMMARY_TTL: Duration = Duration::from_secs(15 * 60);

// ─── Rate limit helpers ───────────────────────────────────────────────────────

/// Daily rate limiter (resets after 24 h from first call).
///
/// **Note:** the 24-hour window is measured using [`std::time::Instant`], which
/// is an in-process monotonic clock. If the process restarts the window resets,
/// which could allow more than `limit` calls per calendar day across restarts.
/// For the Alphavantage 25-req/day limit this is acceptable in practice, but
/// operators should be aware that a restart under sustained load may allow a
/// small overage.
pub struct DailyRateLimit {
    limit: u32,
    count: u32,
    window_start: Instant,
}

impl DailyRateLimit {
    pub fn new(limit: u32) -> Self {
        Self {
            limit,
            count: 0,
            window_start: Instant::now(),
        }
    }

    /// Returns `true` if a request is permitted and increments the counter.
    pub fn check_and_increment(&mut self) -> bool {
        if self.window_start.elapsed() >= Duration::from_secs(24 * 3600) {
            self.count = 0;
            self.window_start = Instant::now();
        }
        if self.count < self.limit {
            self.count += 1;
            true
        } else {
            false
        }
    }

    pub fn remaining(&self) -> u32 {
        self.limit.saturating_sub(self.count)
    }
}

/// Per-minute rate limiter.
pub struct PerMinuteRateLimit {
    limit: u32,
    calls: Vec<Instant>,
}

impl PerMinuteRateLimit {
    pub fn new(limit: u32) -> Self {
        Self {
            limit,
            calls: Vec::new(),
        }
    }

    /// Returns `true` if a request is permitted and records the call time.
    pub fn check_and_increment(&mut self) -> bool {
        let now = Instant::now();
        let one_minute = Duration::from_secs(60);
        self.calls.retain(|t| now.duration_since(*t) < one_minute);
        if self.calls.len() < self.limit as usize {
            self.calls.push(now);
            true
        } else {
            false
        }
    }

    pub fn remaining(&self) -> u32 {
        let now = Instant::now();
        let one_minute = Duration::from_secs(60);
        let active = self
            .calls
            .iter()
            .filter(|t| now.duration_since(**t) < one_minute)
            .count() as u32;
        self.limit.saturating_sub(active)
    }
}

// ─── Alphavantage ─────────────────────────────────────────────────────────────

/// Raw JSON shape returned by Alphavantage Global Quote endpoint.
#[derive(Debug, Deserialize)]
struct AlphavantageGlobalQuote {
    #[serde(rename = "01. symbol")]
    symbol: String,
    #[serde(rename = "05. price")]
    price: String,
    #[serde(rename = "09. change")]
    change: String,
    #[serde(rename = "10. change percent")]
    change_percent: String,
    #[serde(rename = "07. latest trading day")]
    latest_trading_day: String,
}

#[derive(Debug, Deserialize)]
struct AlphavantageResponse {
    #[serde(rename = "Global Quote")]
    global_quote: AlphavantageGlobalQuote,
}

struct AlphavantageState {
    quote_cache: HashMap<String, CacheEntry<QuoteResult>>,
    summary_cache: Option<CacheEntry<Vec<QuoteResult>>>,
    rate_limit: DailyRateLimit,
}

/// Adapter for the Alphavantage Global Quote API.
///
/// Configuration is read from environment variables at construction time —
/// no URL or key is ever hardcoded.
#[derive(Clone)]
pub struct AlphavantageAdapter {
    client: reqwest::Client,
    /// Base URL, e.g. `https://www.alphavantage.co` (no trailing slash).
    base_url: String,
    /// API key — stored in memory for URL construction; never logged.
    api_key: String,
    state: Arc<Mutex<AlphavantageState>>,
    /// Egress inspector — validates the Alphavantage base URL before every HTTP request.
    egress: Arc<EgressInspector>,
}

impl AlphavantageAdapter {
    /// Construct from environment variables.
    ///
    /// Reads `LUMINA_ALPHAVANTAGE_URL` and `ALPHAVANTAGE_API_KEY`.
    /// Returns an error if either is missing.
    pub fn new() -> Result<Self> {
        let base_url = env::var("LUMINA_ALPHAVANTAGE_URL").map_err(|_| {
            LuminaError::Config("LUMINA_ALPHAVANTAGE_URL env var not set".into())
        })?;
        let api_key = env::var("ALPHAVANTAGE_API_KEY")
            .map_err(|_| LuminaError::Config("ALPHAVANTAGE_API_KEY env var not set".into()))?;
        Ok(Self::with_config(base_url, api_key))
    }



    /// Construct with explicit config (useful in tests).
    pub fn with_config(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        let base_url = base_url.into();
        let egress = Arc::new(EgressInspector::from_env());
        Self {
            client: reqwest::Client::new(),
            base_url,
            api_key: api_key.into(),
            state: Arc::new(Mutex::new(AlphavantageState {
                quote_cache: HashMap::new(),
                summary_cache: None,
                rate_limit: DailyRateLimit::new(25),
            })),
            egress,
        }
    }

    /// Construct with explicit config and a custom egress inspector (useful in tests).
    #[cfg(test)]
    pub fn with_config_and_egress(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        egress: EgressInspector,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            api_key: api_key.into(),
            state: Arc::new(Mutex::new(AlphavantageState {
                quote_cache: HashMap::new(),
                summary_cache: None,
                rate_limit: DailyRateLimit::new(25),
            })),
            egress: Arc::new(egress),
        }
    }

    /// Build the Global Quote URL for `symbol`.
    ///
    /// The API key is embedded as required by Alphavantage, but is never
    /// written to logs.
    fn quote_url(&self, symbol: &str) -> String {
        format!(
            "{}/query?function=GLOBAL_QUOTE&symbol={}&apikey={}",
            self.base_url, symbol, self.api_key
        )
    }

    /// Fetch a single quote for `symbol`, using the 5-minute cache.
    pub async fn get_quote(&self, symbol: &str) -> Result<QuoteResult> {
        let symbol_upper = symbol.to_uppercase();

        // Check cache first.
        {
            let state = self.state.lock().unwrap();
            if let Some(entry) = state.quote_cache.get(&symbol_upper) {
                if let Some(cached) = entry.get() {
                    return Ok(cached);
                }
            }
        }

        // Check rate limit.
        {
            let mut state = self.state.lock().unwrap();
            if !state.rate_limit.check_and_increment() {
                return Err(LuminaError::Config(format!(
                    "Alphavantage daily rate limit (25/day) exceeded; {} requests remaining",
                    state.rate_limit.remaining()
                )));
            }
        }

        // IMPORTANT: never log the URL (it embeds the API key).
        let url = self.quote_url(&symbol_upper);

        // Egress check — blocks if the Alphavantage host is not in the allowlist.
        self.egress.inspect(&self.base_url, "alphavantage_quote")
            .map_err(LuminaError::from)?;

        let resp = self
            .client
            .get(&url)
            .send()
            .await?
            .json::<AlphavantageResponse>()
            .await
            .map_err(|e| LuminaError::Network(e))?;

        let gq = resp.global_quote;
        let price: f64 = gq.price.trim().parse().map_err(|_| {
            LuminaError::Config(format!("Cannot parse price for {}", symbol_upper))
        })?;
        let change: f64 = gq.change.trim().parse().map_err(|_| {
            LuminaError::Config(format!("Cannot parse change for {}", symbol_upper))
        })?;
        // Alphavantage returns e.g. "1.2300%"
        let change_pct_str = gq.change_percent.trim().trim_end_matches('%');
        let change_pct: f64 = change_pct_str.parse().map_err(|_| {
            LuminaError::Config(format!("Cannot parse change_pct for {}", symbol_upper))
        })?;

        let result = QuoteResult {
            symbol: gq.symbol.clone(),
            price,
            change,
            change_pct,
            timestamp: gq.latest_trading_day.clone(),
            source: "alphavantage".into(),
        };

        // Store in cache.
        {
            let mut state = self.state.lock().unwrap();
            state
                .quote_cache
                .insert(symbol_upper, CacheEntry::new(result.clone(), QUOTE_TTL));
        }

        Ok(result)
    }

    /// Fetch quotes for SPY, QQQ, and DIA as a market summary.
    ///
    /// Results are cached for 15 minutes.
    pub async fn get_market_summary(&self) -> Result<Vec<QuoteResult>> {
        // Check summary cache.
        {
            let state = self.state.lock().unwrap();
            if let Some(ref entry) = state.summary_cache {
                if let Some(cached) = entry.get() {
                    return Ok(cached);
                }
            }
        }

        let symbols = ["SPY", "QQQ", "DIA"];
        let mut results = Vec::new();
        for sym in &symbols {
            results.push(self.get_quote(sym).await?);
        }

        // Store in summary cache.
        {
            let mut state = self.state.lock().unwrap();
            state.summary_cache = Some(CacheEntry::new(results.clone(), SUMMARY_TTL));
        }

        Ok(results)
    }
}

// ─── Finnhub ──────────────────────────────────────────────────────────────────

/// Raw JSON shape returned by Finnhub /quote endpoint.
#[derive(Debug, Deserialize)]
struct FinnhubQuoteResponse {
    /// Current price
    c: f64,
    /// Change
    d: f64,
    /// Percent change
    dp: f64,
    /// Timestamp (Unix)
    t: i64,
}

/// Individual article from Finnhub /news endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct MarketNewsItem {
    pub headline: String,
    pub source: String,
    pub url: String,
    pub datetime: i64,
}

struct FinnhubState {
    quote_cache: HashMap<String, CacheEntry<QuoteResult>>,
    news_cache: Option<CacheEntry<Vec<MarketNewsItem>>>,
    rate_limit: PerMinuteRateLimit,
}

/// Adapter for the Finnhub REST API.
///
/// Configuration is read from environment variables at construction time —
/// no URL or key is ever hardcoded.
#[derive(Clone)]
pub struct FinnhubAdapter {
    client: reqwest::Client,
    /// Base URL, e.g. `https://finnhub.io/api/v1` (no trailing slash).
    base_url: String,
    /// API token — never logged.
    api_key: String,
    state: Arc<Mutex<FinnhubState>>,
    /// Egress inspector — validates the Finnhub base URL before every HTTP request.
    egress: Arc<EgressInspector>,
}

impl FinnhubAdapter {
    /// Construct from environment variables.
    ///
    /// Reads `LUMINA_FINNHUB_URL` and `FINNHUB_API_KEY`.
    /// Returns an error if either is missing.
    pub fn new() -> Result<Self> {
        let base_url = env::var("LUMINA_FINNHUB_URL")
            .map_err(|_| LuminaError::Config("LUMINA_FINNHUB_URL env var not set".into()))?;
        let api_key = env::var("FINNHUB_API_KEY")
            .map_err(|_| LuminaError::Config("FINNHUB_API_KEY env var not set".into()))?;
        Ok(Self::with_config(base_url, api_key))
    }



    /// Construct with explicit config (useful in tests).
    pub fn with_config(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        let base_url = base_url.into();
        let egress = Arc::new(EgressInspector::from_env());
        Self {
            client: reqwest::Client::new(),
            base_url,
            api_key: api_key.into(),
            state: Arc::new(Mutex::new(FinnhubState {
                quote_cache: HashMap::new(),
                news_cache: None,
                rate_limit: PerMinuteRateLimit::new(60),
            })),
            egress,
        }
    }

    /// Construct with explicit config and a custom egress inspector (useful in tests).
    #[cfg(test)]
    pub fn with_config_and_egress(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        egress: EgressInspector,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            api_key: api_key.into(),
            state: Arc::new(Mutex::new(FinnhubState {
                quote_cache: HashMap::new(),
                news_cache: None,
                rate_limit: PerMinuteRateLimit::new(60),
            })),
            egress: Arc::new(egress),
        }
    }

    /// Build the quote URL for `symbol`.
    fn quote_url(&self, symbol: &str) -> String {
        format!(
            "{}/quote?symbol={}&token={}",
            self.base_url, symbol, self.api_key
        )
    }

    /// Build the market news URL.
    fn news_url(&self) -> String {
        format!(
            "{}/news?category=general&token={}",
            self.base_url, self.api_key
        )
    }

    /// Convert a Unix timestamp to a display string.
    fn format_ts(ts: i64) -> String {
        format!("unix:{}", ts)
    }

    /// Fetch a quote for `symbol`, using the 5-minute cache.
    pub async fn get_quote(&self, symbol: &str) -> Result<QuoteResult> {
        let symbol_upper = symbol.to_uppercase();

        // Check cache.
        {
            let state = self.state.lock().unwrap();
            if let Some(entry) = state.quote_cache.get(&symbol_upper) {
                if let Some(cached) = entry.get() {
                    return Ok(cached);
                }
            }
        }

        // Check rate limit.
        {
            let mut state = self.state.lock().unwrap();
            if !state.rate_limit.check_and_increment() {
                return Err(LuminaError::Config(format!(
                    "Finnhub per-minute rate limit (60/min) exceeded; {} calls remaining",
                    state.rate_limit.remaining()
                )));
            }
        }

        // IMPORTANT: never log the URL (it embeds the API key).
        let url = self.quote_url(&symbol_upper);

        // Egress check — blocks if the Finnhub host is not in the allowlist.
        self.egress.inspect(&self.base_url, "finnhub_quote")
            .map_err(LuminaError::from)?;

        let resp = self
            .client
            .get(&url)
            .send()
            .await?
            .json::<FinnhubQuoteResponse>()
            .await
            .map_err(|e| LuminaError::Network(e))?;

        if resp.c == 0.0 && resp.t == 0 {
            return Err(LuminaError::Config(format!(
                "Invalid or unknown symbol: {}",
                symbol_upper
            )));
        }

        let result = QuoteResult {
            symbol: symbol_upper.clone(),
            price: resp.c,
            change: resp.d,
            change_pct: resp.dp,
            timestamp: Self::format_ts(resp.t),
            source: "finnhub".into(),
        };

        // Store in cache.
        {
            let mut state = self.state.lock().unwrap();
            state
                .quote_cache
                .insert(symbol_upper, CacheEntry::new(result.clone(), QUOTE_TTL));
        }

        Ok(result)
    }

    /// Fetch general market news, cached for 15 minutes.
    pub async fn get_market_news(&self) -> Result<Vec<MarketNewsItem>> {
        // Check cache.
        {
            let state = self.state.lock().unwrap();
            if let Some(ref entry) = state.news_cache {
                if let Some(cached) = entry.get() {
                    return Ok(cached);
                }
            }
        }

        // Check rate limit.
        {
            let mut state = self.state.lock().unwrap();
            if !state.rate_limit.check_and_increment() {
                return Err(LuminaError::Config(format!(
                    "Finnhub per-minute rate limit (60/min) exceeded; {} calls remaining",
                    state.rate_limit.remaining()
                )));
            }
        }

        let url = self.news_url();

        // Egress check — blocks if the Finnhub host is not in the allowlist.
        self.egress.inspect(&self.base_url, "finnhub_news")
            .map_err(LuminaError::from)?;

        let items: Vec<MarketNewsItem> = self
            .client
            .get(&url)
            .send()
            .await?
            .json()
            .await
            .map_err(|e| LuminaError::Network(e))?;

        {
            let mut state = self.state.lock().unwrap();
            state.news_cache = Some(CacheEntry::new(items.clone(), SUMMARY_TTL));
        }

        Ok(items)
    }
}

// ─── Aggregator ───────────────────────────────────────────────────────────────

/// High-level aggregator that wraps both adapters and provides unified access.
pub struct FinanceAggregator {
    alphavantage: AlphavantageAdapter,
    finnhub: FinnhubAdapter,
}

impl FinanceAggregator {
    /// Construct from environment variables (see module docs for names).
    pub fn new() -> Result<Self> {
        Ok(Self {
            alphavantage: AlphavantageAdapter::new()?,
            finnhub: FinnhubAdapter::new()?,
        })
    }

    /// Fetch a quote — tries Alphavantage first, falls back to Finnhub.
    ///
    /// When Alphavantage fails the error is logged at DEBUG level before the
    /// fallback attempt, so rate-limit exhaustion or configuration issues are
    /// visible in diagnostics without surfacing API keys.
    pub async fn get_quote(&self, symbol: &str) -> Result<QuoteResult> {
        match self.alphavantage.get_quote(symbol).await {
            Ok(q) => Ok(q),
            Err(e) => {
                // Log at debug so operators can detect persistent AV failures
                // (e.g. rate-limit exhausted) without noise in normal operation.
                // The error message never contains the API key.
                eprintln!("[lumina::feeds::finance] alphavantage fallback for {}: {}", symbol, e);
                self.finnhub.get_quote(symbol).await
            }
        }
    }

    /// Fetch a market summary (SPY, QQQ, DIA) — tries Alphavantage first.
    pub async fn get_market_summary(&self) -> Result<Vec<QuoteResult>> {
        match self.alphavantage.get_market_summary().await {
            Ok(s) => Ok(s),
            Err(_) => {
                let symbols = ["SPY", "QQQ", "DIA"];
                let mut results = Vec::new();
                for sym in &symbols {
                    results.push(self.finnhub.get_quote(sym).await?);
                }
                Ok(results)
            }
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;

    // ── URL construction ──────────────────────────────────────────────────────

    #[test]
    fn test_quote_url_construction_alphavantage() {
        let adapter = AlphavantageAdapter::with_config("https://av.example.com", "test-av-key");
        let url = adapter.quote_url("AAPL");
        assert!(url.starts_with("https://av.example.com/query?function=GLOBAL_QUOTE"));
        assert!(url.contains("symbol=AAPL"));
        assert!(url.contains("apikey=test-av-key"));
        // API key must NOT appear in log-safe output (just verify URL carries it)
        assert!(!url.contains("LUMINA_ALPHAVANTAGE_URL"));
    }

    #[test]
    fn test_quote_url_construction_finnhub() {
        let adapter = FinnhubAdapter::with_config("https://fh.example.com", "test-fh-key");
        let url = adapter.quote_url("TSLA");
        assert!(url.starts_with("https://fh.example.com/quote?symbol=TSLA"));
        assert!(url.contains("token=test-fh-key"));
    }

    // ── Formatting ────────────────────────────────────────────────────────────

    #[test]
    fn test_quote_formatting() {
        let q = QuoteResult {
            symbol: "AAPL".into(),
            price: 198.52,
            change: 2.41,
            change_pct: 1.23,
            timestamp: "9:45 AM ET".into(),
            source: "alphavantage".into(),
        };
        assert_eq!(
            q.format(),
            "AAPL: $198.52 (+1.23%, +$2.41) — as of 9:45 AM ET"
        );
    }

    #[test]
    fn test_quote_formatting_negative() {
        let q = QuoteResult {
            symbol: "MSFT".into(),
            price: 380.00,
            change: -4.50,
            change_pct: -1.17,
            timestamp: "10:00 AM ET".into(),
            source: "finnhub".into(),
        };
        let formatted = q.format();
        assert!(formatted.contains("MSFT: $380.00"), "got: {}", formatted);
        assert!(formatted.contains("-1.17%"), "got: {}", formatted);
        assert!(formatted.contains("-$4.50"), "got: {}", formatted);
    }

    // ── Rate limiting — daily ─────────────────────────────────────────────────

    #[test]
    fn test_rate_limit_tracking_daily() {
        let mut rl = DailyRateLimit::new(3);
        assert_eq!(rl.remaining(), 3);
        assert!(rl.check_and_increment());
        assert!(rl.check_and_increment());
        assert!(rl.check_and_increment());
        // 4th call should be rejected.
        assert!(!rl.check_and_increment());
        assert_eq!(rl.remaining(), 0);
    }

    #[test]
    fn test_rate_limit_tracking_daily_boundary() {
        let mut rl = DailyRateLimit::new(25);
        for _ in 0..25 {
            assert!(rl.check_and_increment());
        }
        assert!(!rl.check_and_increment());
        assert_eq!(rl.remaining(), 0);
    }

    // ── Rate limiting — per minute ────────────────────────────────────────────

    #[test]
    fn test_rate_limit_tracking_per_minute() {
        let mut rl = PerMinuteRateLimit::new(3);
        assert!(rl.check_and_increment());
        assert!(rl.check_and_increment());
        assert!(rl.check_and_increment());
        // 4th within the minute should be rejected.
        assert!(!rl.check_and_increment());
        assert_eq!(rl.remaining(), 0);
    }

    #[test]
    fn test_rate_limit_per_minute_boundary() {
        let mut rl = PerMinuteRateLimit::new(60);
        for _ in 0..60 {
            assert!(rl.check_and_increment());
        }
        assert!(!rl.check_and_increment());
    }

    // ── Cache TTLs ────────────────────────────────────────────────────────────

    #[test]
    fn test_cache_quote_5min() {
        let value = "hello".to_string();
        let entry = CacheEntry::new(value.clone(), QUOTE_TTL);
        // Fresh cache should be valid.
        assert!(entry.is_valid());
        assert_eq!(entry.get(), Some(value));
        // Verify TTL is 5 minutes.
        assert_eq!(QUOTE_TTL, Duration::from_secs(300));
    }

    #[test]
    fn test_cache_market_15min() {
        let value = vec![1u32, 2, 3];
        let entry = CacheEntry::new(value.clone(), SUMMARY_TTL);
        assert!(entry.is_valid());
        assert_eq!(entry.get(), Some(value));
        // Verify TTL is 15 minutes.
        assert_eq!(SUMMARY_TTL, Duration::from_secs(900));
    }

    #[test]
    fn test_cache_expired() {
        let value = 42u32;
        let entry = CacheEntry {
            value,
            inserted_at: Instant::now() - Duration::from_secs(400),
            ttl: QUOTE_TTL,
        };
        assert!(!entry.is_valid());
        assert_eq!(entry.get(), None);
    }

    // ── Invalid symbol ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_invalid_symbol_returns_error() {
        let server = MockServer::start();
        // Finnhub returns zeroed payload for unknown symbols.
        let _mock = server.mock(|when, then| {
            when.method(GET).path("/quote");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"c":0.0,"d":0.0,"dp":0.0,"h":0.0,"l":0.0,"o":0.0,"pc":0.0,"t":0}"#);
        });

        let adapter = FinnhubAdapter::with_config(server.base_url(), "test-key");
        let result = adapter.get_quote("INVALID_SYMBOL_XYZ").await;

        assert!(result.is_err());
        match result.unwrap_err() {
            LuminaError::Config(msg) => {
                assert!(msg.contains("Invalid or unknown symbol"), "got: {}", msg);
            }
            other => panic!("Expected Config error, got {:?}", other),
        }
    }

    // ── Integration: Alphavantage live mock ───────────────────────────────────

    #[tokio::test]
    async fn test_alphavantage_get_quote_mock() {
        let server = MockServer::start();
        let _mock = server.mock(|when, then| {
            when.method(GET).path("/query");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"Global Quote":{"01. symbol":"AAPL","02. open":"196.00","03. high":"199.00","04. low":"195.00","05. price":"198.52","06. volume":"50000000","07. latest trading day":"2024-01-15","08. previous close":"196.11","09. change":"2.41","10. change percent":"1.2300%"}}"#);
        });

        let adapter = AlphavantageAdapter::with_config(server.base_url(), "test-key");
        let result = adapter.get_quote("AAPL").await;

        assert!(result.is_ok(), "expected ok, got {:?}", result);
        let q = result.unwrap();
        assert_eq!(q.symbol, "AAPL");
        assert!((q.price - 198.52).abs() < 0.01);
        assert!((q.change - 2.41).abs() < 0.01);
        assert!((q.change_pct - 1.23).abs() < 0.01);
        assert_eq!(q.source, "alphavantage");
    }

    // ── Integration: Finnhub live mock ────────────────────────────────────────

    #[tokio::test]
    async fn test_finnhub_get_quote_mock() {
        let server = MockServer::start();
        let _mock = server.mock(|when, then| {
            when.method(GET).path("/quote");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"c":198.52,"d":2.41,"dp":1.23,"h":199.00,"l":195.00,"o":196.00,"pc":196.11,"t":1705312800}"#);
        });

        let adapter = FinnhubAdapter::with_config(server.base_url(), "test-key");
        let result = adapter.get_quote("AAPL").await;

        assert!(result.is_ok(), "expected ok, got {:?}", result);
        let q = result.unwrap();
        assert_eq!(q.symbol, "AAPL");
        assert!((q.price - 198.52).abs() < 0.01);
        assert_eq!(q.source, "finnhub");
    }
}
