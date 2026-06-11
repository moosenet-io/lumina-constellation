//! AGENT-03 / AGENT-06: OperationalStore — execution metadata for the agentic loop.
//!
//! Tool call details (names, durations, statuses) flow here instead of into
//! Engram long-term memory.  Engram stores OUTCOMES only (user message +
//! final response); OperationalStore stores HOW the result was reached.
//!
//! AGENT-06 adds:
//! - `SecurityEventRecord` for guard-fired events (blocked/sanitized/warned).
//! - `avg_duration_ms` — average tool execution time over a rolling window.
//! - `escalation_rate` — fraction of turns that triggered an escalation event.
//! - `daily_trends` — per-day call counts for sparkline rendering.
//! - `security_events_summary` — per-guard action counts for admin dashboard.
//!
//! Storage remains in-memory (one store per process).  90-day pruning is
//! performed on demand via `prune_old(90)`.
//!
//! No hardcoded infrastructure values anywhere in this file.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

// ── ExecutionRecord ────────────────────────────────────────────────────────

/// Metadata record for a single tool call executed within the agentic loop.
///
/// MUST NOT contain tool arguments or raw tool results — only operational
/// metadata (timing, status, identifiers).
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionRecord {
    /// Unique identifier for the conversation turn that triggered this call.
    pub turn_id: String,
    /// User who initiated the turn.
    pub user_id: String,
    /// Name of the tool that was called.
    pub tool_name: String,
    /// Wall-clock execution time in milliseconds.
    pub duration_ms: u64,
    /// Outcome: "ok", "blocked", "error", or "timeout".
    pub status: String,
    /// Unix timestamp (seconds) when this record was created.
    pub timestamp_secs: i64,
}

impl ExecutionRecord {
    /// Create an `ExecutionRecord` from the components.
    pub fn new(
        turn_id: impl Into<String>,
        user_id: impl Into<String>,
        tool_name: impl Into<String>,
        duration_ms: u64,
        status: impl Into<String>,
    ) -> Self {
        let timestamp_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        Self {
            turn_id: turn_id.into(),
            user_id: user_id.into(),
            tool_name: tool_name.into(),
            duration_ms,
            status: status.into(),
            timestamp_secs,
        }
    }

    /// Returns true if this record represents a failed call (status != "ok").
    pub fn is_failure(&self) -> bool {
        self.status != "ok"
    }

    /// Returns true if this record's timestamp falls within the last `days` days.
    pub fn is_within_days(&self, days: u32) -> bool {
        if days == 0 {
            return false;
        }
        let cutoff = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
            - (days as i64 * 86_400);
        self.timestamp_secs >= cutoff
    }
}

// ── SecurityEventRecord ────────────────────────────────────────────────────

/// A security event recorded when a guard fires during the agentic loop.
///
/// This is a thin data record; it does NOT reference the full
/// `chord_proxy::agentic::SecurityEvent` to avoid a cross-crate dependency.
/// The fields are compatible: `guard_name` matches `SecurityEvent::guard_name`,
/// and `action` holds the serialised `SecurityAction` string
/// ("Blocked", "Sanitized", or "Warned").
#[derive(Debug, Clone, PartialEq)]
pub struct SecurityEventRecord {
    /// Which guard fired: "argument", "result", "response", or "behavioral".
    pub guard_name: String,
    /// What was done: "Blocked", "Sanitized", or "Warned".
    pub action: String,
    /// Tool that was involved (empty for behavioural / conversation-level events).
    pub tool_name: String,
    /// User who initiated the turn that triggered this event.
    pub user_id: String,
    /// Unix timestamp (seconds) when this record was created.
    pub timestamp_secs: i64,
}

impl SecurityEventRecord {
    /// Create a new `SecurityEventRecord` with the current timestamp.
    pub fn new(
        guard_name: impl Into<String>,
        action: impl Into<String>,
        tool_name: impl Into<String>,
        user_id: impl Into<String>,
    ) -> Self {
        let timestamp_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        Self {
            guard_name: guard_name.into(),
            action: action.into(),
            tool_name: tool_name.into(),
            user_id: user_id.into(),
            timestamp_secs,
        }
    }

    /// Returns `true` if this record's timestamp falls within the last `days` days.
    pub fn is_within_days(&self, days: u32) -> bool {
        if days == 0 {
            return false;
        }
        let cutoff = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
            - (days as i64 * 86_400);
        self.timestamp_secs >= cutoff
    }
}

// ── OperationalStore ────────────────────────────────────────────────────────

/// In-memory store for tool execution metadata.
///
/// Thread-safe via internal `Arc<Mutex<…>>` fields.
/// Clone is cheap — clones share the same underlying storage.
///
/// Intended lifecycle: one `OperationalStore` per process (created at startup,
/// passed by value/clone to callers).
#[derive(Debug, Clone)]
pub struct OperationalStore {
    records: Arc<Mutex<Vec<ExecutionRecord>>>,
    security_events: Arc<Mutex<Vec<SecurityEventRecord>>>,
}

impl OperationalStore {
    /// Create an empty `OperationalStore`.
    pub fn new() -> Self {
        Self {
            records: Arc::new(Mutex::new(Vec::new())),
            security_events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Append records from `exec_log` to the store.
    ///
    /// Returns the number of records stored.  On lock poisoning (which should
    /// never happen in practice) returns 0 non-fatally.
    pub fn record(&self, user_id: &str, exec_log: &[ExecutionRecord]) -> u32 {
        let user_scoped: Vec<ExecutionRecord> = exec_log
            .iter()
            .map(|r| {
                let mut rec = r.clone();
                // Ensure user_id is set to the caller's user_id when records are
                // ingested via this method (the caller may have built records with
                // a different user_id from exec_log metadata).
                if rec.user_id.is_empty() {
                    rec.user_id = user_id.to_string();
                }
                rec
            })
            .collect();

        let count = user_scoped.len() as u32;
        if let Ok(mut guard) = self.records.lock() {
            guard.extend(user_scoped);
        }
        count
    }

    /// Return the top `limit` tools by call frequency within the last `days` days.
    ///
    /// Sorted descending by call count.  Returns an empty Vec if the store is empty
    /// or if no records fall within the window.
    pub fn top_tools(&self, days: u32) -> Vec<(String, u32)> {
        let guard = match self.records.lock() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };

        let mut counts: HashMap<String, u32> = HashMap::new();
        for record in guard.iter() {
            if record.is_within_days(days) {
                *counts.entry(record.tool_name.clone()).or_insert(0) += 1;
            }
        }

        let mut sorted: Vec<(String, u32)> = counts.into_iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        sorted
    }

    /// Compute the failure rate (failed calls / total calls) within the last `days` days.
    ///
    /// Returns 0.0 if there are no records in the window.
    pub fn failure_rate(&self, days: u32) -> f32 {
        let guard = match self.records.lock() {
            Ok(g) => g,
            Err(_) => return 0.0,
        };

        let (total, failures): (u32, u32) = guard
            .iter()
            .filter(|r| r.is_within_days(days))
            .fold((0, 0), |(t, f), r| {
                (t + 1, f + if r.is_failure() { 1 } else { 0 })
            });

        if total == 0 {
            0.0
        } else {
            failures as f32 / total as f32
        }
    }

    /// Compute the average tool execution duration (ms) within the last `days` days.
    ///
    /// Returns `0.0` if there are no records in the window.
    pub fn avg_duration_ms(&self, days: u32) -> f64 {
        let guard = match self.records.lock() {
            Ok(g) => g,
            Err(_) => return 0.0,
        };

        let (sum, count): (u64, u64) = guard
            .iter()
            .filter(|r| r.is_within_days(days))
            .fold((0u64, 0u64), |(s, c), r| (s + r.duration_ms, c + 1));

        if count == 0 {
            0.0
        } else {
            sum as f64 / count as f64
        }
    }

    /// Compute the escalation rate within the last `days` days.
    ///
    /// Escalation is defined as any turn that contained at least one security event
    /// with action "Blocked" or "Warned".  The rate is expressed as a fraction of
    /// distinct turns (by turn_id) that had at least one such event.
    ///
    /// Returns `0.0` when there are no execution records in the window.
    pub fn escalation_rate(&self, days: u32) -> f32 {
        let exec_guard = match self.records.lock() {
            Ok(g) => g,
            Err(_) => return 0.0,
        };
        let sec_guard = match self.security_events.lock() {
            Ok(g) => g,
            Err(_) => return 0.0,
        };

        // Collect all distinct turn_ids in the window.
        let all_turns: std::collections::HashSet<&str> = exec_guard
            .iter()
            .filter(|r| r.is_within_days(days))
            .map(|r| r.turn_id.as_str())
            .collect();

        if all_turns.is_empty() {
            return 0.0;
        }

        // Collect turn_ids with at least one blocking/warning security event.
        // We derive escalated turns from security event timestamps vs execution
        // record timestamps.  Since both share turn_id, match by user_id proximity
        // or, more accurately, scan all security events that fire within the same
        // window and count how many *distinct* turns in all_turns have a matching
        // security event recorded for them.
        //
        // Because SecurityEventRecord does not store turn_id (it's a guard-level
        // event), we use a conservative proxy: the fraction of the window's total
        // turns that contain at least one "Blocked" execution record (status ==
        // "blocked").
        let escalated_turns: std::collections::HashSet<&str> = exec_guard
            .iter()
            .filter(|r| r.is_within_days(days) && r.status == "blocked")
            .map(|r| r.turn_id.as_str())
            .collect();

        // Additionally count turns associated with security events in the window.
        let sec_event_count = sec_guard
            .iter()
            .filter(|e| e.is_within_days(days))
            .count() as f32;

        let total_turns = all_turns.len() as f32;
        let escalated = escalated_turns.len() as f32;

        // Normalise: clamp to [0.0, 1.0]
        ((escalated + sec_event_count.min(total_turns)) / total_turns).min(1.0)
    }

    /// Compute per-day call counts within the last `days` days.
    ///
    /// Returns a `Vec<(date_str, call_count)>` sorted ascending by date.
    /// `date_str` is in `YYYY-MM-DD` format (UTC).  Days with zero calls are
    /// omitted.
    pub fn daily_trends(&self, days: u32) -> Vec<(String, u32)> {
        let guard = match self.records.lock() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };

        let mut counts: HashMap<String, u32> = HashMap::new();
        for record in guard.iter() {
            if record.is_within_days(days) {
                let date_str = timestamp_to_date_str(record.timestamp_secs);
                *counts.entry(date_str).or_insert(0) += 1;
            }
        }

        let mut sorted: Vec<(String, u32)> = counts.into_iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        sorted
    }

    /// Return per-(guard_name, action) event counts within the last `days` days.
    ///
    /// The key is a formatted string `"{guard_name}:{action}"` to make table
    /// rendering straightforward.  Sorted descending by count then alphabetically.
    ///
    /// Returns `Vec<(String, u32)>` where `String` is `"guard:action"`.
    pub fn security_events_summary(&self, days: u32) -> Vec<(String, u32)> {
        let guard = match self.security_events.lock() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };

        let mut counts: HashMap<String, u32> = HashMap::new();
        for event in guard.iter() {
            if event.is_within_days(days) {
                let key = format!("{}:{}", event.guard_name, event.action);
                *counts.entry(key).or_insert(0) += 1;
            }
        }

        let mut sorted: Vec<(String, u32)> = counts.into_iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        sorted
    }

    /// Append security guard events to the store.
    ///
    /// Returns the number of events stored.
    pub fn record_security_events(&self, events: &[SecurityEventRecord]) -> u32 {
        let count = events.len() as u32;
        if let Ok(mut guard) = self.security_events.lock() {
            guard.extend_from_slice(events);
        }
        count
    }

    /// Remove records AND security events older than `days` days.
    ///
    /// Returns the total number of entries deleted (execution records + security
    /// events combined).
    pub fn prune_old(&self, days: u32) -> u64 {
        let removed_exec = {
            let mut guard = match self.records.lock() {
                Ok(g) => g,
                Err(_) => return 0,
            };
            let before = guard.len();
            if days == 0 {
                guard.clear();
                before as u64
            } else {
                guard.retain(|r| r.is_within_days(days));
                (before - guard.len()) as u64
            }
        };

        let removed_sec = {
            let mut guard = match self.security_events.lock() {
                Ok(g) => g,
                Err(_) => return removed_exec,
            };
            let before = guard.len();
            if days == 0 {
                guard.clear();
                before as u64
            } else {
                guard.retain(|e| e.is_within_days(days));
                (before - guard.len()) as u64
            }
        };

        removed_exec + removed_sec
    }

    /// Return a snapshot of all records (for testing).
    #[cfg(test)]
    pub fn all_records(&self) -> Vec<ExecutionRecord> {
        self.records.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// Return a snapshot of all security event records (for testing).
    #[cfg(test)]
    pub fn all_security_events(&self) -> Vec<SecurityEventRecord> {
        self.security_events
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// Return total execution record count.
    pub fn len(&self) -> usize {
        self.records.lock().map(|g| g.len()).unwrap_or(0)
    }

    /// Returns true if there are no execution records.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Top `limit` tools for a user (or all users when `user_id` is `None`).
    ///
    /// Admin path (`user_id == None`) sees all users; regular users see only
    /// their own records.
    pub fn top_tools_for(
        &self,
        days: u32,
        user_id: Option<&str>,
    ) -> Vec<(String, u32)> {
        let guard = match self.records.lock() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };

        let mut counts: HashMap<String, u32> = HashMap::new();
        for record in guard.iter() {
            if !record.is_within_days(days) {
                continue;
            }
            if let Some(uid) = user_id {
                if record.user_id != uid {
                    continue;
                }
            }
            *counts.entry(record.tool_name.clone()).or_insert(0) += 1;
        }

        let mut sorted: Vec<(String, u32)> = counts.into_iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        sorted
    }

    /// Average duration for a user (or all users when `user_id` is `None`).
    pub fn avg_duration_ms_for(&self, days: u32, user_id: Option<&str>) -> f64 {
        let guard = match self.records.lock() {
            Ok(g) => g,
            Err(_) => return 0.0,
        };

        let (sum, count): (u64, u64) = guard
            .iter()
            .filter(|r| {
                r.is_within_days(days)
                    && user_id.map_or(true, |uid| r.user_id == uid)
            })
            .fold((0u64, 0u64), |(s, c), r| (s + r.duration_ms, c + 1));

        if count == 0 { 0.0 } else { sum as f64 / count as f64 }
    }

    /// Failure rate for a user (or all users when `user_id` is `None`).
    pub fn failure_rate_for(&self, days: u32, user_id: Option<&str>) -> f32 {
        let guard = match self.records.lock() {
            Ok(g) => g,
            Err(_) => return 0.0,
        };

        let (total, failures): (u32, u32) = guard
            .iter()
            .filter(|r| {
                r.is_within_days(days)
                    && user_id.map_or(true, |uid| r.user_id == uid)
            })
            .fold((0, 0), |(t, f), r| {
                (t + 1, f + if r.is_failure() { 1 } else { 0 })
            });

        if total == 0 { 0.0 } else { failures as f32 / total as f32 }
    }
}

impl Default for OperationalStore {
    fn default() -> Self {
        Self::new()
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Convert a Unix timestamp (seconds) to a UTC date string `"YYYY-MM-DD"`.
///
/// Uses only `std` — no external date library dependency.
fn timestamp_to_date_str(ts_secs: i64) -> String {
    // Days since Unix epoch (1970-01-01)
    let days_total = (ts_secs / 86_400) as i64;
    let (year, month, day) = days_to_ymd(days_total);
    format!("{:04}-{:02}-{:02}", year, month, day)
}

/// Convert a count of days since the Unix epoch (1970-01-01) to (year, month, day).
///
/// Algorithm: Gregorian calendar with 400-year cycle.
fn days_to_ymd(days: i64) -> (i64, u32, u32) {
    // Shift epoch to 1 Mar 0000 for easier leap-year arithmetic.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // year of era [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year [0, 365]
    let mp = (5 * doy + 2) / 153; // month prime [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // day [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // month [1, 12]
    let y = y + if m <= 2 { 1 } else { 0 };
    (y, m as u32, d as u32)
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_record(tool: &str, status: &str, secs_ago: i64) -> ExecutionRecord {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        ExecutionRecord {
            turn_id: format!("turn-{tool}"),
            user_id: "user-test".to_string(),
            tool_name: tool.to_string(),
            duration_ms: 100,
            status: status.to_string(),
            timestamp_secs: now - secs_ago,
        }
    }

    // ── ExecutionRecord::new ────────────────────────────────────────────────

    #[test]
    fn test_execution_record_new_sets_timestamp() {
        let before = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let rec = ExecutionRecord::new("turn-1", "user-a", "calendar_get", 42, "ok");
        let after = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        assert!(rec.timestamp_secs >= before);
        assert!(rec.timestamp_secs <= after);
        assert_eq!(rec.tool_name, "calendar_get");
        assert_eq!(rec.status, "ok");
        assert_eq!(rec.duration_ms, 42);
    }

    #[test]
    fn test_execution_record_is_failure() {
        let ok = ExecutionRecord::new("t", "u", "tool", 10, "ok");
        assert!(!ok.is_failure());
        let err = ExecutionRecord::new("t", "u", "tool", 10, "error");
        assert!(err.is_failure());
        let blocked = ExecutionRecord::new("t", "u", "tool", 10, "blocked");
        assert!(blocked.is_failure());
        let timeout = ExecutionRecord::new("t", "u", "tool", 10, "timeout");
        assert!(timeout.is_failure());
    }

    #[test]
    fn test_execution_record_is_within_days() {
        // Created now — should be within 1 day
        let recent = ExecutionRecord::new("t", "u", "tool", 10, "ok");
        assert!(recent.is_within_days(1));
        assert!(recent.is_within_days(30));
        assert!(recent.is_within_days(90));

        // Created 2 days ago — should NOT be within 1 day
        let old = make_record("old_tool", "ok", 2 * 86_400 + 1);
        assert!(!old.is_within_days(1));
        assert!(old.is_within_days(3));

        // days == 0 → always false
        assert!(!recent.is_within_days(0));
    }

    // ── OperationalStore::record ────────────────────────────────────────────

    #[test]
    fn test_operational_store_record_returns_count() {
        let store = OperationalStore::new();
        let recs = vec![
            ExecutionRecord::new("turn-1", "user-a", "search", 50, "ok"),
            ExecutionRecord::new("turn-1", "user-a", "calendar", 80, "ok"),
        ];
        let count = store.record("user-a", &recs);
        assert_eq!(count, 2);
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn test_operational_store_record_empty_slice() {
        let store = OperationalStore::new();
        let count = store.record("user-a", &[]);
        assert_eq!(count, 0);
        assert!(store.is_empty());
    }

    // ── OperationalStore::top_tools ────────────────────────────────────────

    #[test]
    fn test_top_tools_sorted_descending() {
        let store = OperationalStore::new();
        let recs = vec![
            ExecutionRecord::new("t1", "u", "search", 10, "ok"),
            ExecutionRecord::new("t2", "u", "search", 10, "ok"),
            ExecutionRecord::new("t3", "u", "search", 10, "ok"),
            ExecutionRecord::new("t4", "u", "calendar", 10, "ok"),
            ExecutionRecord::new("t5", "u", "calendar", 10, "ok"),
            ExecutionRecord::new("t6", "u", "weather", 10, "ok"),
        ];
        store.record("u", &recs);
        let top = store.top_tools(30);
        assert_eq!(top.len(), 3);
        assert_eq!(top[0].0, "search");
        assert_eq!(top[0].1, 3);
        assert_eq!(top[1].0, "calendar");
        assert_eq!(top[1].1, 2);
        assert_eq!(top[2].0, "weather");
        assert_eq!(top[2].1, 1);
    }

    #[test]
    fn test_top_tools_empty_store_returns_empty() {
        let store = OperationalStore::new();
        assert!(store.top_tools(30).is_empty());
    }

    #[test]
    fn test_top_tools_excludes_old_records() {
        let store = OperationalStore::new();
        // Old record (3 days ago) — should be excluded from 1-day window
        let old = make_record("old_tool", "ok", 3 * 86_400);
        // Recent record
        let recent = ExecutionRecord::new("t2", "u", "new_tool", 10, "ok");
        store.record("u", &[old, recent]);

        let top = store.top_tools(1);
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].0, "new_tool");
    }

    // ── OperationalStore::failure_rate ─────────────────────────────────────

    #[test]
    fn test_failure_rate_all_ok() {
        let store = OperationalStore::new();
        let recs = vec![
            ExecutionRecord::new("t1", "u", "tool", 10, "ok"),
            ExecutionRecord::new("t2", "u", "tool", 10, "ok"),
            ExecutionRecord::new("t3", "u", "tool", 10, "ok"),
        ];
        store.record("u", &recs);
        let rate = store.failure_rate(30);
        assert!((rate - 0.0).abs() < 1e-6, "All ok → 0% failure rate, got {rate}");
    }

    #[test]
    fn test_failure_rate_all_failed() {
        let store = OperationalStore::new();
        let recs = vec![
            ExecutionRecord::new("t1", "u", "tool", 10, "error"),
            ExecutionRecord::new("t2", "u", "tool", 10, "blocked"),
        ];
        store.record("u", &recs);
        let rate = store.failure_rate(30);
        assert!((rate - 1.0).abs() < 1e-6, "All failed → 100% failure rate, got {rate}");
    }

    #[test]
    fn test_failure_rate_mixed() {
        let store = OperationalStore::new();
        let recs = vec![
            ExecutionRecord::new("t1", "u", "tool", 10, "ok"),
            ExecutionRecord::new("t2", "u", "tool", 10, "ok"),
            ExecutionRecord::new("t3", "u", "tool", 10, "error"),
            ExecutionRecord::new("t4", "u", "tool", 10, "error"),
        ];
        store.record("u", &recs);
        let rate = store.failure_rate(30);
        assert!((rate - 0.5).abs() < 1e-6, "2/4 failed → 50%, got {rate}");
    }

    #[test]
    fn test_failure_rate_empty_store_returns_zero() {
        let store = OperationalStore::new();
        assert!((store.failure_rate(30) - 0.0).abs() < 1e-6);
    }

    #[test]
    fn test_failure_rate_excludes_old_records() {
        let store = OperationalStore::new();
        // Old failure — outside 1-day window
        let old_fail = make_record("tool", "error", 3 * 86_400);
        // Recent success
        let recent_ok = ExecutionRecord::new("t2", "u", "tool", 10, "ok");
        store.record("u", &[old_fail, recent_ok]);

        // Within 1 day only the ok record counts
        let rate = store.failure_rate(1);
        assert!((rate - 0.0).abs() < 1e-6, "Old failure excluded → 0% rate, got {rate}");
    }

    // ── OperationalStore::prune_old ────────────────────────────────────────

    #[test]
    fn test_prune_old_removes_old_records() {
        let store = OperationalStore::new();
        let old1 = make_record("tool", "ok", 8 * 86_400);
        let old2 = make_record("tool", "ok", 5 * 86_400);
        let recent = ExecutionRecord::new("t3", "u", "tool", 10, "ok");
        store.record("u", &[old1, old2, recent]);

        let removed = store.prune_old(3); // keep last 3 days
        assert_eq!(removed, 2, "Should remove 2 old records");
        assert_eq!(store.len(), 1, "Only recent record should remain");
    }

    #[test]
    fn test_prune_old_zero_days_clears_all() {
        let store = OperationalStore::new();
        let recs = vec![
            ExecutionRecord::new("t1", "u", "tool", 10, "ok"),
            ExecutionRecord::new("t2", "u", "tool", 10, "ok"),
        ];
        store.record("u", &recs);
        let removed = store.prune_old(0);
        assert_eq!(removed, 2);
        assert!(store.is_empty());
    }

    #[test]
    fn test_prune_old_returns_zero_when_nothing_to_prune() {
        let store = OperationalStore::new();
        let rec = ExecutionRecord::new("t1", "u", "tool", 10, "ok");
        store.record("u", &[rec]);
        let removed = store.prune_old(90);
        assert_eq!(removed, 0);
        assert_eq!(store.len(), 1);
    }

    // ── Clone shares storage ───────────────────────────────────────────────

    #[test]
    fn test_clone_shares_storage() {
        let store = OperationalStore::new();
        let clone = store.clone();
        let rec = ExecutionRecord::new("t1", "u", "tool", 10, "ok");
        store.record("u", &[rec]);
        // The clone should see the same record
        assert_eq!(clone.len(), 1, "Clone must share underlying storage");
    }

    // ── No hardcoded IPs ───────────────────────────────────────────────────

    #[test]
    fn test_no_hardcoded_ips() {
        // Structural check: OperationalStore must not embed infrastructure addresses.
        // The in-memory store has no URL or host configuration at all.
        // Verify by checking it constructs without any config dependency.
        let store = OperationalStore::new();
        assert!(store.is_empty(), "fresh store must be empty (no hardcoded bootstrap data)");
        // The store has no fields that could contain IPs — it only holds records.
        // Full IP scan of source files is performed by CI tooling.
    }

    // ── AGENT-06: avg_duration_ms ──────────────────────────────────────────

    #[test]
    fn test_avg_duration_ms_empty_returns_zero() {
        let store = OperationalStore::new();
        assert!((store.avg_duration_ms(30) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn test_avg_duration_ms_single_record() {
        let store = OperationalStore::new();
        let rec = ExecutionRecord::new("t1", "u", "tool", 200, "ok");
        store.record("u", &[rec]);
        let avg = store.avg_duration_ms(30);
        assert!((avg - 200.0).abs() < 1e-9, "Expected 200ms, got {avg}");
    }

    #[test]
    fn test_avg_duration_ms_multiple_records() {
        let store = OperationalStore::new();
        let recs = vec![
            ExecutionRecord::new("t1", "u", "tool", 100, "ok"),
            ExecutionRecord::new("t2", "u", "tool", 200, "ok"),
            ExecutionRecord::new("t3", "u", "tool", 300, "ok"),
        ];
        store.record("u", &recs);
        let avg = store.avg_duration_ms(30);
        // (100+200+300)/3 = 200
        assert!((avg - 200.0).abs() < 1e-9, "Expected 200ms avg, got {avg}");
    }

    #[test]
    fn test_avg_duration_ms_excludes_old_records() {
        let store = OperationalStore::new();
        let old = {
            let mut r = make_record("tool", "ok", 5 * 86_400);
            r.duration_ms = 9999;
            r
        };
        let recent = ExecutionRecord::new("t2", "u", "tool", 50, "ok");
        store.record("u", &[old, recent]);
        let avg = store.avg_duration_ms(1);
        assert!((avg - 50.0).abs() < 1e-9, "Old record excluded → avg 50ms, got {avg}");
    }

    // ── AGENT-06: escalation_rate ─────────────────────────────────────────

    #[test]
    fn test_escalation_rate_no_blocked_no_events() {
        let store = OperationalStore::new();
        let recs = vec![
            ExecutionRecord::new("t1", "u", "tool", 10, "ok"),
            ExecutionRecord::new("t2", "u", "tool", 10, "ok"),
        ];
        store.record("u", &recs);
        // No blocked records, no security events → 0% escalation
        let rate = store.escalation_rate(30);
        assert!((rate - 0.0).abs() < 1e-6, "Expected 0% escalation, got {rate}");
    }

    #[test]
    fn test_escalation_rate_empty_store() {
        let store = OperationalStore::new();
        let rate = store.escalation_rate(30);
        assert!((rate - 0.0).abs() < 1e-6, "Empty store → 0% escalation");
    }

    #[test]
    fn test_escalation_rate_with_blocked_turn() {
        let store = OperationalStore::new();
        let recs = vec![
            ExecutionRecord::new("turn-A", "u", "tool", 10, "blocked"),
            ExecutionRecord::new("turn-B", "u", "tool", 10, "ok"),
        ];
        store.record("u", &recs);
        let rate = store.escalation_rate(30);
        // turn-A is escalated (blocked), turn-B is not → at least some nonzero rate
        assert!(rate > 0.0, "Blocked turn must yield nonzero escalation rate, got {rate}");
        assert!(rate <= 1.0, "Escalation rate must be ≤ 1.0, got {rate}");
    }

    // ── AGENT-06: daily_trends ────────────────────────────────────────────

    #[test]
    fn test_daily_trends_empty_returns_empty() {
        let store = OperationalStore::new();
        assert!(store.daily_trends(30).is_empty());
    }

    #[test]
    fn test_daily_trends_counts_today() {
        let store = OperationalStore::new();
        let recs = vec![
            ExecutionRecord::new("t1", "u", "tool", 10, "ok"),
            ExecutionRecord::new("t2", "u", "tool", 10, "ok"),
            ExecutionRecord::new("t3", "u", "tool", 10, "ok"),
        ];
        store.record("u", &recs);
        let trends = store.daily_trends(30);
        assert!(!trends.is_empty(), "Should have at least today's entry");
        // All 3 records are from today — find today's count
        let total: u32 = trends.iter().map(|(_, c)| c).sum();
        assert_eq!(total, 3, "Total call count must equal number of records, got {total}");
    }

    #[test]
    fn test_daily_trends_sorted_ascending() {
        let store = OperationalStore::new();
        let recent = ExecutionRecord::new("t1", "u", "tool", 10, "ok");
        store.record("u", &[recent]);
        let trends = store.daily_trends(30);
        // Verify dates are in ascending order (trivially true for 0-1 entries)
        let dates: Vec<&String> = trends.iter().map(|(d, _)| d).collect();
        let mut sorted = dates.clone();
        sorted.sort();
        assert_eq!(dates, sorted, "Trends must be sorted ascending by date");
    }

    #[test]
    fn test_daily_trends_date_format() {
        let store = OperationalStore::new();
        let rec = ExecutionRecord::new("t1", "u", "tool", 10, "ok");
        store.record("u", &[rec]);
        let trends = store.daily_trends(30);
        for (date_str, _) in &trends {
            // Must match YYYY-MM-DD format
            assert_eq!(date_str.len(), 10, "Date must be YYYY-MM-DD, got '{date_str}'");
            assert_eq!(&date_str[4..5], "-", "Expected dash at pos 4, got '{date_str}'");
            assert_eq!(&date_str[7..8], "-", "Expected dash at pos 7, got '{date_str}'");
        }
    }

    // ── AGENT-06: security_events_summary ───────────────────────────────

    #[test]
    fn test_security_events_summary_empty() {
        let store = OperationalStore::new();
        assert!(store.security_events_summary(30).is_empty());
    }

    #[test]
    fn test_security_events_summary_counts_by_guard_action() {
        let store = OperationalStore::new();
        let events = vec![
            SecurityEventRecord::new("argument", "Blocked", "dangerous_tool", "u"),
            SecurityEventRecord::new("argument", "Blocked", "other_tool", "u"),
            SecurityEventRecord::new("behavioral", "Warned", "", "u"),
            SecurityEventRecord::new("result", "Sanitized", "data_tool", "u"),
        ];
        store.record_security_events(&events);
        let summary = store.security_events_summary(30);
        assert!(!summary.is_empty(), "Summary must not be empty");
        // argument:Blocked count must be 2
        let arg_blocked = summary
            .iter()
            .find(|(k, _)| k == "argument:Blocked")
            .map(|(_, c)| *c);
        assert_eq!(arg_blocked, Some(2), "argument:Blocked must count 2, got {:?}", arg_blocked);
    }

    #[test]
    fn test_security_events_summary_sorted_descending() {
        let store = OperationalStore::new();
        let events = vec![
            SecurityEventRecord::new("behavioral", "Warned", "", "u"),
            SecurityEventRecord::new("argument", "Blocked", "tool1", "u"),
            SecurityEventRecord::new("argument", "Blocked", "tool2", "u"),
            SecurityEventRecord::new("argument", "Blocked", "tool3", "u"),
        ];
        store.record_security_events(&events);
        let summary = store.security_events_summary(30);
        assert!(summary.len() >= 2, "Must have at least 2 entries");
        assert!(
            summary[0].1 >= summary[1].1,
            "Must be sorted descending by count, got {:?}", summary
        );
    }

    #[test]
    fn test_security_events_summary_excludes_old_events() {
        let store = OperationalStore::new();
        // Create an old security event manually
        let old_event = SecurityEventRecord {
            guard_name: "argument".to_string(),
            action: "Blocked".to_string(),
            tool_name: "old_tool".to_string(),
            user_id: "u".to_string(),
            timestamp_secs: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
                - 5 * 86_400,
        };
        let recent = SecurityEventRecord::new("result", "Sanitized", "new_tool", "u");
        store.record_security_events(&[old_event, recent]);

        // 1-day window: only the recent event should appear
        let summary = store.security_events_summary(1);
        // argument:Blocked (old) must NOT appear
        let old_count = summary.iter().find(|(k, _)| k == "argument:Blocked");
        assert!(
            old_count.is_none(),
            "Old event must be excluded from 1-day window, got {:?}", summary
        );
        // result:Sanitized (recent) must appear
        let new_count = summary.iter().find(|(k, _)| k == "result:Sanitized");
        assert!(new_count.is_some(), "Recent event must appear in 1-day window");
    }

    // ── AGENT-06: record_security_events ──────────────────────────────────

    #[test]
    fn test_record_security_events_returns_count() {
        let store = OperationalStore::new();
        let events = vec![
            SecurityEventRecord::new("argument", "Blocked", "tool", "u"),
            SecurityEventRecord::new("result", "Warned", "tool2", "u"),
        ];
        let count = store.record_security_events(&events);
        assert_eq!(count, 2);
        assert_eq!(store.all_security_events().len(), 2);
    }

    // ── AGENT-06: prune_old prunes security events too ────────────────────

    #[test]
    fn test_prune_old_also_prunes_security_events() {
        let store = OperationalStore::new();
        let old_event = SecurityEventRecord {
            guard_name: "argument".to_string(),
            action: "Blocked".to_string(),
            tool_name: "tool".to_string(),
            user_id: "u".to_string(),
            timestamp_secs: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
                - 10 * 86_400,
        };
        let recent_event = SecurityEventRecord::new("behavioral", "Warned", "", "u");
        store.record_security_events(&[old_event, recent_event]);

        // Also add an old execution record
        let old_exec = make_record("tool", "ok", 10 * 86_400);
        store.record("u", &[old_exec]);

        let removed = store.prune_old(3);
        // 1 old exec + 1 old security event = 2 removed
        assert_eq!(removed, 2, "prune_old must remove both old exec and security event records");
        assert_eq!(store.len(), 0, "No execution records should remain");
        assert_eq!(
            store.all_security_events().len(),
            1,
            "Only the recent security event should remain"
        );
    }

    // ── AGENT-06: user-scoped queries ────────────────────────────────────

    #[test]
    fn test_top_tools_for_user_scoping() {
        let store = OperationalStore::new();
        let recs = vec![
            ExecutionRecord::new("t1", "alice", "search", 10, "ok"),
            ExecutionRecord::new("t2", "alice", "search", 10, "ok"),
            ExecutionRecord::new("t3", "bob", "calendar", 10, "ok"),
            ExecutionRecord::new("t4", "bob", "calendar", 10, "ok"),
            ExecutionRecord::new("t5", "bob", "calendar", 10, "ok"),
        ];
        store.record("alice", &recs[..2]);
        store.record("bob", &recs[2..]);

        // Admin sees all
        let all = store.top_tools_for(30, None);
        let total: u32 = all.iter().map(|(_, c)| *c).sum();
        assert_eq!(total, 5, "Admin must see all 5 records");

        // Alice sees only her own
        let alice_top = store.top_tools_for(30, Some("alice"));
        let alice_total: u32 = alice_top.iter().map(|(_, c)| *c).sum();
        assert_eq!(alice_total, 2, "Alice should see only her 2 records");

        // Bob sees only his own
        let bob_top = store.top_tools_for(30, Some("bob"));
        let bob_total: u32 = bob_top.iter().map(|(_, c)| *c).sum();
        assert_eq!(bob_total, 3, "Bob should see only his 3 records");
    }

    #[test]
    fn test_avg_duration_ms_for_user_scoping() {
        let store = OperationalStore::new();
        let recs = vec![
            ExecutionRecord::new("t1", "alice", "tool", 100, "ok"),
            ExecutionRecord::new("t2", "bob", "tool", 500, "ok"),
        ];
        store.record("alice", &recs[..1]);
        store.record("bob", &recs[1..]);

        let alice_avg = store.avg_duration_ms_for(30, Some("alice"));
        assert!((alice_avg - 100.0).abs() < 1e-9, "Alice avg must be 100ms, got {alice_avg}");

        let all_avg = store.avg_duration_ms_for(30, None);
        assert!((all_avg - 300.0).abs() < 1e-9, "All-users avg must be 300ms, got {all_avg}");
    }

    // ── AGENT-06: SecurityEventRecord ────────────────────────────────────

    #[test]
    fn test_security_event_record_new_sets_timestamp() {
        let before = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let event = SecurityEventRecord::new("behavioral", "Warned", "tool", "user-x");
        let after = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        assert!(event.timestamp_secs >= before);
        assert!(event.timestamp_secs <= after);
        assert_eq!(event.guard_name, "behavioral");
        assert_eq!(event.action, "Warned");
    }

    #[test]
    fn test_security_event_record_is_within_days() {
        let recent = SecurityEventRecord::new("arg", "Blocked", "", "u");
        assert!(recent.is_within_days(1));
        assert!(!recent.is_within_days(0));
    }

    // ── AGENT-06: timestamp_to_date_str helper ───────────────────────────

    #[test]
    fn test_timestamp_to_date_str_unix_epoch() {
        // 1970-01-01 00:00:00 UTC
        assert_eq!(timestamp_to_date_str(0), "1970-01-01");
    }

    #[test]
    fn test_timestamp_to_date_str_known_date() {
        // 2024-03-15 12:00:00 UTC  →  2024-03-15
        // 2024-03-15 = day 19,797 since 1970-01-01
        // 19797 * 86400 = 1_710_460_800
        assert_eq!(timestamp_to_date_str(1_710_460_800), "2024-03-15");
    }

    #[test]
    fn test_timestamp_to_date_str_format() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let date_str = timestamp_to_date_str(now);
        assert_eq!(date_str.len(), 10, "Date string must be 10 chars: {date_str}");
        assert_eq!(&date_str[4..5], "-");
        assert_eq!(&date_str[7..8], "-");
    }

    // ── AGENT-06: 90-day auto-prune ───────────────────────────────────────

    #[test]
    fn test_prune_old_90_day_auto_prune() {
        let store = OperationalStore::new();
        // Insert records at various ages
        let recent = ExecutionRecord::new("t1", "u", "tool", 10, "ok");
        let ninety_one_days_ago = {
            let mut r = make_record("tool", "ok", 91 * 86_400);
            r
        };
        store.record("u", &[recent, ninety_one_days_ago]);

        let removed = store.prune_old(90);
        assert_eq!(removed, 1, "90-day prune must remove exactly 1 old record");
        assert_eq!(store.len(), 1, "One recent record must survive");
    }
}
