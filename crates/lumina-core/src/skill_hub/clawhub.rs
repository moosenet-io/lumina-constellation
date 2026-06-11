//! WEB-07: ClawHub API adapter
//!
//! Provides HTTP-based discovery of skills published on the ClawHub registry.
//! This module is **read-only**: it can search and inspect skill metadata, but
//! it never downloads or installs skill bytecode.
//!
//! # URL configuration
//! The base URL is read from the `LUMINA_CLAWHUB_URL` environment variable.
//! No URL is hardcoded in this module. If the variable is unset the adapter
//! returns an error on every call.
//!
//! # Security
//! All text fields returned by the registry (name, description, author …) are
//! passed through [`crate::input_guard::guard_input`] before they are handed
//! to callers. This prevents malicious skill descriptions from injecting
//! prompts into the Lumina context window.

use crate::error::{LuminaError, Result};
use crate::input_guard::guard_input;
use crate::egress_inspector::EgressInspector;
use super::{SafetyLevel, SkillInfo};
use serde::Deserialize;

// ── Raw API response types ────────────────────────────────────────────────────

/// Raw JSON shape returned by `GET /api/v1/skills/search?q=…`
#[derive(Debug, Deserialize)]
pub(super) struct RawSearchResponse {
    pub results: Vec<RawSkillEntry>,
}

/// Raw JSON shape returned by `GET /api/v1/skills/{name}`
#[derive(Debug, Deserialize)]
pub(super) struct RawSkillEntry {
    pub name: String,
    pub description: String,
    pub author: String,
    pub downloads: u64,
    pub stars: f32,
    pub age_days: u32,
    pub capabilities: Vec<String>,
    /// Whether this entry has been flagged by the registry moderators.
    #[serde(default)]
    pub flagged: bool,
}

// ── ClawHubAdapter ────────────────────────────────────────────────────────────

/// HTTP adapter for the ClawHub skill registry.
///
/// Instantiate once and share via `Arc`; all methods take `&self`.
pub struct ClawHubAdapter {
    client: reqwest::Client,
    /// Base URL, e.g. `https://clawhub.example.com` (no trailing slash).
    /// Loaded from `LUMINA_CLAWHUB_URL` at construction time.
    base_url: String,
    /// Egress inspector — validates that ClawHub base URL is allowlisted
    /// before every outbound HTTP request (AC5).
    egress: EgressInspector,
}

impl ClawHubAdapter {
    /// Create a new adapter.
    ///
    /// Reads `LUMINA_CLAWHUB_URL` from the environment.
    /// Returns an error if the variable is absent or empty.
    pub fn from_env() -> Result<Self> {
        let base_url = std::env::var("LUMINA_CLAWHUB_URL")
            .map_err(|_| {
                LuminaError::Config(
                    "LUMINA_CLAWHUB_URL is not set — ClawHub adapter requires an explicit URL".to_string(),
                )
            })?;
        let base_url = base_url.trim_end_matches('/').to_string();
        if base_url.is_empty() {
            return Err(LuminaError::Config(
                "LUMINA_CLAWHUB_URL is set but empty".to_string(),
            ));
        }
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| LuminaError::Network(e))?;
        let egress = EgressInspector::from_env();
        Ok(Self { client, base_url, egress })
    }

    /// Create an adapter with an explicit base URL (useful in tests).
    ///
    /// Uses the environment egress allowlist via [`EgressInspector::from_env`].
    pub fn with_url(base_url: impl Into<String>) -> Result<Self> {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        if base_url.is_empty() {
            return Err(LuminaError::Config("base_url must not be empty".to_string()));
        }
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| LuminaError::Network(e))?;
        let egress = EgressInspector::from_env();
        Ok(Self { client, base_url, egress })
    }

    /// Create an adapter with an explicit base URL and a custom egress inspector.
    ///
    /// Used in tests to inject an inspector that permits the mock server URL.
    #[cfg(test)]
    pub fn with_url_and_egress(base_url: impl Into<String>, egress: EgressInspector) -> Result<Self> {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        if base_url.is_empty() {
            return Err(LuminaError::Config("base_url must not be empty".to_string()));
        }
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| LuminaError::Network(e))?;
        Ok(Self { client, base_url, egress })
    }

    /// Search for skills matching `query`.
    ///
    /// Calls `GET {base_url}/api/v1/skills/search?q={query}`.
    /// All text content from the registry is sanitised through [`guard_input`].
    ///
    /// The egress inspector validates the ClawHub domain before the request
    /// is sent (AC5).
    pub async fn search(&self, query: &str) -> Result<Vec<SkillInfo>> {
        let url = format!("{}/api/v1/skills/search", self.base_url);
        // AC5: validate egress before making any outbound HTTP request.
        self.egress.inspect(&url, "skill_search")
            .map_err(LuminaError::from)?;
        let resp = self
            .client
            .get(&url)
            .query(&[("q", query)])
            .send()
            .await
            .map_err(LuminaError::Network)?;

        if !resp.status().is_success() {
            return Err(LuminaError::Network(
                resp.error_for_status().unwrap_err(),
            ));
        }

        let raw: RawSearchResponse = resp.json().await.map_err(LuminaError::Network)?;
        raw.results
            .into_iter()
            .map(|entry| self.sanitise_and_assess(entry))
            .collect()
    }

    /// Fetch full details for a single skill by name.
    ///
    /// Calls `GET {base_url}/api/v1/skills/{encoded_name}` where `encoded_name`
    /// is percent-encoded to prevent path traversal (e.g. `..`, `/` in the name
    /// cannot escape the `/api/v1/skills/` path prefix).
    ///
    /// The egress inspector validates the ClawHub domain before the request
    /// is sent (AC5).
    pub async fn get_skill_details(&self, name: &str) -> Result<SkillInfo> {
        // Percent-encode the skill name to prevent path traversal.
        // This ensures characters like '/', '..', and '%2F' in `name` cannot
        // traverse URL path segments.
        let encoded_name = percent_encode_path_segment(name);
        let url = format!("{}/api/v1/skills/{}", self.base_url, encoded_name);
        // AC5: validate egress before making any outbound HTTP request.
        self.egress.inspect(&url, "skill_search")
            .map_err(LuminaError::from)?;
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(LuminaError::Network)?;

        if !resp.status().is_success() {
            return Err(LuminaError::Network(
                resp.error_for_status().unwrap_err(),
            ));
        }

        let raw: RawSkillEntry = resp.json().await.map_err(LuminaError::Network)?;
        self.sanitise_and_assess(raw)
    }

    /// Assess safety of a raw entry and convert to [`SkillInfo`].
    ///
    /// Text fields pass through [`guard_input`] before being stored.
    pub(super) fn sanitise_and_assess(&self, raw: RawSkillEntry) -> Result<SkillInfo> {
        // Sanitise all free-text fields from the registry.
        let name = guard_input(&raw.name)
            .map(|z| z.as_str().to_owned())
            .unwrap_or_else(|_| "[sanitised]".to_string());
        let description = guard_input(&raw.description)
            .map(|z| z.as_str().to_owned())
            .unwrap_or_else(|_| "[sanitised]".to_string());
        let author = guard_input(&raw.author)
            .map(|z| z.as_str().to_owned())
            .unwrap_or_else(|_| "[sanitised]".to_string());

        let capabilities: Vec<String> = raw
            .capabilities
            .iter()
            .map(|c| {
                guard_input(c)
                    .map(|z| z.as_str().to_owned())
                    .unwrap_or_else(|_| "[sanitised]".to_string())
            })
            .collect();

        let safety = assess_safety(raw.downloads, raw.age_days, raw.flagged, &capabilities);

        Ok(SkillInfo {
            name,
            description,
            author,
            downloads: raw.downloads,
            stars: raw.stars,
            age_days: raw.age_days,
            capabilities,
            safety,
        })
    }
}

// ── URL helpers ───────────────────────────────────────────────────────────────

/// Percent-encode a string for use as a single URL path segment.
///
/// Encodes all characters that are not URL path-segment safe. In particular
/// `/`, `?`, `#`, `%`, and ASCII control characters are encoded, which prevents
/// a skill name like `foo/../admin` or `foo%2Fadmin` from traversing the URL
/// path hierarchy.
fn percent_encode_path_segment(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        match byte {
            // Unreserved characters per RFC 3986 §2.3 — safe to pass through.
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9'
            | b'-' | b'_' | b'.' | b'~' => {
                output.push(*byte as char);
            }
            // Everything else (including '/', '?', '#', '%', whitespace …) is encoded.
            b => {
                output.push('%');
                output.push(HEX_CHARS[(b >> 4) as usize] as char);
                output.push(HEX_CHARS[(b & 0xF) as usize] as char);
            }
        }
    }
    output
}

/// Hex digit lookup table for percent-encoding.
const HEX_CHARS: &[u8; 16] = b"0123456789ABCDEF";

// ── Safety assessment (free function so it can be tested independently) ───────

/// Determine the safety level of a skill based on its registry metadata.
///
/// Rules applied in order (first match wins):
///
/// **Dangerous** if ANY of:
/// - Requests all permissions (capability string `"all"` or `"*"`)
/// - Requests unrestricted network access: `Network(any)`, `Network(*)`
/// - Requests read-write filesystem access: `Filesystem(rw)`, `Filesystem(write)`, `Filesystem(*)`
/// - Requests broad environment write access: `Env(write)`, `Env(all)` as a Dangerous cap (in this tier),
///   `Env(*)`, or any literal `"env"` with write access
/// - Fewer than 100 downloads
/// - Marked as flagged by the registry
///
/// **Caution** if ANY of:
/// - Fewer than 7 days old (`age_days < 7`)
/// - Fewer than 1 000 downloads
/// - Requests `Env(all)` or `Env(*)` (broad environment variable access)
///   [escalates to Dangerous when matched in the Dangerous tier above]
///
/// **Safe** otherwise.
pub(super) fn assess_safety(
    downloads: u64,
    age_days: u32,
    flagged: bool,
    capabilities: &[String],
) -> SafetyLevel {
    // Check for "all permissions" capability: bare "all" or "*".
    let requests_all = capabilities.iter().any(|c| {
        let lc = c.to_lowercase();
        lc == "all" || lc == "*"
    });

    // Check for unrestricted network access.
    // Matches: "Network(any)", "Network(*)", "network(any)", "network(*)"
    let requests_network_any = capabilities.iter().any(|c| {
        let lc = c.to_lowercase();
        lc == "network(any)" || lc == "network(*)"
    });

    // Check for read-write or unrestricted filesystem access.
    // Matches: "Filesystem(rw)", "Filesystem(write)", "Filesystem(*)",
    //          "filesystem(rw)", "filesystem(write)", "filesystem(*)"
    let requests_filesystem_rw = capabilities.iter().any(|c| {
        let lc = c.to_lowercase();
        lc == "filesystem(rw)" || lc == "filesystem(write)" || lc == "filesystem(*)"
    });

    // Check for broad environment-variable write access.
    // Matches: "Env(write)", "Env(all)", "Env(*)", "env(write)", "env(all)", "env(*)"
    // These are Dangerous (not merely Caution) because write access to env vars can
    // expose secrets or hijack process configuration.
    let requests_env_dangerous = capabilities.iter().any(|c| {
        let lc = c.to_lowercase();
        lc == "env(write)" || lc == "env(all)" || lc == "env(*)"
    });

    // Check for broad env access (Caution tier — less broad than write, e.g. read-all).
    // Any capability that starts with "env(" but isn't already in the Dangerous set.
    let requests_env_all = capabilities.iter().any(|c| {
        let lc = c.to_lowercase();
        lc == "env(all)" || lc == "env(*)"
    });

    // Dangerous tier (highest risk, evaluated first).
    if requests_all
        || requests_network_any
        || requests_filesystem_rw
        || requests_env_dangerous
        || downloads < 100
        || flagged
    {
        return SafetyLevel::Dangerous;
    }

    // Caution tier.
    if age_days < 7 || downloads < 1_000 || requests_env_all {
        return SafetyLevel::Caution;
    }

    SafetyLevel::Safe
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::sync::Mutex;

    // Serialise tests that mutate env vars so they don't race each other.
    // Mirrors the pattern used in egress_inspector tests.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    // ── assess_safety unit tests ──────────────────────────────────────────────

    #[test]
    fn test_safety_assessment_dangerous_all_permissions() {
        // Requesting "all" permissions → Dangerous regardless of other factors.
        let level = assess_safety(50_000, 365, false, &["all".to_string()]);
        assert!(matches!(level, SafetyLevel::Dangerous));

        // Wildcard "*" also triggers Dangerous.
        let level = assess_safety(50_000, 365, false, &["*".to_string()]);
        assert!(matches!(level, SafetyLevel::Dangerous));
    }

    #[test]
    fn test_safety_assessment_dangerous_low_downloads() {
        // < 100 downloads → Dangerous.
        let level = assess_safety(99, 30, false, &["network".to_string()]);
        assert!(matches!(level, SafetyLevel::Dangerous));

        // Exactly 0 downloads.
        let level = assess_safety(0, 30, false, &[]);
        assert!(matches!(level, SafetyLevel::Dangerous));
    }

    #[test]
    fn test_safety_assessment_dangerous_flagged() {
        // Flagged by registry → Dangerous even with many downloads.
        let level = assess_safety(100_000, 365, true, &[]);
        assert!(matches!(level, SafetyLevel::Dangerous));
    }

    #[test]
    fn test_safety_assessment_caution_low_downloads() {
        // Between 100 and 999 downloads → Caution.
        let level = assess_safety(500, 30, false, &["network".to_string()]);
        assert!(matches!(level, SafetyLevel::Caution));

        // Exactly 100 downloads (≥100 but <1000) → Caution.
        let level = assess_safety(100, 30, false, &[]);
        assert!(matches!(level, SafetyLevel::Caution));
    }

    #[test]
    fn test_safety_assessment_caution_new_skill() {
        // age_days < 7 → Caution even with many downloads.
        let level = assess_safety(10_000, 6, false, &[]);
        assert!(matches!(level, SafetyLevel::Caution));

        // age_days == 0 → Caution.
        let level = assess_safety(10_000, 0, false, &[]);
        assert!(matches!(level, SafetyLevel::Caution));
    }

    #[test]
    fn test_safety_assessment_dangerous_env_all() {
        // Env(all) now escalates to Dangerous because write-level env access can
        // expose secrets or hijack process configuration (issue #6 fix).
        let level = assess_safety(5_000, 30, false, &["env(all)".to_string()]);
        assert!(matches!(level, SafetyLevel::Dangerous));

        // Case-insensitive.
        let level = assess_safety(5_000, 30, false, &["ENV(ALL)".to_string()]);
        assert!(matches!(level, SafetyLevel::Dangerous));

        // Env(*) is also Dangerous.
        let level = assess_safety(5_000, 30, false, &["env(*)".to_string()]);
        assert!(matches!(level, SafetyLevel::Dangerous));

        // Env(write) is Dangerous.
        let level = assess_safety(5_000, 30, false, &["Env(write)".to_string()]);
        assert!(matches!(level, SafetyLevel::Dangerous));
    }

    #[test]
    fn test_safety_assessment_dangerous_network_any() {
        // Network(any) → Dangerous (issue #6 fix).
        let level = assess_safety(50_000, 365, false, &["Network(any)".to_string()]);
        assert!(matches!(level, SafetyLevel::Dangerous));

        // Network(*) → Dangerous.
        let level = assess_safety(50_000, 365, false, &["network(*)".to_string()]);
        assert!(matches!(level, SafetyLevel::Dangerous));
    }

    #[test]
    fn test_safety_assessment_dangerous_filesystem_rw() {
        // Filesystem(rw) → Dangerous (issue #6 fix).
        let level = assess_safety(50_000, 365, false, &["Filesystem(rw)".to_string()]);
        assert!(matches!(level, SafetyLevel::Dangerous));

        // Filesystem(write) → Dangerous.
        let level = assess_safety(50_000, 365, false, &["filesystem(write)".to_string()]);
        assert!(matches!(level, SafetyLevel::Dangerous));

        // Filesystem(*) → Dangerous.
        let level = assess_safety(50_000, 365, false, &["Filesystem(*)".to_string()]);
        assert!(matches!(level, SafetyLevel::Dangerous));
    }

    #[test]
    fn test_safety_assessment_safe_limited_network() {
        // Bare "network" without (any)/(rw) qualifier is still allowed as Safe
        // when other conditions are met.
        let level = assess_safety(50_000, 365, false, &["network".to_string()]);
        assert!(matches!(level, SafetyLevel::Safe));
    }

    #[test]
    fn test_safety_assessment_safe_vetted_skill() {
        // Well-established skill: many downloads, old, not flagged, limited capabilities.
        let level = assess_safety(50_000, 365, false, &["network".to_string(), "stdout".to_string()]);
        assert!(matches!(level, SafetyLevel::Safe));

        // Minimum thresholds for Safe: exactly 1000 downloads, exactly 7 days old.
        let level = assess_safety(1_000, 7, false, &[]);
        assert!(matches!(level, SafetyLevel::Safe));
    }

    // ── URL construction tests ────────────────────────────────────────────────

    #[test]
    fn test_search_url_construction() {
        // Verify that with_url normalises trailing slashes and produces the
        // correct search endpoint path. We use a mock server for full HTTP
        // coverage in integration tests; here we just check construction.
        let adapter = ClawHubAdapter::with_url("https://clawhub.example.com").unwrap();
        assert_eq!(adapter.base_url, "https://clawhub.example.com");

        // Trailing slash is stripped.
        let adapter = ClawHubAdapter::with_url("https://clawhub.example.com/").unwrap();
        assert_eq!(adapter.base_url, "https://clawhub.example.com");

        // Multiple trailing slashes.
        let adapter = ClawHubAdapter::with_url("https://clawhub.example.com///").unwrap();
        assert_eq!(adapter.base_url, "https://clawhub.example.com");
    }

    #[test]
    #[serial]
    fn test_from_env_requires_env_var() {
        // Hold the env lock for the duration of this test to prevent races
        // with any other test that reads or writes LUMINA_CLAWHUB_URL.
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        // Temporarily unset the env var.
        let prev = std::env::var("LUMINA_CLAWHUB_URL").ok();
        std::env::remove_var("LUMINA_CLAWHUB_URL");

        let result = ClawHubAdapter::from_env();
        assert!(result.is_err(), "Should fail when LUMINA_CLAWHUB_URL is not set");

        // Restore previous value.
        if let Some(val) = prev {
            std::env::set_var("LUMINA_CLAWHUB_URL", val);
        }
    }

    // ── percent_encode_path_segment ───────────────────────────────────────────

    #[test]
    fn test_percent_encode_safe_chars() {
        // Unreserved characters must pass through unencoded.
        assert_eq!(percent_encode_path_segment("file-watcher"), "file-watcher");
        assert_eq!(percent_encode_path_segment("my_skill.v2~"), "my_skill.v2~");
        assert_eq!(percent_encode_path_segment("ABC123"), "ABC123");
    }

    #[test]
    fn test_percent_encode_path_traversal_blocked() {
        // '/' must be encoded to prevent path traversal.
        // "foo/../admin" → "foo%2F..%2Fadmin"; the encoded slashes prevent the
        // path from being normalised by an HTTP server or proxy into "/api/v1/skills/admin".
        let encoded = percent_encode_path_segment("foo/../admin");
        assert!(!encoded.contains('/'), "Slash must be percent-encoded: {}", encoded);
        assert!(encoded.contains("%2F"), "Slash should be %2F-encoded: {}", encoded);

        // A name that is purely ".." — the dots are unreserved chars (RFC3986 §2.3)
        // and are left unencoded. This is safe because there is no slash to separate
        // path segments; the entire string is sent as a single literal segment.
        let encoded = percent_encode_path_segment("..");
        // The slash-encoding guarantee means no injected slashes.
        assert!(!encoded.contains('/'), "Slash must not appear: {}", encoded);
    }

    #[test]
    fn test_percent_encode_percent_itself_encoded() {
        // A pre-encoded sequence in the name (e.g. '%2F') must have the '%' re-encoded,
        // preventing double-decode path traversal.
        let encoded = percent_encode_path_segment("foo%2Fadmin");
        assert!(encoded.starts_with("foo%25"), "Percent must be encoded: {}", encoded);
    }

    #[test]
    fn test_percent_encode_spaces_and_special_chars() {
        let encoded = percent_encode_path_segment("hello world");
        assert_eq!(encoded, "hello%20world");

        let encoded = percent_encode_path_segment("a?b#c");
        assert!(!encoded.contains('?'), "? must be encoded");
        assert!(!encoded.contains('#'), "# must be encoded");
    }

    // ── Egress inspector integration ─────────────────────────────────────────

    #[tokio::test]
    async fn test_egress_inspector_blocks_disallowed_domain() {
        use crate::egress_inspector::EgressInspector;

        // Create an inspector that only permits localhost — not the mock target.
        let egress = EgressInspector::new(vec!["localhost".to_string(), "127.0.0.1".to_string()]);
        // Point the adapter at a non-allowlisted host.
        let adapter = ClawHubAdapter::with_url_and_egress(
            "https://evil.example.com",
            egress,
        ).unwrap();

        // Both search and get_skill_details should be blocked before making any HTTP request.
        let search_result = adapter.search("anything").await;
        assert!(search_result.is_err(), "search should be blocked by egress inspector");

        let details_result = adapter.get_skill_details("any-skill").await;
        assert!(details_result.is_err(), "get_skill_details should be blocked by egress inspector");
    }
}
