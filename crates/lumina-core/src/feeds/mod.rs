//! Feed adapters for Lumina Constellation.
//!
//! This module provides:
//! - [`FeedSource`] trait: common interface for all news feed adapters
//! - [`Headline`] struct: normalised article representation
//! - [`FeedRegistry`]: holds adapters, deduplicates, and caches results
//! - [`finance`]: financial data adapters (Alphavantage + Finnhub)
//!
//! ## Quick start — news feeds
//!
//! ```rust,no_run
//! # tokio_test::block_on(async {
//! use lumina_core::feeds::{FeedRegistry, Headline};
//!
//! let registry = FeedRegistry::from_env();
//! let headlines = registry.fetch_headlines("technology", 5).await;
//! for h in &headlines {
//!     println!("[{}] {}", h.source, h.title);
//! }
//! # })
//! ```
//!
//! ## Environment variables — news feeds
//!
//! | Variable | Purpose |
//! |---|---|
//! | `LUMINA_GNEWS_BASE_URL` | GNews API base URL |
//! | `GNEWS_API_KEY` | GNews API key |
//! | `LUMINA_NEWSAPI_BASE_URL` | NewsAPI base URL |
//! | `NEWSAPI_KEY` | NewsAPI key |
//!
//! ## Environment variables — finance feeds
//!
//! | Variable | Purpose |
//! |---|---|
//! | `LUMINA_ALPHAVANTAGE_URL` | Alphavantage base URL |
//! | `ALPHAVANTAGE_API_KEY` | Alphavantage API key |
//! | `LUMINA_FINNHUB_URL` | Finnhub base URL |
//! | `FINNHUB_API_KEY` | Finnhub API key |

pub mod news;
pub mod finance;

use crate::error::Result;
use async_trait::async_trait;

// ── Headline ──────────────────────────────────────────────────────────────

/// A normalised news article from any feed source.
#[derive(Debug, Clone)]
pub struct Headline {
    /// Article title.
    pub title: String,
    /// Human-readable source name (e.g. "BBC News").
    pub source: String,
    /// Direct URL to the full article.
    pub url: String,
    /// ISO 8601 publication timestamp (or empty string if unavailable).
    pub published_at: String,
    /// Short description / snippet of the article.
    pub snippet: String,
    /// Category this article was fetched under (e.g. "technology").
    pub category: String,
}

// ── FeedSource trait ──────────────────────────────────────────────────────

/// A news feed adapter: fetches headlines for a given category.
///
/// Implementors must be `Send + Sync` so they can be stored in an
/// `Arc`-wrapped registry and called from any async context.
///
/// API keys **must never appear** in log output, error messages, or
/// `Headline` fields.
#[async_trait]
pub trait FeedSource: Send + Sync {
    /// Unique, stable name for this adapter (e.g. `"gnews"`).
    fn name(&self) -> &str;

    /// Return `true` when the adapter has the configuration it needs to
    /// attempt a fetch (e.g. API key and base URL are set).
    fn is_configured(&self) -> bool;

    /// Fetch up to `count` headlines for `category`.
    ///
    /// On any transient error return `Ok(vec![])` so the registry can fall
    /// back to the cache. Propagate only programming errors (logic bugs)
    /// as `Err`.
    async fn fetch(&self, category: &str, count: usize) -> Result<Vec<Headline>>;
}

// ── FeedRegistry ──────────────────────────────────────────────────────────

pub use news::NewsAggregator as FeedRegistry;
