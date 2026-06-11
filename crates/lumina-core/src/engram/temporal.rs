//! EMEM-03: Temporal awareness and time-based retrieval for Engram v2.
//!
//! ## What this module provides
//! 1. `TemporalQuery` — parse time references from natural language query text
//!    using keyword matching (no LLM — keeps retrieval fast per inference de-bloat rules).
//! 2. `temporal_decay` / `access_boost` — scoring factors applied on top of
//!    cosine similarity so recently-created, frequently-accessed memories rank higher.
//! 3. `apply_temporal_scoring` — convenience wrapper that applies both factors to
//!    a `(content, score)` list returned by `retrieve_from_embeddings`.
//!
//! ## Design notes
//! - All time arithmetic uses `std::time::SystemTime` + the `unix_secs_to_iso` /
//!   `iso_now` helpers from `types.rs`. No chrono dependency.
//! - ISO 8601 strings from the DB (`"YYYY-MM-DDTHH:MM:SSZ"`) are parsed back to
//!   Unix seconds with `iso_to_unix_secs` so we can compute age in days.
//! - Keyword matching is case-insensitive and supports the phrases listed in the
//!   EMEM-03 spec: today, yesterday, this week, last week, this month, last month,
//!   recently, when did I first.

use std::time::{SystemTime, UNIX_EPOCH};

// ── Unix time helpers ─────────────────────────────────────────────────────────

/// Return the current time as Unix seconds (u64).
fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Parse an ISO 8601 string (`"YYYY-MM-DDTHH:MM:SSZ"`) to Unix seconds.
///
/// Accepts both `Z` suffix and strings without a suffix. Returns `None` if
/// the string cannot be parsed (e.g. empty, malformed, v1 migration artifact).
///
/// Only handles the date portion when parsing — full datetime works too.
pub fn iso_to_unix_secs(ts: &str) -> Option<u64> {
    // Accept YYYY-MM-DD prefix (at minimum) in YYYY-MM-DDTHH:MM:SSZ format
    let ts = ts.trim_end_matches('Z');
    let date_part = ts.get(..10)?; // "YYYY-MM-DD" — .get(..10)? already handles short strings
    let year: u64 = date_part.get(..4)?.parse().ok()?;
    let month: u64 = date_part.get(5..7)?.parse().ok()?;
    let day: u64 = date_part.get(8..10)?.parse().ok()?;

    // Parse time if present: "THH:MM:SS"
    let (hour, min, sec) = if ts.len() >= 19 {
        let h: u64 = ts.get(11..13).and_then(|s| s.parse().ok()).unwrap_or(0);
        let m: u64 = ts.get(14..16).and_then(|s| s.parse().ok()).unwrap_or(0);
        let s: u64 = ts.get(17..19).and_then(|s| s.parse().ok()).unwrap_or(0);
        (h, m, s)
    } else {
        (0, 0, 0)
    };

    // Convert Gregorian date to Julian Day Number, then to Unix days.
    // Algorithm: Fliegel-Van Flandern (same as used in types.rs for the inverse).
    // JDN for 1970-01-01 = 2440588
    let a = (14u64.saturating_sub(month)) / 12;
    let y = year + 4800 - a;
    let m = month + 12 * a - 3;
    let jdn = day + (153 * m + 2) / 5 + 365 * y + y / 4 - y / 100 + y / 400 - 32045;
    let epoch_jdn: u64 = 2_440_588;
    if jdn < epoch_jdn {
        return None; // Before Unix epoch
    }
    let days = jdn - epoch_jdn;
    Some(days * 86400 + hour * 3600 + min * 60 + sec)
}

/// Compute the age of a timestamp in fractional days from now.
///
/// Returns `f64::MAX` if the timestamp cannot be parsed (treats as maximally old).
pub fn days_since(created_at: &str) -> f64 {
    match iso_to_unix_secs(created_at) {
        Some(ts) => {
            let now = now_unix_secs();
            if now > ts {
                (now - ts) as f64 / 86400.0
            } else {
                0.0
            }
        }
        None => f64::MAX,
    }
}

// ── TemporalQuery ─────────────────────────────────────────────────────────────

/// The result of parsing time references from a natural language query.
#[derive(Debug, Clone, PartialEq)]
pub struct TemporalQuery {
    /// If set, only memories created within `[start_unix_secs, end_unix_secs)`
    /// should be considered. Both values are Unix seconds.
    pub time_range: Option<(u64, u64)>,
    /// If true, results should be ordered oldest-first (`ORDER BY created_at ASC`)
    /// and limited to 1 — maps to "when did I first" queries.
    pub oldest_first: bool,
}

impl TemporalQuery {
    /// No time constraint — backward-compatible with all existing behaviour.
    pub fn none() -> Self {
        Self { time_range: None, oldest_first: false }
    }

    /// Whether this query has any temporal constraint.
    pub fn has_constraint(&self) -> bool {
        self.time_range.is_some() || self.oldest_first
    }
}

/// Parse temporal keywords from `query_text` (case-insensitive).
///
/// Returns a `TemporalQuery` describing the constraint detected. If no time
/// reference is found, returns `TemporalQuery::none()` (backward compatible).
///
/// Keyword priority (first match wins):
/// 1. "when did i first" → oldest_first = true, no time range
/// 2. "today"             → [midnight today, now]
/// 3. "yesterday"         → [midnight yesterday, midnight today]
/// 4. "last week" / "this week" / "recently" → last 7 days
/// 5. "last month" / "this month"            → last 30 days
pub fn parse_temporal_query(query_text: &str) -> TemporalQuery {
    let lower = query_text.to_lowercase();
    let now = now_unix_secs();

    // Midnight today (start of current UTC day)
    let today_start = (now / 86400) * 86400;

    if lower.contains("when did i first") || lower.contains("first time") {
        return TemporalQuery { time_range: None, oldest_first: true };
    }

    if lower.contains("today") {
        return TemporalQuery {
            time_range: Some((today_start, now + 1)),
            oldest_first: false,
        };
    }

    if lower.contains("yesterday") {
        let yesterday_start = today_start.saturating_sub(86400);
        return TemporalQuery {
            time_range: Some((yesterday_start, today_start)),
            oldest_first: false,
        };
    }

    if lower.contains("last week")
        || lower.contains("this week")
        || lower.contains("recently")
        || lower.contains("recent")
    {
        let week_ago = now.saturating_sub(7 * 86400);
        return TemporalQuery {
            time_range: Some((week_ago, now + 1)),
            oldest_first: false,
        };
    }

    if lower.contains("last month") || lower.contains("this month") {
        let month_ago = now.saturating_sub(30 * 86400);
        return TemporalQuery {
            time_range: Some((month_ago, now + 1)),
            oldest_first: false,
        };
    }

    TemporalQuery::none()
}

/// Return `true` if `created_at` (ISO 8601) falls within the time range.
///
/// If `created_at` cannot be parsed, it is treated as the oldest possible time
/// (Unix second 0) — this handles v1 migration artifacts gracefully.
pub fn matches_time_range(created_at: &str, range: (u64, u64)) -> bool {
    let ts = iso_to_unix_secs(created_at).unwrap_or(0);
    let (start, end) = range;
    ts >= start && ts < end
}

// ── Temporal decay scoring ────────────────────────────────────────────────────

/// Decay constant λ — 1% per day (`e^(-0.01 * days)`).
/// Gentle enough that a 30-day-old memory retains ~74% of its score.
pub const DECAY_LAMBDA: f64 = 0.01;

/// Compute the temporal decay factor for a memory.
///
/// `decay_factor = e^(-λ * days_since_creation)`
///
/// Range: `(0.0, 1.0]`. A brand-new memory has factor = 1.0; a 100-day-old
/// memory has factor ≈ 0.37.
///
/// If `created_at` cannot be parsed (v1 artifact), returns the minimum possible
/// factor so the memory sinks to the bottom of the ranking.
pub fn temporal_decay(created_at: &str) -> f64 {
    let age_days = days_since(created_at);
    if age_days == f64::MAX {
        // Cannot parse — treat as oldest possible: near-zero decay factor
        return f64::MIN_POSITIVE;
    }
    (-DECAY_LAMBDA * age_days).exp()
}

/// Compute the access boost factor for a memory.
///
/// `access_boost = 1.0 + 0.1 * ln(access_count + 1)`
///
/// Range: `[1.0, ∞)`. A never-accessed memory has boost = 1.0 (no change).
/// A memory accessed 100 times has boost ≈ 1.46.
pub fn access_boost(access_count: i32) -> f64 {
    let count = access_count.max(0) as f64;
    1.0 + 0.1 * (count + 1.0).ln()
}

/// Apply temporal decay and access boost to a list of `(content, base_score)`
/// pairs from `retrieve_from_embeddings`.
///
/// Each entry's score is multiplied by `temporal_decay(created_at) * access_boost(access_count)`.
/// The list is re-sorted descending by the adjusted score.
///
/// `metadata` must have the same length as `scored` and provides
/// `(created_at, access_count)` for each corresponding entry.
pub fn apply_temporal_scoring(
    scored: Vec<(String, f32)>,
    metadata: &[(String, i32)],
) -> Vec<(String, f32)> {
    assert_eq!(
        scored.len(),
        metadata.len(),
        "apply_temporal_scoring: scored and metadata must have the same length"
    );

    let mut weighted: Vec<(String, f32)> = scored
        .into_iter()
        .zip(metadata.iter())
        .map(|((content, base_score), (created_at, ac))| {
            let decay = temporal_decay(created_at) as f32;
            let boost = access_boost(*ac) as f32;
            (content, base_score * decay * boost)
        })
        .collect();

    weighted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    weighted
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: current Unix secs
    fn now() -> u64 {
        now_unix_secs()
    }

    // ── iso_to_unix_secs ──────────────────────────────────────────────────────

    #[test]
    fn test_iso_to_unix_secs_epoch() {
        // Unix epoch: 1970-01-01T00:00:00Z
        assert_eq!(iso_to_unix_secs("1970-01-01T00:00:00Z"), Some(0));
    }

    #[test]
    fn test_iso_to_unix_secs_known_date() {
        // 2026-06-06T00:00:00Z — verify it parses without panic and returns a
        // plausible value (> 1700000000 = 2023).
        let secs = iso_to_unix_secs("2026-06-06T00:00:00Z").unwrap();
        assert!(secs > 1_700_000_000, "expected post-2023 timestamp, got {secs}");
    }

    #[test]
    fn test_iso_to_unix_secs_invalid_returns_none() {
        assert!(iso_to_unix_secs("").is_none());
        assert!(iso_to_unix_secs("not-a-date").is_none());
        assert!(iso_to_unix_secs("1960-01-01T00:00:00Z").is_none()); // before epoch
    }

    #[test]
    fn test_iso_to_unix_secs_date_only() {
        // Date-only string (no time component) should still parse
        let secs = iso_to_unix_secs("2026-06-06").unwrap();
        assert!(secs > 1_700_000_000);
    }

    // ── parse_temporal_query ──────────────────────────────────────────────────

    #[test]
    fn test_parse_no_time_ref_is_backward_compatible() {
        // No temporal keyword → TemporalQuery::none() — existing behaviour unchanged.
        let q = parse_temporal_query("what do I like for breakfast?");
        assert!(!q.has_constraint());
        assert!(q.time_range.is_none());
        assert!(!q.oldest_first);
    }

    #[test]
    fn test_parse_last_week_range() {
        let n = now();
        let q = parse_temporal_query("what did we discuss last week?");
        assert!(q.time_range.is_some(), "'last week' should produce a time range");
        let (start, end) = q.time_range.unwrap();
        // start should be ~7 days ago
        let expected_start = n.saturating_sub(7 * 86400);
        let delta = (start as i64 - expected_start as i64).abs();
        assert!(delta < 5, "start should be ~7 days ago, delta={delta}s");
        assert!(end > n, "end should be beyond now");
    }

    #[test]
    fn test_parse_this_week_range() {
        let n = now();
        let q = parse_temporal_query("memories from this week");
        let (start, _) = q.time_range.expect("'this week' should produce a time range");
        let expected_start = n.saturating_sub(7 * 86400);
        let delta = (start as i64 - expected_start as i64).abs();
        assert!(delta < 5, "this week start delta={delta}s");
    }

    #[test]
    fn test_parse_yesterday_range() {
        let n = now();
        let today_start = (n / 86400) * 86400;
        let q = parse_temporal_query("what happened yesterday?");
        let (start, end) = q.time_range.expect("'yesterday' should produce a time range");
        assert_eq!(end, today_start, "yesterday range should end at midnight today");
        let expected_start = today_start.saturating_sub(86400);
        assert_eq!(start, expected_start, "yesterday range should start at midnight yesterday");
    }

    #[test]
    fn test_parse_today_range() {
        let n = now();
        let today_start = (n / 86400) * 86400;
        let q = parse_temporal_query("what have I done today?");
        let (start, end) = q.time_range.expect("'today' should produce a time range");
        assert_eq!(start, today_start, "today range should start at midnight today");
        assert!(end > n, "today range end should be >= now");
    }

    #[test]
    fn test_parse_recently_range() {
        let n = now();
        let q = parse_temporal_query("show me recent memories about work");
        let (start, _) = q.time_range.expect("'recently' should produce a time range");
        let expected_start = n.saturating_sub(7 * 86400);
        let delta = (start as i64 - expected_start as i64).abs();
        assert!(delta < 5, "recently start delta={delta}s");
    }

    #[test]
    fn test_parse_last_month_range() {
        let n = now();
        let q = parse_temporal_query("what happened last month?");
        let (start, _) = q.time_range.expect("'last month' should produce a time range");
        let expected_start = n.saturating_sub(30 * 86400);
        let delta = (start as i64 - expected_start as i64).abs();
        assert!(delta < 5, "last month start delta={delta}s");
    }

    #[test]
    fn test_parse_when_did_i_first_sets_oldest_first() {
        let q = parse_temporal_query("when did I first mention my dog?");
        assert!(q.oldest_first, "'when did I first' should set oldest_first=true");
        // time_range is not required — we just flip sort order
        let q2 = parse_temporal_query("show me the first time I talked about hiking");
        assert!(q2.oldest_first, "'first time' should also set oldest_first=true");
    }

    #[test]
    fn test_parse_case_insensitive() {
        // "LAST WEEK" (all caps) should still parse
        let q = parse_temporal_query("LAST WEEK discussion");
        assert!(q.time_range.is_some(), "case-insensitive match should work");
    }

    // ── matches_time_range ────────────────────────────────────────────────────

    #[test]
    fn test_matches_time_range_inside() {
        // A timestamp right in the middle of a range should match
        let mid = 1_000_000u64;
        let ts = crate::engram::types::unix_secs_to_iso(mid);
        assert!(matches_time_range(&ts, (mid - 100, mid + 100)));
    }

    #[test]
    fn test_matches_time_range_outside() {
        let mid = 1_000_000u64;
        let ts = crate::engram::types::unix_secs_to_iso(mid);
        assert!(!matches_time_range(&ts, (mid + 1, mid + 200)));
    }

    #[test]
    fn test_matches_time_range_invalid_ts_treated_as_oldest() {
        // Unparseable timestamp treated as Unix second 0 — falls outside a
        // recent time range.
        let range = (now().saturating_sub(7 * 86400), now() + 1);
        assert!(!matches_time_range("not-a-date", range));
    }

    // ── temporal_decay ────────────────────────────────────────────────────────

    #[test]
    fn test_temporal_decay_brand_new_is_near_one() {
        // A memory created right now should have a decay factor near 1.0.
        let now_ts = crate::engram::types::iso_now();
        let factor = temporal_decay(&now_ts);
        assert!(
            (factor - 1.0).abs() < 0.01,
            "brand-new memory decay should be ~1.0, got {factor}"
        );
    }

    #[test]
    fn test_temporal_decay_old_memory_is_lower() {
        // A memory from ~100 days ago should have a much lower decay factor.
        let old_secs = now().saturating_sub(100 * 86400);
        let old_ts = crate::engram::types::unix_secs_to_iso(old_secs);
        let factor = temporal_decay(&old_ts);
        // e^(-0.01 * 100) ≈ 0.3679
        assert!(factor < 0.5, "100-day-old memory should have decay < 0.5, got {factor}");
        assert!(factor > 0.0, "decay should be positive");
    }

    #[test]
    fn test_temporal_decay_reduces_old_memory_score() {
        // Verify that a new memory scores higher than an old one after decay.
        let new_ts = crate::engram::types::iso_now();
        let old_secs = now().saturating_sub(365 * 86400);
        let old_ts = crate::engram::types::unix_secs_to_iso(old_secs);

        let decay_new = temporal_decay(&new_ts);
        let decay_old = temporal_decay(&old_ts);

        assert!(
            decay_new > decay_old,
            "new memory decay ({decay_new}) should be greater than old ({decay_old})"
        );
    }

    #[test]
    fn test_temporal_decay_unparseable_ts_returns_min_positive() {
        let factor = temporal_decay("garbage-timestamp");
        assert!(factor > 0.0, "decay should always be positive");
        assert!(factor < 0.01, "unparseable ts should produce near-zero decay");
    }

    // ── access_boost ─────────────────────────────────────────────────────────

    #[test]
    fn test_access_boost_zero_is_one() {
        // Never-accessed memory: boost = 1.0 + 0.1 * ln(1) = 1.0
        let b = access_boost(0);
        assert!((b - 1.0).abs() < 1e-9, "access_boost(0) should be 1.0, got {b}");
    }

    #[test]
    fn test_access_boost_increases_with_count() {
        let b1 = access_boost(1);
        let b10 = access_boost(10);
        let b100 = access_boost(100);
        assert!(b1 > 1.0, "access_boost(1) should be > 1.0, got {b1}");
        assert!(b10 > b1, "boost should grow with access count");
        assert!(b100 > b10, "boost should grow with access count");
    }

    #[test]
    fn test_access_boost_negative_count_clamps_to_zero() {
        // Negative access_count (shouldn't happen, but be defensive) treated as 0
        let b = access_boost(-5);
        assert!((b - 1.0).abs() < 1e-9, "negative access_count should clamp to 0, got {b}");
    }

    #[test]
    fn test_access_boost_works() {
        // Verify formula: 1.0 + 0.1 * ln(10 + 1) = 1.0 + 0.1 * ln(11) ≈ 1.2398
        let b = access_boost(10);
        let expected = 1.0 + 0.1 * (11.0_f64).ln();
        assert!((b - expected).abs() < 1e-9, "access_boost(10) expected {expected}, got {b}");
    }

    // ── apply_temporal_scoring ────────────────────────────────────────────────

    #[test]
    fn test_apply_temporal_scoring_new_beats_old_equal_base() {
        let new_ts = crate::engram::types::iso_now();
        let old_secs = now().saturating_sub(200 * 86400);
        let old_ts = crate::engram::types::unix_secs_to_iso(old_secs);

        // Both have the same base score of 0.9 and 0 access_count
        let scored = vec![
            ("old fact".to_string(), 0.9f32),
            ("new fact".to_string(), 0.9f32),
        ];
        let metadata = vec![
            (old_ts.clone(), 0i32),
            (new_ts.clone(), 0i32),
        ];
        let result = apply_temporal_scoring(scored, &metadata);

        // New fact should rank first
        assert_eq!(result[0].0, "new fact", "new fact should rank first after decay");
    }

    #[test]
    fn test_apply_temporal_scoring_high_access_boosts_old() {
        let new_ts = crate::engram::types::iso_now();
        let old_secs = now().saturating_sub(30 * 86400); // 30 days ago — moderate decay
        let old_ts = crate::engram::types::unix_secs_to_iso(old_secs);

        // Old memory has high access count, new has none
        let scored = vec![
            ("old popular fact".to_string(), 0.9f32),
            ("new unknown fact".to_string(), 0.9f32),
        ];
        let metadata = vec![
            (old_ts, 1000i32),  // Very frequently accessed — big boost
            (new_ts, 0i32),
        ];
        let result = apply_temporal_scoring(scored, &metadata);

        // Old but very popular fact should beat new unaccessed fact after boost
        assert_eq!(result[0].0, "old popular fact",
            "frequently accessed old fact should beat new unaccessed fact");
    }

    #[test]
    fn test_apply_temporal_scoring_empty_input() {
        let result = apply_temporal_scoring(vec![], &[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_apply_temporal_scoring_preserves_order_equal_metadata() {
        // Two brand-new, equally-accessed memories with different base scores
        let ts = crate::engram::types::iso_now();
        let scored = vec![
            ("high score".to_string(), 0.95f32),
            ("low score".to_string(), 0.50f32),
        ];
        let metadata = vec![
            (ts.clone(), 0i32),
            (ts.clone(), 0i32),
        ];
        let result = apply_temporal_scoring(scored, &metadata);
        assert_eq!(result[0].0, "high score", "higher base score should still rank first");
    }
}
