//! P2-04: Per-user Vigil briefing configuration.
//!
//! Per-user settings that control when and how the morning briefing is generated
//! and delivered. All configuration is sourced from environment variables or
//! caller-supplied values — no hardcoded defaults beyond sensible schedule/source
//! strings.
//!
//! ## Environment variables
//!
//! | Variable                  | Default          | Description                             |
//! |---------------------------|------------------|-----------------------------------------|
//! | `VIGIL_SCHEDULE`          | `"0 7 * * *"`    | Cron expression for briefing delivery   |
//! | `VIGIL_SOURCES`           | `"weather,calendar,news,commute,tasks"` | Enabled sources  |
//! | `VIGIL_DETAIL`            | `"brief"`        | Detail level: `"brief"` or `"full"`     |
//! | `VIGIL_USER_NAME`         | `""`             | Display name used in the briefing prompt|
//! | `VIGIL_TIMEZONE`          | `"UTC"`          | User's timezone (IANA, e.g. `"America/Los_Angeles"`) |
//! | `VIGIL_LLM_MODEL`         | `"lumina-deep"`  | Model alias passed to the Chord request |
//! | `VIGIL_DELIVER_CHANNEL`   | `"matrix"`       | Delivery channel name                   |
//! | `VIGIL_BRIEFING_TIMEOUT_SECS` | `60`         | Max seconds to wait for LLM generation  |

// ── DetailLevel ───────────────────────────────────────────────────────────────

/// Controls the verbosity of the generated morning briefing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetailLevel {
    /// A compact 3–4-sentence overview.
    Brief,
    /// A detailed briefing with analysis and actionable suggestions.
    Full,
}

impl DetailLevel {
    /// Parse a string representation lossily (case-insensitive).
    ///
    /// Returns `DetailLevel::Brief` for any unrecognised value.
    ///
    /// Named `parse_lossy` rather than `from_str` to avoid shadowing the
    /// standard `std::str::FromStr` trait signature without actually
    /// implementing it (which would require a `Result` return type).
    pub fn parse_lossy(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "full" => DetailLevel::Full,
            _ => DetailLevel::Brief,
        }
    }

    /// Return the canonical string representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            DetailLevel::Brief => "brief",
            DetailLevel::Full  => "full",
        }
    }
}

// ── BriefingConfig ────────────────────────────────────────────────────────────

/// Per-user configuration for the morning briefing.
///
/// All fields have sensible defaults. Fields are intentionally public so
/// callers can construct a config directly or via [`BriefingConfig::from_env`].
#[derive(Debug, Clone)]
pub struct BriefingConfig {
    /// Cron expression controlling when the briefing fires.
    ///
    /// Default: `"0 7 * * *"` (07:00 daily).
    pub schedule: String,

    /// Comma-separated list of enabled source names.
    ///
    /// Recognised names: `weather`, `calendar`, `news`, `commute`, `tasks`.
    /// Default: `"weather,calendar,news,commute,tasks"`.
    pub sources: String,

    /// Verbosity level for the generated briefing.
    pub detail: DetailLevel,

    /// The user's display name, injected into the generation prompt.
    ///
    /// When empty, the prompt uses a generic greeting.
    pub user_name: String,

    /// IANA timezone name (e.g. `"America/Los_Angeles"`).
    ///
    /// Used in the generation prompt so the LLM can reference local times.
    pub timezone: String,

    /// Model alias passed to the Chord request.
    ///
    /// Default: `"lumina-deep"` (high-quality 120B class model).
    pub llm_model: String,

    /// Delivery channel name (e.g. `"matrix"`).
    pub deliver_channel: String,

    /// Maximum seconds to allow for LLM briefing generation before timing out
    /// and delivering a partial briefing from whatever data was collected.
    pub briefing_timeout_secs: u64,
}

impl Default for BriefingConfig {
    fn default() -> Self {
        Self {
            schedule:               "0 7 * * *".to_string(),
            sources:                "weather,calendar,news,commute,tasks".to_string(),
            detail:                 DetailLevel::Brief,
            user_name:              String::new(),
            timezone:               "UTC".to_string(),
            llm_model:              "lumina-deep".to_string(),
            deliver_channel:        "matrix".to_string(),
            briefing_timeout_secs:  60,
        }
    }
}

impl BriefingConfig {
    /// Load configuration from environment variables.
    ///
    /// Missing variables use their defaults; no variable is required.
    pub fn from_env() -> Self {
        use std::env;
        let mut cfg = Self::default();

        if let Ok(v) = env::var("VIGIL_SCHEDULE") {
            if !v.is_empty() { cfg.schedule = v; }
        }
        if let Ok(v) = env::var("VIGIL_SOURCES") {
            if !v.is_empty() { cfg.sources = v; }
        }
        if let Ok(v) = env::var("VIGIL_DETAIL") {
            if !v.is_empty() { cfg.detail = DetailLevel::parse_lossy(&v); }
        }
        if let Ok(v) = env::var("VIGIL_USER_NAME") {
            cfg.user_name = v; // empty string is intentional here
        }
        if let Ok(v) = env::var("VIGIL_TIMEZONE") {
            if !v.is_empty() { cfg.timezone = v; }
        }
        if let Ok(v) = env::var("VIGIL_LLM_MODEL") {
            if !v.is_empty() { cfg.llm_model = v; }
        }
        if let Ok(v) = env::var("VIGIL_DELIVER_CHANNEL") {
            if !v.is_empty() { cfg.deliver_channel = v; }
        }
        if let Ok(v) = env::var("VIGIL_BRIEFING_TIMEOUT_SECS") {
            if let Ok(n) = v.trim().parse::<u64>() {
                cfg.briefing_timeout_secs = n;
            }
        }

        cfg
    }

    /// Return the set of enabled source names as a `Vec<&str>`.
    ///
    /// Source names are trimmed and lowercased.  Empty tokens are dropped.
    pub fn enabled_sources(&self) -> Vec<String> {
        self.sources
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// Return `true` if `source_name` appears in the enabled sources list.
    pub fn source_enabled(&self, source_name: &str) -> bool {
        let needle = source_name.trim().to_lowercase();
        self.enabled_sources().contains(&needle)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::sync::Mutex;

    /// Serialise all tests that mutate process-global env vars.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    // ── DetailLevel ───────────────────────────────────────────────────────────

    #[test]
    fn test_detail_level_brief_parse_lossy() {
        assert_eq!(DetailLevel::parse_lossy("brief"), DetailLevel::Brief);
        assert_eq!(DetailLevel::parse_lossy("BRIEF"), DetailLevel::Brief);
        assert_eq!(DetailLevel::parse_lossy(""),       DetailLevel::Brief);
        assert_eq!(DetailLevel::parse_lossy("unknown"), DetailLevel::Brief);
    }

    #[test]
    fn test_detail_level_full_parse_lossy() {
        assert_eq!(DetailLevel::parse_lossy("full"), DetailLevel::Full);
        assert_eq!(DetailLevel::parse_lossy("FULL"), DetailLevel::Full);
        assert_eq!(DetailLevel::parse_lossy(" full "), DetailLevel::Full);
    }

    #[test]
    fn test_detail_level_as_str() {
        assert_eq!(DetailLevel::Brief.as_str(), "brief");
        assert_eq!(DetailLevel::Full.as_str(),  "full");
    }

    // ── BriefingConfig defaults ───────────────────────────────────────────────

    #[test]
    fn test_default_config_schedule() {
        let cfg = BriefingConfig::default();
        assert_eq!(cfg.schedule, "0 7 * * *");
    }

    #[test]
    fn test_default_config_sources_include_key_sources() {
        let cfg = BriefingConfig::default();
        let sources = cfg.enabled_sources();
        assert!(sources.contains(&"weather".to_string()));
        assert!(sources.contains(&"news".to_string()));
        assert!(sources.contains(&"commute".to_string()));
    }

    #[test]
    fn test_default_config_detail_is_brief() {
        assert_eq!(BriefingConfig::default().detail, DetailLevel::Brief);
    }

    #[test]
    fn test_default_config_llm_model() {
        assert_eq!(BriefingConfig::default().llm_model, "lumina-deep");
    }

    #[test]
    fn test_default_config_timezone_is_utc() {
        assert_eq!(BriefingConfig::default().timezone, "UTC");
    }

    #[test]
    fn test_default_config_timeout() {
        assert_eq!(BriefingConfig::default().briefing_timeout_secs, 60);
    }

    // ── enabled_sources ───────────────────────────────────────────────────────

    #[test]
    fn test_enabled_sources_parses_comma_list() {
        let cfg = BriefingConfig {
            sources: "weather,news,commute".to_string(),
            ..Default::default()
        };
        let sources = cfg.enabled_sources();
        assert_eq!(sources, vec!["weather", "news", "commute"]);
    }

    #[test]
    fn test_enabled_sources_trims_whitespace() {
        let cfg = BriefingConfig {
            sources: " weather , news , commute ".to_string(),
            ..Default::default()
        };
        let sources = cfg.enabled_sources();
        assert!(sources.contains(&"weather".to_string()));
        assert!(sources.contains(&"news".to_string()));
        assert!(sources.contains(&"commute".to_string()));
    }

    #[test]
    fn test_enabled_sources_drops_empty_tokens() {
        let cfg = BriefingConfig {
            sources: "weather,,news".to_string(),
            ..Default::default()
        };
        // The empty token between ",," should not appear.
        let sources = cfg.enabled_sources();
        assert!(!sources.contains(&"".to_string()));
        assert_eq!(sources.len(), 2);
    }

    #[test]
    fn test_enabled_sources_lowercases() {
        let cfg = BriefingConfig {
            sources: "Weather,NEWS".to_string(),
            ..Default::default()
        };
        let sources = cfg.enabled_sources();
        assert!(sources.contains(&"weather".to_string()));
        assert!(sources.contains(&"news".to_string()));
    }

    #[test]
    fn test_source_enabled_true() {
        let cfg = BriefingConfig {
            sources: "weather,news".to_string(),
            ..Default::default()
        };
        assert!(cfg.source_enabled("weather"));
        assert!(cfg.source_enabled("NEWS")); // case-insensitive
    }

    #[test]
    fn test_source_enabled_false() {
        let cfg = BriefingConfig {
            sources: "weather,news".to_string(),
            ..Default::default()
        };
        assert!(!cfg.source_enabled("commute"));
        assert!(!cfg.source_enabled("tasks"));
    }

    // ── from_env ─────────────────────────────────────────────────────────────

    #[test]
    #[serial]
    fn test_from_env_defaults_when_nothing_set() {
        let _g = ENV_LOCK.lock().unwrap();
        let keys = [
            "VIGIL_SCHEDULE", "VIGIL_SOURCES", "VIGIL_DETAIL", "VIGIL_USER_NAME",
            "VIGIL_TIMEZONE", "VIGIL_LLM_MODEL", "VIGIL_DELIVER_CHANNEL",
            "VIGIL_BRIEFING_TIMEOUT_SECS",
        ];
        for k in &keys { std::env::remove_var(k); }

        let cfg = BriefingConfig::from_env();
        assert_eq!(cfg.schedule, "0 7 * * *");
        assert_eq!(cfg.detail, DetailLevel::Brief);
        assert_eq!(cfg.timezone, "UTC");
        assert_eq!(cfg.llm_model, "lumina-deep");

        for k in &keys { std::env::remove_var(k); }
    }

    #[test]
    #[serial]
    fn test_from_env_reads_schedule() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("VIGIL_SCHEDULE", "0 8 * * *");
        let cfg = BriefingConfig::from_env();
        assert_eq!(cfg.schedule, "0 8 * * *");
        std::env::remove_var("VIGIL_SCHEDULE");
    }

    #[test]
    #[serial]
    fn test_from_env_reads_detail_full() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("VIGIL_DETAIL", "full");
        let cfg = BriefingConfig::from_env();
        assert_eq!(cfg.detail, DetailLevel::Full);
        std::env::remove_var("VIGIL_DETAIL");
    }

    #[test]
    #[serial]
    fn test_from_env_reads_timeout() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("VIGIL_BRIEFING_TIMEOUT_SECS", "120");
        let cfg = BriefingConfig::from_env();
        assert_eq!(cfg.briefing_timeout_secs, 120);
        std::env::remove_var("VIGIL_BRIEFING_TIMEOUT_SECS");
    }

    #[test]
    #[serial]
    fn test_from_env_ignores_invalid_timeout() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("VIGIL_BRIEFING_TIMEOUT_SECS", "not_a_number");
        let cfg = BriefingConfig::from_env();
        // Invalid value → falls back to default
        assert_eq!(cfg.briefing_timeout_secs, 60);
        std::env::remove_var("VIGIL_BRIEFING_TIMEOUT_SECS");
    }
}
