//! P2-13: Weather source adapter for Vigil.
//!
//! Fetches current conditions and a brief forecast from an HTTP JSON API.
//! All configuration (location, API key, endpoint URL) comes from
//! `UserSettings` — no hardcoded values.
//!
//! Required env vars (via `UserSettings::from_env`):
//!   `VIGIL_WEATHER_LOCATION`  — query location (city, postal code, etc.)
//!   `VIGIL_WEATHER_API_KEY`   — provider API key
//!   `VIGIL_WEATHER_API_URL`   — full base URL of the weather endpoint
//!
//! On any error the adapter returns `Ok(SourceData::unavailable("weather"))`.

use super::{build_query_url, truncate_safe, SourceAdapter, SourceData, UserSettings};
use crate::error::Result;
use async_trait::async_trait;
use reqwest::Client;

/// Weather source adapter.
///
/// Calls `{api_url}?location={location}&key={api_key}` (fully percent-encoded)
/// and expects a JSON response.  Supported shapes:
///   1. `{"summary": "..."}` — pre-formatted string
///   2. Anything else — raw body (truncated to 512 bytes on a char boundary)
pub struct WeatherAdapter {
    client: Client,
}

impl WeatherAdapter {
    pub fn new() -> Self {
        Self { client: Client::new() }
    }

    /// Build the request URL from settings, using `reqwest::Url` for proper encoding.
    fn build_url(&self, settings: &UserSettings) -> Option<String> {
        let base     = settings.weather_api_url.as_deref()?;
        let location = settings.weather_location.as_deref()?;
        let key      = settings.weather_api_key.as_deref().unwrap_or("");
        build_query_url(base, &[("location", location), ("key", key)])
    }
}

impl Default for WeatherAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SourceAdapter for WeatherAdapter {
    fn name(&self) -> &str {
        "weather"
    }

    fn is_configured(&self, settings: &UserSettings) -> bool {
        settings.weather_location.is_some()
            && settings.weather_api_url.is_some()
    }

    async fn fetch(&self, settings: &UserSettings) -> Result<SourceData> {
        let url = match self.build_url(settings) {
            Some(u) => u,
            None => {
                log::warn!("weather adapter: missing configuration, returning unavailable");
                return Ok(SourceData::unavailable("weather"));
            }
        };

        let resp = match self.client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                log::warn!("weather adapter: request failed: {}", e);
                return Ok(SourceData::unavailable("weather"));
            }
        };

        if !resp.status().is_success() {
            log::warn!("weather adapter: HTTP {} from endpoint", resp.status());
            return Ok(SourceData::unavailable("weather"));
        }

        let body = match resp.text().await {
            Ok(t) => t,
            Err(e) => {
                log::warn!("weather adapter: failed to read response body: {}", e);
                return Ok(SourceData::unavailable("weather"));
            }
        };

        // Try to extract a top-level "summary" field; fall back to truncated body.
        let summary = extract_summary_field(&body)
            .unwrap_or_else(|| truncate_safe(&body, 512));

        Ok(SourceData::new("weather", summary))
    }
}

// ── Parsing helpers ───────────────────────────────────────────────────────

/// Attempt to deserialise `{"summary": "..."}` from `body`.
fn extract_summary_field(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    v.get("summary")?.as_str().map(|s| s.to_string())
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;

    fn settings_with(url: &str) -> UserSettings {
        UserSettings {
            weather_location:  Some("TestCity".to_string()),
            weather_api_key:   Some("test-key".to_string()),
            weather_api_url:   Some(url.to_string()),
            ..Default::default()
        }
    }

    // ── is_configured ────────────────────────────────────────────────────

    #[test]
    fn test_weather_is_configured_with_location_and_url() {
        let s = UserSettings {
            weather_location: Some("Paris".to_string()),
            weather_api_url:  Some("http://example.com".to_string()),
            ..Default::default()
        };
        assert!(WeatherAdapter::new().is_configured(&s));
    }

    #[test]
    fn test_weather_not_configured_missing_location() {
        let s = UserSettings {
            weather_api_url: Some("http://example.com".to_string()),
            ..Default::default()
        };
        assert!(!WeatherAdapter::new().is_configured(&s));
    }

    #[test]
    fn test_weather_not_configured_missing_url() {
        let s = UserSettings {
            weather_location: Some("Paris".to_string()),
            ..Default::default()
        };
        assert!(!WeatherAdapter::new().is_configured(&s));
    }

    #[test]
    fn test_weather_not_configured_all_missing() {
        assert!(!WeatherAdapter::new().is_configured(&UserSettings::default()));
    }

    // ── URL building ──────────────────────────────────────────────────────

    #[test]
    fn test_build_url_encodes_location_with_special_chars() {
        let a = WeatherAdapter::new();
        let s = UserSettings {
            weather_location: Some("New York & Co".to_string()),
            weather_api_key:  Some("k=1".to_string()),
            weather_api_url:  Some("http://example.com/api".to_string()),
            ..Default::default()
        };
        let url = a.build_url(&s).unwrap();
        let parsed = reqwest::Url::parse(&url).unwrap();
        let params: Vec<_> = parsed.query_pairs().collect();
        assert_eq!(params[0].1, "New York & Co", "location must be preserved exactly");
        assert_eq!(params[1].1, "k=1",           "key must be preserved exactly");
    }

    // ── HTTP responses ────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_weather_returns_summary_on_200() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET).path("/weather");
            then.status(200)
                .header("Content-Type", "application/json")
                .body(r#"{"summary":"Sunny, 22°C"}"#);
        });

        let s = settings_with(&format!("{}/weather", server.base_url()));
        let result = WeatherAdapter::new().fetch(&s).await.unwrap();
        assert_eq!(result.source, "weather");
        assert_eq!(result.summary, "Sunny, 22°C");
        mock.assert();
    }

    #[tokio::test]
    async fn test_weather_returns_unavailable_on_500() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET).path("/weather");
            then.status(500).body("Internal Server Error");
        });

        let s = settings_with(&format!("{}/weather", server.base_url()));
        let result = WeatherAdapter::new().fetch(&s).await.unwrap();
        assert!(!result.available);
        mock.assert();
    }

    #[tokio::test]
    async fn test_weather_returns_unavailable_on_404() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET).path("/weather");
            then.status(404).body("Not Found");
        });

        let s = settings_with(&format!("{}/weather", server.base_url()));
        let result = WeatherAdapter::new().fetch(&s).await.unwrap();
        assert!(!result.available);
        mock.assert();
    }

    #[tokio::test]
    async fn test_weather_falls_back_to_raw_body_when_no_summary_field() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/weather");
            then.status(200)
                .header("Content-Type", "application/json")
                .body(r#"{"temperature":22,"condition":"sunny"}"#);
        });

        let s = settings_with(&format!("{}/weather", server.base_url()));
        let result = WeatherAdapter::new().fetch(&s).await.unwrap();
        assert_eq!(result.source, "weather");
        // Should contain the raw body, not "unavailable"
        assert!(result.summary.contains("22") || result.summary.contains("sunny"));
    }

    #[tokio::test]
    async fn test_weather_returns_unavailable_missing_env() {
        let s = UserSettings::default();
        let result = WeatherAdapter::new().fetch(&s).await.unwrap();
        assert!(!result.available);
    }

    // ── Helper unit tests ─────────────────────────────────────────────────

    #[test]
    fn test_extract_summary_field_present() {
        let body = r#"{"summary":"Rainy, 10°C"}"#;
        assert_eq!(extract_summary_field(body), Some("Rainy, 10°C".to_string()));
    }

    #[test]
    fn test_extract_summary_field_absent() {
        let body = r#"{"temp":10}"#;
        assert!(extract_summary_field(body).is_none());
    }

    #[test]
    fn test_extract_summary_field_invalid_json() {
        assert!(extract_summary_field("not json").is_none());
    }
}
