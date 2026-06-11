//! Vigil — morning briefing engine and source adapters.
//!
//! This module provides:
//! - P2-13: source adapters (weather, news, commute)
//! - P2-04: briefing engine ([`briefing::VigilBriefing`]) and per-user config
//!
//! ## Quick start — adapters only
//!
//! ```rust,no_run
//! # tokio_test::block_on(async {
//! use lumina_core::vigil::adapters::{AdapterRegistry, UserSettings};
//!
//! let settings = UserSettings::from_env();
//! let registry = AdapterRegistry::default_registry();
//! let data = registry.fetch_all(&settings).await;
//! for item in data {
//!     println!("[{}] {}", item.source, item.summary);
//! }
//! # })
//! ```
//!
//! ## Quick start — morning briefing
//!
//! ```rust,no_run
//! # tokio_test::block_on(async {
//! use lumina_core::vigil::briefing::VigilBriefing;
//! use lumina_core::vigil::config::BriefingConfig;
//! use lumina_core::vigil::adapters::UserSettings;
//!
//! let config   = BriefingConfig::from_env();
//! let settings = UserSettings::from_env();
//! let briefing = VigilBriefing::new(config, settings);
//! let prompt   = briefing.build_prompt().await;
//! // Pass `prompt` to your Chord/LLM call to produce the final text.
//! println!("{}", prompt);
//! # })
//! ```

pub mod adapters;
pub mod briefing;
pub mod config;
pub mod evening;

pub use adapters::{AdapterRegistry, SourceAdapter, SourceData, UserSettings};

/// Fetch data from all configured adapters and return the results.
///
/// This is a convenience wrapper around `AdapterRegistry::default_registry`
/// and `AdapterRegistry::fetch_all`.  Unconfigured adapters are silently
/// skipped; failed or timed-out adapters contribute `"unavailable"` entries.
pub async fn fetch_all(settings: &UserSettings) -> Vec<SourceData> {
    AdapterRegistry::default_registry().fetch_all(settings).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_fetch_all_returns_empty_when_nothing_configured() {
        let settings = UserSettings::default();
        let results = fetch_all(&settings).await;
        assert!(results.is_empty(),
            "no adapters configured → result should be empty, got: {:?}",
            results.iter().map(|r| &r.source).collect::<Vec<_>>());
    }

    #[test]
    fn test_default_registry_has_expected_adapters() {
        let r = AdapterRegistry::default_registry();
        assert_eq!(r.len(), 3);
        let names = r.adapter_names();
        assert!(names.contains(&"weather"), "missing weather adapter");
        assert!(names.contains(&"news"),    "missing news adapter");
        assert!(names.contains(&"commute"), "missing commute adapter");
    }
}
