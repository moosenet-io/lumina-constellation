//! WEB-05: Commute source adapter for Vigil — DuckDuckGo + web fetch.
//!
//! Fetches estimated travel time and traffic conditions without any external
//! API key by combining WEB-02 (DuckDuckGo search) and WEB-01 (web fetch).
//!
//! # How it works
//! 1. Searches DuckDuckGo: `"current traffic {home} to {work}"`
//! 2. Tries to extract travel time and conditions from the top snippet with regex
//! 3. If no time found in snippet, fetches the top result page for fuller content
//! 4. Formats: `"Commute: ~35 min via I-880 (moderate traffic)"`
//!
//! # Configuration (env vars via `UserSettings`)
//!   `VIGIL_COMMUTE_HOME`   — origin address (required)
//!   `VIGIL_COMMUTE_WORK`   — destination address (required)
//!   `LUMINA_DDG_URL`       — DuckDuckGo base URL (required for search to work)
//!
//! `VIGIL_COMMUTE_API_URL` / `VIGIL_COMMUTE_API_KEY` are accepted but ignored.
//! No API key or signup required.
//!
//! # Cache
//! Results are cached for [`CACHE_TTL_SECS`] (10 minutes) per (home, work) pair.

use super::{SourceAdapter, SourceData, UserSettings};
use crate::error::Result;
use crate::web::search::WebSearch;
use crate::web::WebClient;
use async_trait::async_trait;
use regex::Regex;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Cache TTL in seconds (10 minutes).
const CACHE_TTL_SECS: u64 = 600;

/// Number of search results to request.
const SEARCH_RESULT_COUNT: usize = 3;

/// Characters from a page body to inspect.
const MAX_BODY_CHARS: usize = 2_000;

// ── CommuteAdapter ────────────────────────────────────────────────────────

type CacheEntry = (String, Instant);

/// Vigil commute adapter — uses DuckDuckGo + web fetch, no API key needed.
pub struct CommuteAdapter {
    time_re:    Regex,
    traffic_re: Regex,
    route_re:   Regex,
    cache:      Arc<Mutex<HashMap<(String, String), CacheEntry>>>,
}

impl CommuteAdapter {
    pub fn new() -> Self {
        Self {
            time_re: Regex::new(
                r"(?i)(?:about\s+)?(\d+\s*(?:h(?:r|our)s?\s+\d+\s*min(?:ute)?s?|h(?:r|our)s?|min(?:ute)?s?))"
            ).expect("time regex"),
            traffic_re: Regex::new(
                r"(?i)(light|moderate|heavy|normal|no|severe|slow|clear|mild)\s+(?:traffic|congestion|delay)"
            ).expect("traffic regex"),
            route_re: Regex::new(
                r"(?i)\bvia\s+([\w][\w\s\-\.]{2,25})"
            ).expect("route regex"),
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn cache_get(&self, home: &str, work: &str) -> Option<String> {
        let guard = self.cache.lock().ok()?;
        let (summary, inserted) = guard.get(&(home.to_string(), work.to_string()))?;
        if inserted.elapsed().as_secs() < CACHE_TTL_SECS {
            Some(summary.clone())
        } else {
            None
        }
    }

    fn cache_set(&self, home: &str, work: &str, summary: String) {
        if let Ok(mut guard) = self.cache.lock() {
            guard.insert((home.to_string(), work.to_string()), (summary, Instant::now()));
        }
    }

    fn extract(&self, text: &str) -> CommuteParts {
        let body = &text[..text.len().min(MAX_BODY_CHARS)];
        CommuteParts {
            time:    self.time_re.find(body).map(|m| m.as_str().to_string()),
            traffic: self.traffic_re.captures(body)
                         .and_then(|c| c.get(0))
                         .map(|m| m.as_str().to_lowercase()),
            route:   self.route_re.captures(body)
                         .and_then(|c| c.get(1))
                         .map(|m| m.as_str().trim().to_string()),
        }
    }

    fn format(&self, parts: &CommuteParts, fallback: &str) -> String {
        match &parts.time {
            Some(t) => {
                let mut s = format!("Commute: ~{}", t);
                if let Some(r) = &parts.route   { s.push_str(&format!(" via {}", r)); }
                if let Some(c) = &parts.traffic { s.push_str(&format!(" ({})", c)); }
                s
            }
            None if !fallback.is_empty() => {
                format!("Commute: {}", &fallback[..fallback.len().min(120)].trim())
            }
            None => "Commute: data unavailable".to_string(),
        }
    }
}

impl Default for CommuteAdapter {
    fn default() -> Self { Self::new() }
}

struct CommuteParts {
    time:    Option<String>,
    traffic: Option<String>,
    route:   Option<String>,
}

#[async_trait]
impl SourceAdapter for CommuteAdapter {
    fn name(&self) -> &str { "commute" }

    fn is_configured(&self, settings: &UserSettings) -> bool {
        settings.commute_home.is_some() && settings.commute_work.is_some()
    }

    async fn fetch(&self, settings: &UserSettings) -> Result<SourceData> {
        let home = match &settings.commute_home {
            Some(h) => h.clone(),
            None => return Ok(SourceData::unavailable("commute")),
        };
        let work = match &settings.commute_work {
            Some(w) => w.clone(),
            None => return Ok(SourceData::unavailable("commute")),
        };

        if let Some(cached) = self.cache_get(&home, &work) {
            log::debug!("commute: cache hit {} → {}", home, work);
            return Ok(SourceData::new("commute", cached));
        }

        // DuckDuckGo search — requires LUMINA_DDG_URL to be configured.
        let searcher = match WebSearch::from_env() {
            Ok(s) => s,
            Err(e) => {
                log::warn!("commute: search unavailable ({})", e);
                return Ok(SourceData::unavailable("commute"));
            }
        };

        let query = format!("current traffic {} to {}", home, work);
        let results = match searcher.search(&query, SEARCH_RESULT_COUNT).await {
            Ok(r) if !r.is_empty() => r,
            Ok(_) => {
                log::debug!("commute: no results for '{}'", query);
                return Ok(SourceData::unavailable("commute"));
            }
            Err(e) => {
                log::warn!("commute: search failed: {}", e);
                return Ok(SourceData::unavailable("commute"));
            }
        };

        let top_snippet = &results[0].snippet;
        let parts = self.extract(top_snippet);

        let summary = if parts.time.is_some() {
            self.format(&parts, top_snippet)
        } else {
            // Fetch the full page for richer content.
            let page_text = WebClient::from_env()
                .ok()
                .and_then(|c| {
                    let url = results[0].url.clone();
                    // Use tokio::task::block_in_place would be wrong here; we're already async.
                    // Instead we'll just capture the future and let it run inline.
                    Some((c, url))
                })
                .map(|(client, url)| async move {
                    client.fetch(&url, "vigil").await.map(|p| p.content).unwrap_or_default()
                });

            let body = if let Some(fut) = page_text {
                fut.await
            } else {
                String::new()
            };

            if !body.is_empty() {
                let page_parts = self.extract(&body);
                if page_parts.time.is_some() {
                    self.format(&page_parts, &body)
                } else {
                    self.format(&parts, top_snippet)
                }
            } else {
                self.format(&parts, top_snippet)
            }
        };

        self.cache_set(&home, &work, summary.clone());
        Ok(SourceData::new("commute", summary))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn adapter() -> CommuteAdapter { CommuteAdapter::new() }

    // ── is_configured ─────────────────────────────────────────────────────

    #[test]
    fn test_configured_with_home_and_work() {
        let s = UserSettings {
            commute_home: Some("Home St".to_string()),
            commute_work: Some("Work Ave".to_string()),
            ..Default::default()
        };
        assert!(adapter().is_configured(&s));
    }

    #[test]
    fn test_not_configured_missing_home() {
        let s = UserSettings {
            commute_work: Some("Work Ave".to_string()),
            ..Default::default()
        };
        assert!(!adapter().is_configured(&s));
    }

    #[test]
    fn test_not_configured_missing_work() {
        let s = UserSettings {
            commute_home: Some("Home St".to_string()),
            ..Default::default()
        };
        assert!(!adapter().is_configured(&s));
    }

    #[test]
    fn test_not_configured_empty() {
        assert!(!adapter().is_configured(&UserSettings::default()));
    }

    #[test]
    fn test_configured_without_api_key_or_url() {
        let s = UserSettings {
            commute_home:    Some("123 Main St".to_string()),
            commute_work:    Some("456 Corp Dr".to_string()),
            commute_api_url: None,
            commute_api_key: None,
            ..Default::default()
        };
        assert!(adapter().is_configured(&s));
    }

    // ── extract ───────────────────────────────────────────────────────────

    #[test]
    fn test_extracts_minutes() {
        let p = adapter().extract("Travel time: 35 min via I-880");
        assert!(p.time.as_deref().map_or(false, |t| t.contains("35")));
    }

    #[test]
    fn test_extracts_hour_and_minutes() {
        let p = adapter().extract("About 1 hr 20 min due to heavy traffic");
        assert!(p.time.is_some());
    }

    #[test]
    fn test_extracts_traffic_condition() {
        let p = adapter().extract("35 min, moderate traffic expected today");
        assert!(p.traffic.as_deref().map_or(false, |t| t.contains("moderate")));
    }

    #[test]
    fn test_extracts_route_via() {
        let p = adapter().extract("30 min via I-880 North to downtown");
        assert!(p.route.as_ref().map_or(false, |r| r.contains("I-880")));
    }

    #[test]
    fn test_no_time_in_unrelated_text() {
        let p = adapter().extract("Breaking news: market closed today");
        assert!(p.time.is_none());
    }

    #[test]
    fn test_body_truncated_to_max_chars() {
        let long_body = "a".repeat(MAX_BODY_CHARS + 1_000);
        // Should not panic
        let _ = adapter().extract(&long_body);
    }

    // ── format ────────────────────────────────────────────────────────────

    #[test]
    fn test_format_time_only() {
        let a = adapter();
        let p = CommuteParts { time: Some("35 min".to_string()), traffic: None, route: None };
        assert_eq!(a.format(&p, ""), "Commute: ~35 min");
    }

    #[test]
    fn test_format_time_route_traffic() {
        let a = adapter();
        let p = CommuteParts {
            time:    Some("35 min".to_string()),
            route:   Some("I-880".to_string()),
            traffic: Some("moderate traffic".to_string()),
        };
        let s = a.format(&p, "");
        assert!(s.contains("35 min") && s.contains("I-880") && s.contains("moderate traffic"));
    }

    #[test]
    fn test_format_no_time_uses_snippet() {
        let a = adapter();
        let p = CommuteParts { time: None, traffic: None, route: None };
        let s = a.format(&p, "Traffic is clear on US-101");
        assert!(s.contains("Traffic is clear"));
    }

    #[test]
    fn test_format_snippet_capped_at_120_chars() {
        let a = adapter();
        let p = CommuteParts { time: None, traffic: None, route: None };
        let long_snip = "x".repeat(200);
        let s = a.format(&p, &long_snip);
        // "Commute: " (9) + 120 = 129 max
        assert!(s.len() <= 130);
    }

    #[test]
    fn test_format_no_time_no_snippet_unavailable() {
        let a = adapter();
        let p = CommuteParts { time: None, traffic: None, route: None };
        assert_eq!(a.format(&p, ""), "Commute: data unavailable");
    }

    // ── cache ─────────────────────────────────────────────────────────────

    #[test]
    fn test_cache_miss_on_new_key() {
        assert!(adapter().cache_get("home", "work").is_none());
    }

    #[test]
    fn test_cache_hit_after_set() {
        let a = adapter();
        a.cache_set("home", "work", "Commute: ~20 min".to_string());
        assert_eq!(a.cache_get("home", "work").as_deref(), Some("Commute: ~20 min"));
    }

    #[test]
    fn test_cache_independent_routes() {
        let a = adapter();
        a.cache_set("h1", "w1", "~20 min".to_string());
        a.cache_set("h2", "w2", "~45 min".to_string());
        assert_eq!(a.cache_get("h1", "w1").as_deref(), Some("~20 min"));
        assert_eq!(a.cache_get("h2", "w2").as_deref(), Some("~45 min"));
        assert!(a.cache_get("h1", "w2").is_none());
    }

    // ── fetch (unconfigured path) ─────────────────────────────────────────

    #[tokio::test]
    async fn test_fetch_unavailable_not_configured() {
        let r = adapter().fetch(&UserSettings::default()).await.unwrap();
        assert!(!r.available);
        assert_eq!(r.source, "commute");
    }

    #[tokio::test]
    #[serial]
    async fn test_fetch_unavailable_when_ddg_not_configured() {
        // LUMINA_DDG_URL not set → WebSearch::from_env() returns Err → unavailable
        std::env::remove_var("LUMINA_DDG_URL");
        let s = UserSettings {
            commute_home: Some("Home".to_string()),
            commute_work: Some("Work".to_string()),
            ..Default::default()
        };
        let r = adapter().fetch(&s).await.unwrap();
        assert!(!r.available, "should be unavailable when DDG URL not set");
    }
}
