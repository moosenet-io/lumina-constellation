//! P2-13 / WEB-03: News source adapter for Vigil.
//!
//! Delegates to [`crate::feeds::news::NewsAggregator`] (WEB-03) to fetch
//! multi-source, deduplicated headlines.  The user's `news_categories` setting
//! (comma-separated, default: `"technology,business,general"`) controls which
//! categories are requested; up to `MAX_HEADLINES_PER_CATEGORY` headlines are
//! fetched per category and then merged and formatted as a `SourceData` summary
//! suitable for the Vigil morning briefing prompt.
//!
//! ## Configuration
//!
//! The adapter is considered configured when at least one of the underlying
//! [`NewsAggregator`] sources reports `is_configured()`.  All env vars are read
//! by the aggregator adapters directly:
//!
//! | Variable | Purpose |
//! |---|---|
//! | `LUMINA_GNEWS_BASE_URL` | GNews API base URL |
//! | `GNEWS_API_KEY` | GNews API key |
//! | `LUMINA_NEWSAPI_BASE_URL` | NewsAPI base URL |
//! | `NEWSAPI_KEY` | NewsAPI key |
//!
//! Legacy `VIGIL_NEWS_API_URL` / `VIGIL_NEWS_API_KEY` vars are no longer used;
//! the adapter falls back to `SourceData::unavailable("news")` on any error.
//!
//! Optional (Vigil-layer):
//!   `VIGIL_NEWS_CATEGORIES` — comma-separated categories
//!                              (default: `"technology,business,general"`)

use super::{SourceAdapter, SourceData, UserSettings};
use crate::error::Result;
use crate::feeds::news::NewsAggregator;
use async_trait::async_trait;

const DEFAULT_CATEGORIES: &str = "technology,business,general";
const MAX_HEADLINES_PER_CATEGORY: usize = 3;
const MAX_TOTAL_HEADLINES: usize = 5;

/// News source adapter for Vigil.
///
/// Delegates to `NewsAggregator` (WEB-03) to obtain multi-source, deduplicated
/// headlines, then formats them as a plain-text summary for the briefing prompt.
pub struct NewsAdapter {
    aggregator: NewsAggregator,
}

impl NewsAdapter {
    pub fn new() -> Self {
        Self {
            aggregator: NewsAggregator::from_env(),
        }
    }

    /// Return the list of categories to fetch, split from `settings.news_categories`.
    ///
    /// Falls back to `DEFAULT_CATEGORIES` when the setting is absent or empty.
    fn categories<'a>(&self, settings: &'a UserSettings) -> Vec<String> {
        let raw = settings.news_categories.as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_CATEGORIES);
        raw.split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// Format a slice of [`crate::feeds::Headline`]s as a bullet-point summary.
    fn format_headlines(headlines: &[crate::feeds::Headline]) -> String {
        headlines
            .iter()
            .map(|h| format!("• {} ({})", h.title, h.source))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Default for NewsAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SourceAdapter for NewsAdapter {
    fn name(&self) -> &str {
        "news"
    }

    /// Configured when the underlying `NewsAggregator` has at least one
    /// configured source (GNews or NewsAPI env vars present).
    fn is_configured(&self, _settings: &UserSettings) -> bool {
        self.aggregator.has_configured_sources()
    }

    async fn fetch(&self, settings: &UserSettings) -> Result<SourceData> {
        let categories = self.categories(settings);

        // Collect headlines across all requested categories, then truncate.
        let mut all_headlines: Vec<crate::feeds::Headline> = Vec::new();
        for category in &categories {
            let mut headlines = self.aggregator
                .fetch_headlines(category, MAX_HEADLINES_PER_CATEGORY)
                .await;
            all_headlines.append(&mut headlines);
        }

        if all_headlines.is_empty() {
            log::warn!("news adapter: no headlines returned from aggregator");
            return Ok(SourceData::unavailable("news"));
        }

        // Truncate to the overall cap across all categories.
        all_headlines.truncate(MAX_TOTAL_HEADLINES);
        let summary = Self::format_headlines(&all_headlines);
        Ok(SourceData::new("news", summary))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feeds::{FeedSource, Headline};
    use crate::error::Result as LResult;
    use std::sync::Arc;

    // ── Helpers ───────────────────────────────────────────────────────────

    fn make_headline(title: &str, source: &str, category: &str) -> Headline {
        Headline {
            title: title.to_string(),
            source: source.to_string(),
            url: String::new(),
            published_at: "2026-06-06T12:00:00Z".to_string(),
            snippet: String::new(),
            category: category.to_string(),
        }
    }

    // ── Stub FeedSource that returns a fixed list ──────────────────────────

    struct StubSource {
        headlines: Vec<Headline>,
    }

    #[async_trait::async_trait]
    impl FeedSource for StubSource {
        fn name(&self) -> &str { "stub" }
        fn is_configured(&self) -> bool { true }
        async fn fetch(&self, _category: &str, count: usize) -> LResult<Vec<Headline>> {
            Ok(self.headlines.iter().take(count).cloned().collect())
        }
    }

    struct UnconfiguredSource;

    #[async_trait::async_trait]
    impl FeedSource for UnconfiguredSource {
        fn name(&self) -> &str { "unconfigured" }
        fn is_configured(&self) -> bool { false }
        async fn fetch(&self, _: &str, _: usize) -> LResult<Vec<Headline>> {
            Ok(vec![])
        }
    }

    fn adapter_with_stub(headlines: Vec<Headline>) -> NewsAdapter {
        let mut agg = NewsAggregator::new();
        agg.register(Arc::new(StubSource { headlines }));
        NewsAdapter { aggregator: agg }
    }

    // ── is_configured ─────────────────────────────────────────────────────

    #[test]
    fn test_news_configured_when_aggregator_has_configured_source() {
        let adapter = adapter_with_stub(vec![]);
        assert!(adapter.is_configured(&UserSettings::default()));
    }

    #[test]
    fn test_news_not_configured_when_no_sources() {
        let adapter = NewsAdapter { aggregator: NewsAggregator::new() };
        assert!(!adapter.is_configured(&UserSettings::default()));
    }

    #[test]
    fn test_news_not_configured_when_all_sources_unconfigured() {
        let mut agg = NewsAggregator::new();
        agg.register(Arc::new(UnconfiguredSource));
        let adapter = NewsAdapter { aggregator: agg };
        assert!(!adapter.is_configured(&UserSettings::default()));
    }

    // ── Categories ────────────────────────────────────────────────────────

    #[test]
    fn test_default_categories_when_not_set() {
        let adapter = adapter_with_stub(vec![]);
        let settings = UserSettings::default();
        let cats = adapter.categories(&settings);
        assert!(!cats.is_empty(), "default categories should not be empty");
        // Default includes technology, business, general
        assert!(cats.contains(&"technology".to_string()));
        assert!(cats.contains(&"business".to_string()));
        assert!(cats.contains(&"general".to_string()));
    }

    #[test]
    fn test_user_categories_used_when_set() {
        let adapter = adapter_with_stub(vec![]);
        let settings = UserSettings {
            news_categories: Some("tech,sports".to_string()),
            ..Default::default()
        };
        let cats = adapter.categories(&settings);
        assert_eq!(cats, vec!["tech", "sports"]);
    }

    #[test]
    fn test_empty_category_tokens_dropped() {
        let adapter = adapter_with_stub(vec![]);
        let settings = UserSettings {
            news_categories: Some("tech,,sports".to_string()),
            ..Default::default()
        };
        let cats = adapter.categories(&settings);
        assert_eq!(cats, vec!["tech", "sports"]);
    }

    // ── fetch ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_fetch_returns_headlines_as_source_data() {
        let headlines = vec![
            make_headline("Rust 2.0 released", "TechNews", "technology"),
            make_headline("Markets rally", "FinTimes", "business"),
        ];
        let adapter = adapter_with_stub(headlines);
        let settings = UserSettings {
            news_categories: Some("technology".to_string()),
            ..Default::default()
        };
        let result = adapter.fetch(&settings).await.unwrap();
        assert_eq!(result.source, "news");
        assert!(result.available);
        assert!(result.summary.contains("Rust 2.0 released"));
        assert!(result.summary.contains("TechNews"));
    }

    #[tokio::test]
    async fn test_fetch_returns_unavailable_when_no_headlines() {
        let adapter = adapter_with_stub(vec![]);
        let settings = UserSettings {
            news_categories: Some("technology".to_string()),
            ..Default::default()
        };
        let result = adapter.fetch(&settings).await.unwrap();
        assert!(!result.available, "empty headlines should yield unavailable");
    }

    #[tokio::test]
    async fn test_fetch_truncates_to_max_total() {
        let headlines: Vec<Headline> = (1..=10)
            .map(|i| make_headline(&format!("Story {}", i), "Source", "general"))
            .collect();
        let adapter = adapter_with_stub(headlines);
        let settings = UserSettings {
            news_categories: Some("general".to_string()),
            ..Default::default()
        };
        let result = adapter.fetch(&settings).await.unwrap();
        let count = result.summary.lines().count();
        assert!(
            count <= MAX_TOTAL_HEADLINES,
            "summary should contain at most {} headlines, got {}",
            MAX_TOTAL_HEADLINES,
            count
        );
    }

    #[tokio::test]
    async fn test_fetch_formats_as_bullet_points() {
        let headlines = vec![make_headline("Headline Alpha", "SourceAlpha", "general")];
        let adapter = adapter_with_stub(headlines);
        let settings = UserSettings {
            news_categories: Some("general".to_string()),
            ..Default::default()
        };
        let result = adapter.fetch(&settings).await.unwrap();
        assert!(
            result.summary.starts_with('•'),
            "summary should use bullet points, got: {}",
            result.summary
        );
        assert!(result.summary.contains("Headline Alpha"));
        assert!(result.summary.contains("SourceAlpha"));
    }
}
