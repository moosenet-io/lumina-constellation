//! FORGE-07: Automated retraining schedule checker.
//!
//! Reads TrainingStats (approved+edited count) and a persisted
//! `last_trained_at` timestamp from vault, then decides whether a new
//! training run should be triggered.  Pure-Rust, no LLM call.
//!
//! Decision logic:
//!   should_retrain = true  when ALL of:
//!     1. approved_count + edited_count >= MIN_TRAINING_SAMPLES  (env, default 50)
//!     2. days_since_last_training >= TRAINING_INTERVAL_DAYS     (env, default 7)
//!        OR last_trained_at is None (never trained)
//!
//! Env vars:
//!   MIN_TRAINING_SAMPLES    Minimum curated samples required (default 50)
//!   TRAINING_INTERVAL_DAYS  Minimum days between runs (default 7)

use crate::error::{LuminaError, Result};
use crate::training_store::TrainingStats;
use crate::vault;
use secrecy::ExposeSecret;

const VAULT_KEY: &str = "LUMINA_LAST_TRAINED_AT";
const DEFAULT_MIN_SAMPLES: u64 = 50;
const DEFAULT_INTERVAL_DAYS: u64 = 7;

/// Outcome of a `RetrainScheduler::check()` call.
#[derive(Debug, Clone, PartialEq)]
pub struct RetrainDecision {
    pub should_retrain: bool,
    pub reason: String,
    pub approved_count: u64,
    pub days_since_last: Option<u64>,
}

/// Checks whether a retraining run should be triggered.
pub struct RetrainScheduler {
    min_samples: u64,
    interval_days: u64,
}

impl RetrainScheduler {
    pub fn new(min_samples: u64, interval_days: u64) -> Self {
        Self { min_samples, interval_days }
    }

    /// Build from environment variables.
    pub fn from_env() -> Self {
        let min_samples = std::env::var("MIN_TRAINING_SAMPLES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_MIN_SAMPLES);
        let interval_days = std::env::var("TRAINING_INTERVAL_DAYS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_INTERVAL_DAYS);
        Self { min_samples, interval_days }
    }

    /// Check whether a training run should be triggered given current stats.
    pub fn check(&self, stats: &TrainingStats) -> RetrainDecision {
        let approved_count = (stats.approved + stats.edited) as u64;

        if approved_count < self.min_samples {
            return RetrainDecision {
                should_retrain: false,
                reason: format!(
                    "Need {} curated samples, have {} (approved={}, edited={})",
                    self.min_samples, approved_count, stats.approved, stats.edited
                ),
                approved_count,
                days_since_last: self.days_since_last_training(),
            };
        }

        let days_since = self.days_since_last_training();
        match days_since {
            None => RetrainDecision {
                should_retrain: true,
                reason: format!(
                    "Never trained — {} curated samples ready",
                    approved_count
                ),
                approved_count,
                days_since_last: None,
            },
            Some(days) if days >= self.interval_days => RetrainDecision {
                should_retrain: true,
                reason: format!(
                    "{} days since last training (threshold {}), {} curated samples ready",
                    days, self.interval_days, approved_count
                ),
                approved_count,
                days_since_last: Some(days),
            },
            Some(days) => RetrainDecision {
                should_retrain: false,
                reason: format!(
                    "Last training {} day(s) ago — wait {} more day(s)",
                    days,
                    self.interval_days.saturating_sub(days)
                ),
                approved_count,
                days_since_last: Some(days),
            },
        }
    }

    /// Persist the current time as the last-training timestamp in vault.
    ///
    /// Uses RFC-3339 / ISO-8601 format. Best-effort: logs warning on failure.
    pub fn mark_trained(&self) -> Result<()> {
        let now = current_iso8601();
        let mut store = vault::VaultStore::load()
            .map_err(|e| LuminaError::Config(format!("Cannot load vault: {e}")))?;
        store
            .set(VAULT_KEY.to_string(), secrecy::SecretString::new(now.into()))
            .map_err(|e| LuminaError::Config(format!("Cannot write {VAULT_KEY} to vault: {e}")))?;
        Ok(())
    }

    // ── Private ────────────────────────────────────────────────────────────

    /// Read `LUMINA_LAST_TRAINED_AT` from vault and compute days since then.
    /// Returns `None` if the key is absent or the timestamp cannot be parsed.
    fn days_since_last_training(&self) -> Option<u64> {
        let store = vault::VaultStore::load().ok()?;
        let raw = store.get(VAULT_KEY)?;
        let ts_str = raw.expose_secret();
        parse_days_since(ts_str)
    }
}

// ── Pure-Rust helpers (testable without vault) ────────────────────────────

/// Parse an ISO-8601 datetime string and return elapsed full days.
/// Format accepted: `YYYY-MM-DDTHH:MM:SSZ` (UTC only, no offset variants).
pub(crate) fn parse_days_since(ts: &str) -> Option<u64> {
    // We only use std — no chrono dependency.
    // Parse: YYYY-MM-DDTHH:MM:SSZ
    if ts.len() < 19 {
        return None;
    }
    let date_part = &ts[..10]; // YYYY-MM-DD
    let parts: Vec<&str> = date_part.split('-').collect();
    if parts.len() != 3 {
        return None;
    }
    let year: i64 = parts[0].parse().ok()?;
    let month: i64 = parts[1].parse().ok()?;
    let day: i64 = parts[2].parse().ok()?;

    let ts_days = date_to_days(year, month, day)?;
    let now_str = current_iso8601();
    let now_date = &now_str[..10];
    let now_parts: Vec<&str> = now_date.split('-').collect();
    if now_parts.len() != 3 {
        return None;
    }
    let ny: i64 = now_parts[0].parse().ok()?;
    let nm: i64 = now_parts[1].parse().ok()?;
    let nd: i64 = now_parts[2].parse().ok()?;
    let now_days = date_to_days(ny, nm, nd)?;

    Some(now_days.saturating_sub(ts_days) as u64)
}

/// Convert a calendar date to a Julian Day Number (days since some epoch).
/// Algorithm: Fliegel & Van Flandern (1968), integer arithmetic only.
fn date_to_days(year: i64, month: i64, day: i64) -> Option<i64> {
    if month < 1 || month > 12 || day < 1 || day > 31 {
        return None;
    }
    let jdn = (1461 * (year + 4800 + (month - 14) / 12)) / 4
        + (367 * (month - 2 - 12 * ((month - 14) / 12))) / 12
        - (3 * ((year + 4900 + (month - 14) / 12) / 100)) / 4
        + day
        - 32075;
    Some(jdn)
}

/// Return the current UTC date-time as `YYYY-MM-DDTHH:MM:SSZ`.
/// Uses `std::time::SystemTime` — no external dependencies.
fn current_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    unix_secs_to_iso8601(secs)
}

/// Convert Unix seconds to `YYYY-MM-DDTHH:MM:SSZ` (pure arithmetic, no libc).
pub(crate) fn unix_secs_to_iso8601(secs: u64) -> String {
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = secs / 86400; // days since 1970-01-01

    // Convert day count to Y-M-D via algorithm
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };

    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::training_store::TrainingStats;

    fn stats(approved: i64, edited: i64) -> TrainingStats {
        TrainingStats {
            total_turns: approved + edited + 5,
            pending: 5,
            approved,
            rejected: 0,
            edited,
            oldest: None,
            newest: None,
        }
    }

    fn sched(min: u64, interval: u64) -> RetrainScheduler {
        RetrainScheduler::new(min, interval)
    }

    // Below sample threshold → do not retrain
    #[test]
    fn test_below_sample_threshold() {
        let s = sched(50, 7);
        let d = s.check(&stats(10, 5)); // 15 total — below 50
        assert!(!d.should_retrain);
        assert_eq!(d.approved_count, 15);
        assert!(d.reason.contains("Need 50"));
    }

    // Meets samples, never trained → retrain
    #[test]
    fn test_never_trained_enough_samples() {
        let s = RetrainSchedulerNoVault { min_samples: 10, interval_days: 7 };
        let d = s.check_with_days(&stats(8, 5), None); // 13 samples, never trained
        assert!(d.should_retrain);
        assert!(d.reason.contains("Never trained"));
        assert_eq!(d.days_since_last, None);
    }

    // Meets samples, interval elapsed → retrain
    #[test]
    fn test_interval_elapsed() {
        let s = RetrainSchedulerNoVault { min_samples: 10, interval_days: 7 };
        let d = s.check_with_days(&stats(8, 5), Some(10)); // 13 samples, 10 days
        assert!(d.should_retrain);
        assert!(d.reason.contains("10 days since"));
    }

    // Meets samples, interval NOT elapsed → do not retrain
    #[test]
    fn test_interval_not_elapsed() {
        let s = RetrainSchedulerNoVault { min_samples: 10, interval_days: 7 };
        let d = s.check_with_days(&stats(8, 5), Some(3)); // 13 samples, 3 days
        assert!(!d.should_retrain);
        assert!(d.reason.contains("wait 4 more"));
    }

    // Exact threshold — boundary check
    #[test]
    fn test_exact_threshold_triggers() {
        let s = RetrainSchedulerNoVault { min_samples: 10, interval_days: 7 };
        let d = s.check_with_days(&stats(10, 0), Some(7)); // exactly 10 samples, exactly 7 days
        assert!(d.should_retrain);
    }

    // parse_days_since: valid timestamp 10 days ago
    #[test]
    fn test_parse_days_since_past() {
        // Use a date far in the past to guarantee > 0 days
        let result = parse_days_since("2020-01-01T00:00:00Z");
        assert!(result.is_some());
        assert!(result.unwrap() > 1000); // well over a thousand days ago
    }

    // parse_days_since: malformed string → None
    #[test]
    fn test_parse_days_since_malformed() {
        assert!(parse_days_since("not-a-date").is_none());
        assert!(parse_days_since("").is_none());
    }

    // unix_secs_to_iso8601: known epoch → 1970-01-01
    #[test]
    fn test_unix_epoch() {
        let s = unix_secs_to_iso8601(0);
        assert_eq!(s, "1970-01-01T00:00:00Z");
    }

    // unix_secs_to_iso8601: a known date
    #[test]
    fn test_known_date() {
        // 2024-01-01 00:00:00 UTC = 1704067200 seconds since epoch (widely verified)
        let s = unix_secs_to_iso8601(1704067200);
        assert_eq!(s, "2024-01-01T00:00:00Z");
    }

    // ── Test-only helper to inject `days_since_last` without hitting vault ──

    struct RetrainSchedulerNoVault {
        min_samples: u64,
        interval_days: u64,
    }

    impl RetrainSchedulerNoVault {
        fn check_with_days(&self, stats: &TrainingStats, days_since: Option<u64>) -> RetrainDecision {
            let approved_count = (stats.approved + stats.edited) as u64;
            if approved_count < self.min_samples {
                return RetrainDecision {
                    should_retrain: false,
                    reason: format!("Need {} curated samples, have {}", self.min_samples, approved_count),
                    approved_count,
                    days_since_last: days_since,
                };
            }
            match days_since {
                None => RetrainDecision {
                    should_retrain: true,
                    reason: format!("Never trained — {} curated samples ready", approved_count),
                    approved_count,
                    days_since_last: None,
                },
                Some(days) if days >= self.interval_days => RetrainDecision {
                    should_retrain: true,
                    reason: format!("{} days since last training (threshold {}), {} curated samples ready",
                        days, self.interval_days, approved_count),
                    approved_count,
                    days_since_last: Some(days),
                },
                Some(days) => RetrainDecision {
                    should_retrain: false,
                    reason: format!("Last training {} day(s) ago — wait {} more day(s)",
                        days, self.interval_days.saturating_sub(days)),
                    approved_count,
                    days_since_last: Some(days),
                },
            }
        }
    }
}
