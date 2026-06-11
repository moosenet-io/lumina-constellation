//! EDGE-05: Simple 5-field cron expression parser.
//!
//! Supported syntax: `min hour dom month dow`
//!
//! Field  Range  Supports
//! -----  -----  --------
//! min    0-59   `*`, `N`, `N,M,…`, `N-M`, `*/N`
//! hour   0-23   same
//! dom    1-31   same
//! month  1-12   same
//! dow    0-6    same  (0 = Sunday)

use crate::error::{LuminaError, Result};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// ── CronField ────────────────────────────────────────────────────────────────

/// One parsed field of a 5-field cron expression.
#[derive(Debug, Clone, PartialEq)]
pub enum CronField {
    /// `*` — any value matches.
    Any,
    /// `N` — exactly this value.
    Value(u8),
    /// `N,M,…` — any of these values.
    List(Vec<u8>),
    /// `N-M` — inclusive range.
    Range(u8, u8),
    /// `*/N` — every N steps (starting from the field's minimum).
    Step(u8),
}

impl CronField {
    /// Returns true if `v` satisfies this field.
    ///
    /// `min_val` is the field's minimum (0 for min/hour/dow, 1 for dom/month).
    /// For `Step(n)`, matches every Nth value starting from `min_val`.
    pub fn matches_with_min(&self, v: u8, min_val: u8) -> bool {
        match self {
            CronField::Any => true,
            CronField::Value(n) => v == *n,
            CronField::List(lst) => lst.contains(&v),
            CronField::Range(lo, hi) => v >= *lo && v <= *hi,
            CronField::Step(n) => *n > 0 && v >= min_val && (v - min_val) % n == 0,
        }
    }

    /// Convenience wrapper for 0-based fields (minutes, hours, dow).
    pub fn matches(&self, v: u8) -> bool {
        self.matches_with_min(v, 0)
    }

    /// Parse a single cron field token, validating values against [min_val, max_val].
    ///
    /// Returns an error for:
    /// - Wrong number of fields
    /// - Non-numeric tokens
    /// - Values outside [min_val, max_val]
    /// - Step of 0
    /// - Range with start > end
    fn parse(token: &str, min_val: u8, max_val: u8) -> Result<Self> {
        if token == "*" {
            return Ok(CronField::Any);
        }

        // `*/N` step
        if let Some(step_str) = token.strip_prefix("*/") {
            let n: u8 = step_str.parse().map_err(|_| {
                LuminaError::Config(format!("Invalid cron step '{}' in '{}'", step_str, token))
            })?;
            if n == 0 {
                return Err(LuminaError::Config(
                    "Cron step value must be >= 1".to_string(),
                ));
            }
            return Ok(CronField::Step(n));
        }

        // `N,M,…` list (comma present)
        if token.contains(',') {
            let parts: Vec<u8> = token
                .split(',')
                .map(|p| {
                    let v: u8 = p.trim().parse::<u8>().map_err(|_| {
                        LuminaError::Config(format!("Invalid list element '{}' in '{}'", p, token))
                    })?;
                    if v < min_val || v > max_val {
                        return Err(LuminaError::Config(format!(
                            "Value {} out of range [{}, {}] in '{}'",
                            v, min_val, max_val, token
                        )));
                    }
                    Ok(v)
                })
                .collect::<Result<_>>()?;
            return Ok(CronField::List(parts));
        }

        // `N-M` range
        if token.contains('-') {
            let mut parts = token.splitn(2, '-');
            let lo: u8 = parts
                .next()
                .unwrap_or("")
                .parse()
                .map_err(|_| LuminaError::Config(format!("Invalid range start in '{}'", token)))?;
            let hi: u8 = parts
                .next()
                .unwrap_or("")
                .parse()
                .map_err(|_| LuminaError::Config(format!("Invalid range end in '{}'", token)))?;
            if lo > hi {
                return Err(LuminaError::Config(format!(
                    "Range {}-{} is invalid (start > end)",
                    lo, hi
                )));
            }
            if lo < min_val || hi > max_val {
                return Err(LuminaError::Config(format!(
                    "Range {}-{} is outside valid range [{}, {}]",
                    lo, hi, min_val, max_val
                )));
            }
            return Ok(CronField::Range(lo, hi));
        }

        // Plain number
        let n: u8 = token.parse().map_err(|_| {
            LuminaError::Config(format!("Invalid cron field value '{}'", token))
        })?;
        if n < min_val || n > max_val {
            return Err(LuminaError::Config(format!(
                "Value {} out of range [{}, {}]",
                n, min_val, max_val
            )));
        }
        Ok(CronField::Value(n))
    }
}

// ── CronExpr ─────────────────────────────────────────────────────────────────

/// A parsed 5-field cron expression.
#[derive(Debug, Clone)]
pub struct CronExpr {
    pub minutes: CronField, // 0-59
    pub hours: CronField,   // 0-23
    pub dom: CronField,     // 1-31
    pub month: CronField,   // 1-12
    pub dow: CronField,     // 0-6  (0 = Sunday)
}

impl CronExpr {
    /// Parse a 5-field cron expression string.
    ///
    /// Returns an error for wrong field count or invalid field syntax.
    pub fn parse(expr: &str) -> Result<Self> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(LuminaError::Config(format!(
                "Cron expression must have 5 fields, got {}: '{}'",
                fields.len(),
                expr
            )));
        }

        let minutes = CronField::parse(fields[0], 0, 59)?;
        let hours = CronField::parse(fields[1], 0, 23)?;
        let dom = CronField::parse(fields[2], 1, 31)?;
        let month = CronField::parse(fields[3], 1, 12)?;
        let dow = CronField::parse(fields[4], 0, 6)?;

        Ok(CronExpr { minutes, hours, dom, month, dow })
    }

    /// Calculate the next `SystemTime` after `from` that satisfies this expression.
    ///
    /// Uses a forward-scan approach: advances by 1-minute steps from `from + 60s`
    /// up to one year into the future.  Returns the first matching time or
    /// `from + 1_year` as a fallback if nothing matches (should not happen with
    /// valid expressions, but avoids an infinite loop).
    pub fn next_after(&self, from: SystemTime) -> SystemTime {
        // Advance to the start of the next minute.
        let start = from + Duration::from_secs(60);
        let start_secs = start
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Round down to the start of this minute (whole-minute epoch).
        let start_secs = (start_secs / 60) * 60;

        let one_year_secs: u64 = 366 * 24 * 3600;

        let mut t = start_secs;
        let limit = start_secs + one_year_secs;

        while t < limit {
            let (year, month, dom, hour, minute, dow) = decompose(t);

            if !self.month.matches_with_min(month, 1) {
                // Jump to the 1st day of the next month (skip entire month)
                t = next_month_start(t, year, month);
                continue;
            }
            if !self.dom.matches_with_min(dom, 1) || !self.dow.matches(dow) {
                // Jump to next day
                t = t + 24 * 3600;
                // Round down to midnight
                t = (t / 86400) * 86400;
                continue;
            }
            if !self.hours.matches(hour) {
                // Jump to next hour
                t = t + 3600;
                // Round down to the hour
                t = (t / 3600) * 3600;
                continue;
            }
            if self.minutes.matches(minute) {
                return UNIX_EPOCH + Duration::from_secs(t);
            }
            // Next minute
            t += 60;
        }

        // Fallback: one year from start (should not reach here for valid expressions)
        UNIX_EPOCH + Duration::from_secs(start_secs + one_year_secs)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Decompose Unix seconds into (year, month, dom, hour, minute, dow).
///
/// Pure integer arithmetic — no libc.  Weekday via Tomohiko Sakamoto's
/// algorithm (dow: 0=Sunday).
fn decompose(secs: u64) -> (i64, u8, u8, u8, u8, u8) {
    let minute = ((secs % 3600) / 60) as u8;
    let hour = ((secs % 86400) / 3600) as u8;
    let days = (secs / 86400) as i64; // days since 1970-01-01

    // Convert day count to Y-M-D using the same algorithm as retrain_scheduler
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = if mo <= 2 { y + 1 } else { y };

    // Weekday: Zeller / Sakamoto approach (0=Sunday)
    // Using the days-since-epoch approach: 1970-01-01 was a Thursday (4).
    let dow = ((days + 4).rem_euclid(7)) as u8;

    (year, mo, d, hour, minute, dow)
}

/// Advance `t` to the start of the next calendar month.
fn next_month_start(t: u64, year: i64, month: u8) -> u64 {
    // Calculate the day this month started by finding the 1st of this month.
    // Easier: just add days-in-this-month * 86400 and then subtract partial day.
    // But simpler: find Unix epoch seconds for 1st of next month.
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1u8)
    } else {
        (year, month + 1)
    };
    // Convert next_year/next_month/1 to days since epoch.
    // Using the inverse of the decompose algorithm (Gregorian to JDN minus offset).
    let days = gregorian_to_days(next_year, next_month as i64, 1);
    // Ensure we don't go backward (should not happen, but guard against it).
    let next_epoch = (days as u64) * 86400;
    if next_epoch > t { next_epoch } else { t + 86400 }
}

/// Convert a Gregorian date to days since Unix epoch (1970-01-01).
fn gregorian_to_days(year: i64, month: i64, day: i64) -> i64 {
    // Algorithm: from the same Fliegel & Van Flandern JDN as in retrain_scheduler.
    let jdn = (1461 * (year + 4800 + (month - 14) / 12)) / 4
        + (367 * (month - 2 - 12 * ((month - 14) / 12))) / 12
        - (3 * ((year + 4900 + (month - 14) / 12) / 100)) / 4
        + day
        - 32075;
    // JDN for 1970-01-01
    let epoch_jdn: i64 = 2440588;
    jdn - epoch_jdn
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    // ── CronField::parse ──────────────────────────────────────────────────────

    #[test]
    fn test_field_any() {
        let f = CronField::parse("*", 0, 59).unwrap();
        assert_eq!(f, CronField::Any);
        assert!(f.matches(0));
        assert!(f.matches(59));
    }

    #[test]
    fn test_field_value() {
        let f = CronField::parse("7", 0, 59).unwrap();
        assert_eq!(f, CronField::Value(7));
        assert!(f.matches(7));
        assert!(!f.matches(8));
    }

    #[test]
    fn test_field_list() {
        let f = CronField::parse("1,2,3", 0, 59).unwrap();
        assert!(f.matches(1));
        assert!(f.matches(3));
        assert!(!f.matches(4));
    }

    #[test]
    fn test_field_range() {
        let f = CronField::parse("5-10", 0, 59).unwrap();
        assert!(f.matches(5));
        assert!(f.matches(10));
        assert!(!f.matches(4));
        assert!(!f.matches(11));
    }

    #[test]
    fn test_field_step() {
        let f = CronField::parse("*/15", 0, 59).unwrap();
        assert!(f.matches(0));
        assert!(f.matches(15));
        assert!(f.matches(30));
        assert!(f.matches(45));
        assert!(!f.matches(1));
        assert!(!f.matches(16));
    }

    #[test]
    fn test_field_step_zero_error() {
        assert!(CronField::parse("*/0", 0, 59).is_err());
    }

    #[test]
    fn test_field_invalid_range_direction() {
        assert!(CronField::parse("10-5", 0, 59).is_err());
    }

    #[test]
    fn test_field_invalid_value() {
        assert!(CronField::parse("abc", 0, 59).is_err());
    }

    // ── CronExpr::parse ───────────────────────────────────────────────────────

    #[test]
    fn test_parse_daily_7am() {
        // "0 7 * * *" — every day at 07:00
        let c = CronExpr::parse("0 7 * * *").unwrap();
        assert_eq!(c.minutes, CronField::Value(0));
        assert_eq!(c.hours, CronField::Value(7));
        assert_eq!(c.dom, CronField::Any);
        assert_eq!(c.month, CronField::Any);
        assert_eq!(c.dow, CronField::Any);
    }

    #[test]
    fn test_parse_every_5_minutes() {
        let c = CronExpr::parse("*/5 * * * *").unwrap();
        assert_eq!(c.minutes, CronField::Step(5));
        assert_eq!(c.hours, CronField::Any);
    }

    #[test]
    fn test_parse_weekly_monday() {
        // Every Monday at 09:00
        let c = CronExpr::parse("0 9 * * 1").unwrap();
        assert_eq!(c.dow, CronField::Value(1));
    }

    #[test]
    fn test_parse_wrong_field_count() {
        assert!(CronExpr::parse("0 7 * *").is_err());       // 4 fields
        assert!(CronExpr::parse("0 7 * * * *").is_err());  // 6 fields
    }

    // ── next_after ────────────────────────────────────────────────────────────

    /// Helper: Unix epoch seconds → SystemTime.
    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    /// 2024-01-01 00:00:00 UTC = 1704067200
    const T_2024_01_01: u64 = 1704067200;

    #[test]
    fn test_next_after_daily_cron() {
        // "0 7 * * *" — next 07:00 after 2024-01-01 00:00:00
        // Expected: 2024-01-01 07:00:00 = 1704067200 + 7*3600 = 1704067200 + 25200
        let cron = CronExpr::parse("0 7 * * *").unwrap();
        let next = cron.next_after(at(T_2024_01_01));
        let next_secs = next.duration_since(UNIX_EPOCH).unwrap().as_secs();
        let expected = T_2024_01_01 + 7 * 3600;
        assert_eq!(next_secs, expected, "Expected 2024-01-01 07:00, got offset {}", next_secs - T_2024_01_01);
    }

    #[test]
    fn test_next_after_hourly() {
        // "0 * * * *" — next hour after 2024-01-01 00:00:00 should be 01:00
        let cron = CronExpr::parse("0 * * * *").unwrap();
        let next = cron.next_after(at(T_2024_01_01));
        let next_secs = next.duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(next_secs, T_2024_01_01 + 3600, "Expected 01:00:00");
    }

    #[test]
    fn test_next_after_every_5_minutes() {
        // "*/5 * * * *" from 00:00 → first match at 00:05
        let cron = CronExpr::parse("*/5 * * * *").unwrap();
        let next = cron.next_after(at(T_2024_01_01));
        let next_secs = next.duration_since(UNIX_EPOCH).unwrap().as_secs();
        // 00:05 = T + 5*60
        assert_eq!(next_secs, T_2024_01_01 + 5 * 60);
    }

    #[test]
    fn test_next_after_skips_past_time() {
        // If we're at 07:30, next "0 7 * * *" should be tomorrow 07:00
        let cron = CronExpr::parse("0 7 * * *").unwrap();
        let from = at(T_2024_01_01 + 7 * 3600 + 30 * 60); // 07:30
        let next = cron.next_after(from);
        let next_secs = next.duration_since(UNIX_EPOCH).unwrap().as_secs();
        let expected = T_2024_01_01 + 86400 + 7 * 3600; // tomorrow 07:00
        assert_eq!(next_secs, expected);
    }

    #[test]
    fn test_decompose_known_date() {
        // 2024-01-01 07:05:00 UTC
        let secs = T_2024_01_01 + 7 * 3600 + 5 * 60;
        let (year, month, dom, hour, minute, _dow) = decompose(secs);
        assert_eq!(year, 2024);
        assert_eq!(month, 1);
        assert_eq!(dom, 1);
        assert_eq!(hour, 7);
        assert_eq!(minute, 5);
    }

    #[test]
    fn test_next_after_monthly() {
        // "0 0 1 * *" — 1st of each month at midnight
        // From 2024-01-01 00:00, next is 2024-02-01 00:00
        let cron = CronExpr::parse("0 0 1 * *").unwrap();
        let next = cron.next_after(at(T_2024_01_01));
        let next_secs = next.duration_since(UNIX_EPOCH).unwrap().as_secs();
        // 2024-02-01 = 1706745600
        assert_eq!(next_secs, 1706745600, "Expected 2024-02-01 00:00:00");
    }

    // ── Field validation tests ────────────────────────────────────────────────

    #[test]
    fn test_field_out_of_range_value_rejected() {
        // minute max is 59 — 60 should fail
        assert!(CronField::parse("60", 0, 59).is_err());
        // hour max is 23 — 24 should fail
        assert!(CronField::parse("24", 0, 23).is_err());
        // month max is 12 — 13 should fail
        assert!(CronField::parse("13", 1, 12).is_err());
        // dow max is 6 — 7 should fail
        assert!(CronField::parse("7", 0, 6).is_err());
    }

    #[test]
    fn test_full_cron_out_of_range_rejects() {
        // minute=99, hour=25, dom=32, month=13, dow=9 — all invalid
        assert!(CronExpr::parse("99 7 * * *").is_err());
        assert!(CronExpr::parse("0 25 * * *").is_err());
        assert!(CronExpr::parse("0 7 32 * *").is_err());
        assert!(CronExpr::parse("0 7 * 13 *").is_err());
        assert!(CronExpr::parse("0 7 * * 9").is_err());
    }

    #[test]
    fn test_step_on_1indexed_field() {
        // dom is 1-31; "*/3" should match 1, 4, 7, 10, …  NOT 3, 6, 9, …
        let f = CronField::parse("*/3", 1, 31).unwrap();
        // matches_with_min(1, 1): (1-1) % 3 == 0 → true
        assert!(f.matches_with_min(1, 1), "dom 1 should match */3");
        // matches_with_min(4, 1): (4-1) % 3 == 0 → true
        assert!(f.matches_with_min(4, 1), "dom 4 should match */3");
        // matches_with_min(7, 1): (7-1) % 3 == 0 → true
        assert!(f.matches_with_min(7, 1), "dom 7 should match */3");
        // matches_with_min(3, 1): (3-1) % 3 == 2 → false
        assert!(!f.matches_with_min(3, 1), "dom 3 should NOT match */3");
        // matches_with_min(6, 1): (6-1) % 3 == 2 → false
        assert!(!f.matches_with_min(6, 1), "dom 6 should NOT match */3");
    }

    #[test]
    fn test_range_out_of_bounds_rejected() {
        // Range 0-32 with max_val=31 should fail
        assert!(CronField::parse("0-32", 1, 31).is_err());
    }

    #[test]
    fn test_list_out_of_bounds_rejected() {
        // List with an out-of-range value should fail
        assert!(CronField::parse("1,2,60", 0, 59).is_err());
    }
}
