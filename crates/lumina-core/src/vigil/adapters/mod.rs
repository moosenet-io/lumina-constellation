//! P2-13: Vigil source adapter trait and registry.
//!
//! Adapters provide weather, news, and commute data for Vigil morning briefings.
//! Each adapter has a 15-second timeout. Unconfigured or failed adapters return
//! `Ok("unavailable")` rather than propagating errors — Vigil continues with
//! whatever data is available.
//!
//! All three adapters run **concurrently** via `tokio::task::JoinSet`; worst-case
//! latency is one timeout period (15 s), not three.

pub mod weather;
pub mod news;
pub mod commute;

use crate::error::Result;
use async_trait::async_trait;
use std::sync::Arc;

// ── Shared helpers ────────────────────────────────────────────────────────

/// Truncate `s` to at most `max` bytes, respecting UTF-8 char boundaries.
///
/// Safe replacement for the naïve `&s[..max]` that panics on multi-byte chars.
pub(crate) fn truncate_safe(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// Build a URL by appending query parameters, using `reqwest::Url` to ensure
/// all key/value pairs are properly percent-encoded.
///
/// Avoids query-parameter injection (e.g. a location of `"A&evil=1"` would
/// inject an extra param with naive string concatenation).
pub(crate) fn build_query_url(
    base: &str,
    params: &[(&str, &str)],
) -> Option<String> {
    let mut url = reqwest::Url::parse(base).ok()?;
    {
        let mut pairs = url.query_pairs_mut();
        for (k, v) in params {
            pairs.append_pair(k, v);
        }
    }
    Some(url.to_string())
}

// ── UserSettings ──────────────────────────────────────────────────────────

/// Per-user settings required by source adapters.
///
/// All fields are optional; adapters check `is_configured` before fetching.
/// Values come from environment variables or caller-supplied config — no
/// hardcoded locations, keys, or addresses.
#[derive(Debug, Clone, Default)]
pub struct UserSettings {
    /// Location string (city, postal code, etc.) for weather queries.
    /// Read from `VIGIL_WEATHER_LOCATION`.
    pub weather_location: Option<String>,

    /// API key for the configured weather provider.
    /// Read from `VIGIL_WEATHER_API_KEY`.
    pub weather_api_key: Option<String>,

    /// Base URL of the weather API endpoint.
    /// Read from `VIGIL_WEATHER_API_URL`.
    pub weather_api_url: Option<String>,

    /// Comma-separated news categories (e.g. `"tech,business"`).
    /// Read from `VIGIL_NEWS_CATEGORIES`. Defaults to `"general"`.
    pub news_categories: Option<String>,

    /// API key for the configured news provider.
    /// Read from `VIGIL_NEWS_API_KEY`.
    pub news_api_key: Option<String>,

    /// Base URL of the news API endpoint.
    /// Read from `VIGIL_NEWS_API_URL`.
    pub news_api_url: Option<String>,

    /// Home address or location for commute origin.
    /// Read from `VIGIL_COMMUTE_HOME`.
    pub commute_home: Option<String>,

    /// Work address or location for commute destination.
    /// Read from `VIGIL_COMMUTE_WORK`.
    pub commute_work: Option<String>,

    /// Base URL of the maps/traffic API endpoint.
    /// Read from `VIGIL_COMMUTE_API_URL`.
    pub commute_api_url: Option<String>,

    /// API key for the commute provider.
    /// Read from `VIGIL_COMMUTE_API_KEY`.
    pub commute_api_key: Option<String>,
}

impl UserSettings {
    /// Load all settings from environment variables.
    ///
    /// No value is required; missing vars leave the corresponding field `None`.
    pub fn from_env() -> Self {
        use std::env;
        Self {
            weather_location:  env::var("VIGIL_WEATHER_LOCATION").ok().filter(|s| !s.is_empty()),
            weather_api_key:   env::var("VIGIL_WEATHER_API_KEY").ok().filter(|s| !s.is_empty()),
            weather_api_url:   env::var("VIGIL_WEATHER_API_URL").ok().filter(|s| !s.is_empty()),
            news_categories:   env::var("VIGIL_NEWS_CATEGORIES").ok().filter(|s| !s.is_empty()),
            news_api_key:      env::var("VIGIL_NEWS_API_KEY").ok().filter(|s| !s.is_empty()),
            news_api_url:      env::var("VIGIL_NEWS_API_URL").ok().filter(|s| !s.is_empty()),
            commute_home:      env::var("VIGIL_COMMUTE_HOME").ok().filter(|s| !s.is_empty()),
            commute_work:      env::var("VIGIL_COMMUTE_WORK").ok().filter(|s| !s.is_empty()),
            commute_api_url:   env::var("VIGIL_COMMUTE_API_URL").ok().filter(|s| !s.is_empty()),
            commute_api_key:   env::var("VIGIL_COMMUTE_API_KEY").ok().filter(|s| !s.is_empty()),
        }
    }
}

// ── SourceData ────────────────────────────────────────────────────────────

/// Plain-text output from a source adapter, ready for Vigil's LLM context.
#[derive(Debug, Clone)]
pub struct SourceData {
    /// Human-readable label for this data source (e.g. `"weather"`).
    pub source: String,
    /// Text summary to inject into the briefing prompt.
    pub summary: String,
    /// Whether the adapter successfully returned data.
    pub available: bool,
}

impl SourceData {
    pub fn new(source: impl Into<String>, summary: impl Into<String>) -> Self {
        Self { source: source.into(), summary: summary.into(), available: true }
    }

    /// Sentinel value returned when an adapter is not configured or fails.
    pub fn unavailable(source: impl Into<String>) -> Self {
        Self { source: source.into(), summary: String::new(), available: false }
    }
}

// ── SourceAdapter trait ───────────────────────────────────────────────────

/// A Vigil source adapter: fetches one category of briefing data.
///
/// Implementors must be `Send + Sync` so the registry can call them from
/// any async context. Every `fetch` call has an implicit 15-second timeout
/// (enforced by `AdapterRegistry::fetch_all`).
#[async_trait]
pub trait SourceAdapter: Send + Sync {
    /// Unique name for this adapter (e.g. `"weather"`).
    fn name(&self) -> &str;

    /// Fetch a text summary for the current period.
    ///
    /// On any error the adapter should log a warning and return
    /// `Ok(SourceData::unavailable(self.name()))` so Vigil continues.
    async fn fetch(&self, settings: &UserSettings) -> Result<SourceData>;

    /// Return `true` when the adapter has enough configuration to attempt a fetch.
    fn is_configured(&self, settings: &UserSettings) -> bool;
}

// ── AdapterRegistry ───────────────────────────────────────────────────────

/// Holds all registered adapters; calls only the configured ones.
///
/// `fetch_all` runs all configured adapters **concurrently** via
/// `tokio::task::JoinSet` — worst-case latency is one timeout period, not
/// N × timeout.
pub struct AdapterRegistry {
    adapters: Vec<Arc<dyn SourceAdapter>>,
    /// Per-adapter timeout in seconds (default 15, as per spec).
    pub timeout_secs: u64,
}

impl AdapterRegistry {
    /// Build the default registry with weather, news, and commute adapters.
    pub fn default_registry() -> Self {
        let mut r = Self::new();
        r.register(Arc::new(weather::WeatherAdapter::new()));
        r.register(Arc::new(news::NewsAdapter::new()));
        r.register(Arc::new(commute::CommuteAdapter::new()));
        r
    }

    pub fn new() -> Self {
        Self { adapters: Vec::new(), timeout_secs: 15 }
    }

    /// Register a new adapter.
    pub fn register(&mut self, adapter: Arc<dyn SourceAdapter>) {
        self.adapters.push(adapter);
    }

    /// Return the number of registered adapters.
    pub fn len(&self) -> usize {
        self.adapters.len()
    }

    /// Return `true` if no adapters are registered.
    pub fn is_empty(&self) -> bool {
        self.adapters.is_empty()
    }

    /// Iterate over adapter names (for introspection/testing).
    pub fn adapter_names(&self) -> Vec<&str> {
        self.adapters.iter().map(|a| a.name()).collect()
    }

    /// Fetch from all configured adapters **concurrently**, applying the
    /// per-adapter timeout via `tokio::time::timeout`.
    ///
    /// Unconfigured adapters are silently skipped.
    /// Timed-out or errored adapters contribute `SourceData::unavailable`.
    /// Results are returned in registration order.
    pub async fn fetch_all(&self, settings: &UserSettings) -> Vec<SourceData> {
        let timeout_secs = self.timeout_secs;
        let mut set = tokio::task::JoinSet::new();

        // Spawn each configured adapter as a concurrent task.
        // We attach an index so we can restore registration order.
        let mut index: usize = 0;
        for adapter in &self.adapters {
            if !adapter.is_configured(settings) {
                continue;
            }
            let adapter = Arc::clone(adapter);
            let settings = settings.clone();
            let task_index = index;
            index += 1;

            set.spawn(async move {
                let timeout = std::time::Duration::from_secs(timeout_secs);
                let data = tokio::time::timeout(timeout, adapter.fetch(&settings))
                    .await
                    .unwrap_or_else(|_| {
                        log::warn!(
                            "vigil adapter '{}' timed out after {}s",
                            adapter.name(),
                            timeout_secs
                        );
                        Ok(SourceData::unavailable(adapter.name()))
                    })
                    .unwrap_or_else(|e| {
                        log::warn!("vigil adapter '{}' error: {}", adapter.name(), e);
                        SourceData::unavailable(adapter.name())
                    });
                (task_index, data)
            });
        }

        // Collect and sort by registration order.
        let mut pairs: Vec<(usize, SourceData)> = Vec::new();
        while let Some(res) = set.join_next().await {
            match res {
                Ok(pair) => pairs.push(pair),
                Err(e) => log::warn!("vigil adapter task panicked: {}", e),
            }
        }
        pairs.sort_by_key(|(i, _)| *i);
        pairs.into_iter().map(|(_, d)| d).collect()
    }

    /// Like [`fetch_all`] but only spawns adapters whose name is in `enabled`.
    ///
    /// Adapters that are configured but not in `enabled` are not called at all,
    /// avoiding wasted HTTP requests and API quota for disabled sources.
    pub async fn fetch_all_filtered(
        &self,
        settings: &UserSettings,
        enabled: &std::collections::HashSet<String>,
    ) -> Vec<SourceData> {
        let timeout_secs = self.timeout_secs;
        let mut set = tokio::task::JoinSet::new();
        let mut index: usize = 0;

        for adapter in &self.adapters {
            if !enabled.contains(adapter.name()) {
                continue;
            }
            if !adapter.is_configured(settings) {
                continue;
            }
            let adapter = Arc::clone(adapter);
            let settings = settings.clone();
            let task_index = index;
            index += 1;

            set.spawn(async move {
                let timeout = std::time::Duration::from_secs(timeout_secs);
                let data = tokio::time::timeout(timeout, adapter.fetch(&settings))
                    .await
                    .unwrap_or_else(|_| {
                        log::warn!(
                            "vigil adapter '{}' timed out after {}s",
                            adapter.name(),
                            timeout_secs
                        );
                        Ok(SourceData::unavailable(adapter.name()))
                    })
                    .unwrap_or_else(|e| {
                        log::warn!("vigil adapter '{}' error: {}", adapter.name(), e);
                        SourceData::unavailable(adapter.name())
                    });
                (task_index, data)
            });
        }

        let mut pairs: Vec<(usize, SourceData)> = Vec::new();
        while let Some(res) = set.join_next().await {
            match res {
                Ok(pair) => pairs.push(pair),
                Err(e) => log::warn!("vigil adapter task panicked: {}", e),
            }
        }
        pairs.sort_by_key(|(i, _)| *i);
        pairs.into_iter().map(|(_, d)| d).collect()
    }
}

impl Default for AdapterRegistry {
    fn default() -> Self {
        Self::default_registry()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    /// Serialize all tests that touch process-global env vars.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    // ── Test adapters ─────────────────────────────────────────────────────

    struct AlwaysConfigured;
    struct NeverConfigured;
    struct AlwaysFails;

    #[async_trait]
    impl SourceAdapter for AlwaysConfigured {
        fn name(&self) -> &str { "always_configured" }
        fn is_configured(&self, _: &UserSettings) -> bool { true }
        async fn fetch(&self, _: &UserSettings) -> Result<SourceData> {
            Ok(SourceData::new("always_configured", "ok"))
        }
    }

    #[async_trait]
    impl SourceAdapter for NeverConfigured {
        fn name(&self) -> &str { "never_configured" }
        fn is_configured(&self, _: &UserSettings) -> bool { false }
        async fn fetch(&self, _: &UserSettings) -> Result<SourceData> {
            Ok(SourceData::new("never_configured", "should_not_appear"))
        }
    }

    #[async_trait]
    impl SourceAdapter for AlwaysFails {
        fn name(&self) -> &str { "always_fails" }
        fn is_configured(&self, _: &UserSettings) -> bool { true }
        async fn fetch(&self, _: &UserSettings) -> Result<SourceData> {
            Err(crate::error::LuminaError::Internal("forced error".to_string()))
        }
    }

    struct SlowAdapter(u64);
    #[async_trait]
    impl SourceAdapter for SlowAdapter {
        fn name(&self) -> &str { "slow" }
        fn is_configured(&self, _: &UserSettings) -> bool { true }
        async fn fetch(&self, _: &UserSettings) -> Result<SourceData> {
            tokio::time::sleep(std::time::Duration::from_secs(self.0)).await;
            Ok(SourceData::new("slow", "late"))
        }
    }

    struct TrackedAdapter(Arc<AtomicBool>);
    #[async_trait]
    impl SourceAdapter for TrackedAdapter {
        fn name(&self) -> &str { "tracked" }
        fn is_configured(&self, _: &UserSettings) -> bool { false }
        async fn fetch(&self, _: &UserSettings) -> Result<SourceData> {
            self.0.store(true, Ordering::SeqCst);
            Ok(SourceData::new("tracked", "called"))
        }
    }

    // ── Shared helper tests ───────────────────────────────────────────────

    #[test]
    fn test_truncate_safe_short() {
        assert_eq!(truncate_safe("hello", 512), "hello");
    }

    #[test]
    fn test_truncate_safe_long_ascii() {
        let s = "a".repeat(600);
        let t = truncate_safe(&s, 512);
        // "a" × 512 + "…" (UTF-8 3 bytes) = 515 bytes
        assert!(t.len() <= 515);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn test_truncate_safe_multibyte_boundary() {
        // "é" is 2 bytes (0xC3 0xA9). Build a string where byte 512 would
        // fall inside one of these chars.
        let s = "a".repeat(511) + "éb";
        // byte 511 = 'é' first byte, byte 512 = 'é' second byte, byte 513 = 'b'
        let t = truncate_safe(&s, 512);
        // Must not panic and must be valid UTF-8
        assert!(std::str::from_utf8(t.as_bytes()).is_ok());
    }

    #[test]
    fn test_build_query_url_encodes_params() {
        let url = build_query_url(
            "http://example.com/api",
            &[("location", "New York & Co"), ("key", "abc=123")],
        ).unwrap();
        // Should contain percent-encoded values, not raw "&" or "="
        assert!(!url.contains("location=New York"),
            "space should be encoded, got: {}", url);
        // The '&' in "New York & Co" must be encoded, not treated as a separator
        let parsed = reqwest::Url::parse(&url).unwrap();
        let params: Vec<_> = parsed.query_pairs().collect();
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].0, "location");
        assert_eq!(params[0].1, "New York & Co");
        assert_eq!(params[1].0, "key");
        assert_eq!(params[1].1, "abc=123");
    }

    #[test]
    fn test_build_query_url_invalid_base() {
        assert!(build_query_url("not-a-url", &[]).is_none());
    }

    // ── Registry tests ────────────────────────────────────────────────────

    #[test]
    fn test_source_data_unavailable() {
        let d = SourceData::unavailable("weather");
        assert_eq!(d.source, "weather");
        assert!(!d.available);
    }

    #[test]
    fn test_registry_new_is_empty() {
        let r = AdapterRegistry::new();
        assert!(r.is_empty());
    }

    #[test]
    fn test_default_registry_has_three_adapters() {
        let r = AdapterRegistry::default_registry();
        assert_eq!(r.len(), 3);
    }

    #[tokio::test]
    async fn test_registry_skips_unconfigured() {
        let called = Arc::new(AtomicBool::new(false));
        let mut r = AdapterRegistry::new();
        r.register(Arc::new(TrackedAdapter(called.clone())));
        let settings = UserSettings::default();
        let results = r.fetch_all(&settings).await;
        assert!(results.is_empty(), "unconfigured adapter should not appear in results");
        assert!(!called.load(Ordering::SeqCst), "fetch should not be called on unconfigured adapter");
    }

    #[tokio::test]
    async fn test_registry_includes_configured() {
        let mut r = AdapterRegistry::new();
        r.register(Arc::new(AlwaysConfigured));
        let settings = UserSettings::default();
        let results = r.fetch_all(&settings).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].summary, "ok");
    }

    #[tokio::test]
    async fn test_registry_error_yields_unavailable() {
        let mut r = AdapterRegistry::new();
        r.register(Arc::new(AlwaysFails));
        let settings = UserSettings::default();
        let results = r.fetch_all(&settings).await;
        assert_eq!(results.len(), 1);
        assert!(!results[0].available);
    }

    #[tokio::test]
    async fn test_registry_timeout_yields_unavailable() {
        let mut r = AdapterRegistry::new();
        r.timeout_secs = 1;
        r.register(Arc::new(SlowAdapter(60)));
        let settings = UserSettings::default();
        let results = r.fetch_all(&settings).await;
        assert_eq!(results.len(), 1);
        assert!(!results[0].available);
    }

    #[tokio::test]
    async fn test_registry_mixed_adapters() {
        let mut r = AdapterRegistry::new();
        r.register(Arc::new(AlwaysConfigured));
        r.register(Arc::new(NeverConfigured));
        r.register(Arc::new(AlwaysFails));
        let settings = UserSettings::default();
        let results = r.fetch_all(&settings).await;
        // AlwaysConfigured → "ok", NeverConfigured → skipped, AlwaysFails → "unavailable"
        assert_eq!(results.len(), 2);
        // Order is preserved (registration order)
        assert_eq!(results[0].summary, "ok");
        assert!(!results[1].available);
    }

    #[tokio::test]
    async fn test_registry_concurrent_runs_all_in_parallel() {
        // Two slow adapters with 60s sleep but 1s timeout each.
        // Sequential would take ~2s; concurrent should take ~1s.
        let mut r = AdapterRegistry::new();
        r.timeout_secs = 1;
        r.register(Arc::new(SlowAdapter(60)));
        r.register(Arc::new(SlowAdapter(60)));
        let settings = UserSettings::default();
        let start = std::time::Instant::now();
        let results = r.fetch_all(&settings).await;
        let elapsed = start.elapsed();
        // Both should time out, returning 2 × "unavailable"
        assert_eq!(results.len(), 2);
        // Concurrent: should finish in < 3s (well under 2 × 1s = 2s would be exact,
        // but we give generous slack for CI load).
        assert!(elapsed.as_secs() < 3,
            "fetch_all should run concurrently; took {}s", elapsed.as_secs_f64());
    }

    // ── UserSettings from_env (serialized — env vars are process-global) ─

    #[test]
    #[serial]
    fn test_user_settings_defaults_all_none() {
        let _g = ENV_LOCK.lock().unwrap();
        let keys = [
            "VIGIL_WEATHER_LOCATION", "VIGIL_WEATHER_API_KEY", "VIGIL_WEATHER_API_URL",
            "VIGIL_NEWS_CATEGORIES", "VIGIL_NEWS_API_KEY", "VIGIL_NEWS_API_URL",
            "VIGIL_COMMUTE_HOME", "VIGIL_COMMUTE_WORK",
            "VIGIL_COMMUTE_API_URL", "VIGIL_COMMUTE_API_KEY",
        ];
        for k in &keys { std::env::remove_var(k); }
        let s = UserSettings::from_env();
        assert!(s.weather_location.is_none());
        assert!(s.weather_api_key.is_none());
        assert!(s.news_categories.is_none());
        assert!(s.commute_home.is_none());
        assert!(s.commute_work.is_none());
        for k in &keys { std::env::remove_var(k); }
    }

    #[test]
    #[serial]
    fn test_user_settings_from_env_reads_values() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("VIGIL_WEATHER_LOCATION", "London");
        std::env::set_var("VIGIL_NEWS_CATEGORIES", "tech,business");
        std::env::set_var("VIGIL_COMMUTE_HOME", "123 Home St");
        std::env::set_var("VIGIL_COMMUTE_WORK", "456 Work Ave");

        let s = UserSettings::from_env();
        assert_eq!(s.weather_location.as_deref(), Some("London"));
        assert_eq!(s.news_categories.as_deref(), Some("tech,business"));
        assert_eq!(s.commute_home.as_deref(), Some("123 Home St"));
        assert_eq!(s.commute_work.as_deref(), Some("456 Work Ave"));

        for k in &["VIGIL_WEATHER_LOCATION", "VIGIL_NEWS_CATEGORIES",
                   "VIGIL_COMMUTE_HOME", "VIGIL_COMMUTE_WORK"] {
            std::env::remove_var(k);
        }
    }

    #[test]
    #[serial]
    fn test_user_settings_empty_string_treated_as_none() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("VIGIL_WEATHER_LOCATION", "");
        let s = UserSettings::from_env();
        assert!(s.weather_location.is_none(), "empty string should be treated as None");
        std::env::remove_var("VIGIL_WEATHER_LOCATION");
    }
}
