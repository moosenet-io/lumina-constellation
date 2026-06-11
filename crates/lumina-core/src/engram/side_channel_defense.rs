//! ESEC-06: Shared memory side-channel defense.
//!
//! Prevents malicious household members from probing another user's private
//! memories via query patterns, timing inference, or embedding similarity leakage.
//!
//! Three layers of defense:
//! 1. Uniform denial — identical response regardless of whether private data exists
//! 2. Probe detection — tracks cross-user query frequency; flags sustained probing
//! 3. Query sanitization — strips other users' identifiers before memory search
//! 4. Timing jitter — adds random 50-200ms delay to normalize response latency

use std::collections::HashMap;
use std::time::{Duration, Instant};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Uniform denial message returned for any cross-user memory query.
///
/// Intentionally generic: does NOT reveal whether the other user's data exists.
pub const UNIFORM_DENIAL_MSG: &str =
    "I can only access your own memories and shared household information.";

/// Number of cross-user queries within `PROBE_WINDOW_SECS` that triggers a probe flag.
pub const PROBE_THRESHOLD: usize = 3;

/// Sliding window (in seconds) for probe detection.
pub const PROBE_WINDOW_SECS: u64 = 300;

/// Minimum timing jitter in milliseconds.
pub const TIMING_JITTER_MIN_MS: u64 = 50;

/// Maximum timing jitter in milliseconds (exclusive upper bound for the range).
pub const TIMING_JITTER_MAX_MS: u64 = 200;

// ── ProbeStatus ───────────────────────────────────────────────────────────────

/// Result of a probe detection check for a given user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeStatus {
    /// Query frequency is within normal bounds.
    Normal,
    /// User has exceeded the cross-user query threshold.
    PotentialProbe {
        /// How many cross-user queries were observed in the window.
        query_count: usize,
    },
}

// ── SideChannelDefense ────────────────────────────────────────────────────────

/// Stateful guard that enforces side-channel defenses on shared memory access.
///
/// Create one instance per session (or per request handler). Thread safety is
/// the caller's responsibility; wrap in `Mutex` / `RwLock` for shared use.
#[derive(Default)]
pub struct SideChannelDefense {
    /// Maps `user_id` → list of timestamps when that user made a cross-user query.
    probe_tracker: HashMap<String, Vec<Instant>>,
}

impl SideChannelDefense {
    /// Construct a new, empty defense state.
    pub fn new() -> Self {
        Self {
            probe_tracker: HashMap::new(),
        }
    }

    // ── Public API ─────────────────────────────────────────────────────────

    /// Return the uniform denial message.
    ///
    /// Always returns the same static string regardless of whether the queried
    /// data exists. Callers MUST use this instead of constructing custom
    /// "not found" / "access denied" messages that could leak information.
    pub fn uniform_denial() -> &'static str {
        UNIFORM_DENIAL_MSG
    }

    /// Return `true` if `query` references a user other than `requesting_user_id`.
    ///
    /// Checks whether the query text contains any entry from `known_user_ids`
    /// that is NOT the requesting user. Case-insensitive substring match.
    ///
    /// # Arguments
    /// * `query` — the raw query string from the requesting user
    /// * `requesting_user_id` — the authenticated user submitting the query
    /// * `known_user_ids` — full list of user identifiers in the household
    pub fn is_cross_user_query(
        query: &str,
        requesting_user_id: &str,
        known_user_ids: &[&str],
    ) -> bool {
        let query_lower = query.to_lowercase();
        for uid in known_user_ids {
            // Skip the requesting user's own identifier
            if uid.eq_ignore_ascii_case(requesting_user_id) {
                continue;
            }
            if query_lower.contains(&uid.to_lowercase()) {
                return true;
            }
        }
        false
    }

    /// Strip all known user identifiers (except the requester's own) from `query`.
    ///
    /// Returns the sanitized query string. This prevents the embedding search
    /// from accidentally returning another user's private memories simply because
    /// those memories contain the queried name.
    ///
    /// Names are replaced with a single space and the result is trimmed.
    pub fn sanitize_query(query: &str, known_user_ids: &[&str]) -> String {
        let mut sanitized = query.to_string();
        for uid in known_user_ids {
            // Case-insensitive removal: rebuild with case-preserving replacement
            let uid_lower = uid.to_lowercase();
            let mut result = String::with_capacity(sanitized.len());
            let lower = sanitized.to_lowercase();
            let mut cursor = 0;
            while cursor < sanitized.len() {
                if lower[cursor..].starts_with(&uid_lower) {
                    result.push(' ');
                    cursor += uid.len();
                } else {
                    // Push one char at a time (handles multi-byte safely)
                    let ch = sanitized[cursor..].chars().next().unwrap_or(' ');
                    result.push(ch);
                    cursor += ch.len_utf8();
                }
            }
            sanitized = result;
        }
        // Collapse runs of whitespace to a single space and trim
        sanitized
            .split_whitespace()
            .collect::<Vec<&str>>()
            .join(" ")
    }

    /// Record a cross-user query for `user_id` and return the current probe status.
    ///
    /// Prunes timestamps outside the `PROBE_WINDOW_SECS` window before checking.
    /// Returns `ProbeStatus::PotentialProbe` when the count meets or exceeds
    /// `PROBE_THRESHOLD`.
    pub fn check_probe(&mut self, user_id: &str) -> ProbeStatus {
        let now = Instant::now();
        let window = Duration::from_secs(PROBE_WINDOW_SECS);

        let times = self.probe_tracker
            .entry(user_id.to_string())
            .or_default();

        // Record this query
        times.push(now);

        // Prune entries outside the window
        times.retain(|t| now.duration_since(*t) <= window);

        let count = times.len();
        if count >= PROBE_THRESHOLD {
            ProbeStatus::PotentialProbe { query_count: count }
        } else {
            ProbeStatus::Normal
        }
    }

    /// Return a randomized `Duration` between `TIMING_JITTER_MIN_MS` and
    /// `TIMING_JITTER_MAX_MS` milliseconds.
    ///
    /// Uses subsecond nanoseconds from the system clock as an entropy source.
    /// Avoids external crate dependencies (`rand` is not required).
    pub fn jitter_delay() -> Duration {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos();
        let range = TIMING_JITTER_MAX_MS - TIMING_JITTER_MIN_MS;
        let jitter_ms = TIMING_JITTER_MIN_MS + (nanos as u64 % range);
        Duration::from_millis(jitter_ms)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Uniform denial must return the same static string on every call.
    #[test]
    fn test_uniform_denial_consistent() {
        let first = SideChannelDefense::uniform_denial();
        let second = SideChannelDefense::uniform_denial();
        let third = SideChannelDefense::uniform_denial();
        assert_eq!(first, second);
        assert_eq!(second, third);
        assert_eq!(first, UNIFORM_DENIAL_MSG);
        // Must not contain anything that leaks presence/absence of data
        assert!(!first.contains("doesn't have"));
        assert!(!first.contains("no memories"));
        assert!(!first.contains("found"));
    }

    /// Probe detection should flag the user after `PROBE_THRESHOLD` queries.
    #[test]
    fn test_probe_detection_triggers_after_threshold() {
        let mut defense = SideChannelDefense::new();
        let user = "user-tester";

        // First PROBE_THRESHOLD - 1 queries: should be Normal
        for i in 0..(PROBE_THRESHOLD - 1) {
            let status = defense.check_probe(user);
            assert_eq!(
                status,
                ProbeStatus::Normal,
                "Query {} should still be Normal",
                i + 1
            );
        }

        // The threshold-th query triggers PotentialProbe
        let status = defense.check_probe(user);
        assert!(
            matches!(status, ProbeStatus::PotentialProbe { query_count } if query_count >= PROBE_THRESHOLD),
            "Expected PotentialProbe after {} queries, got {:?}",
            PROBE_THRESHOLD,
            status
        );
    }

    /// Query sanitization should remove other users' names/IDs from the query.
    #[test]
    fn test_sanitize_query_strips_user_names() {
        let known_users = &["user-alice", "user-bob", "user-carol"];

        let query = "Does user-alice have any notes about budgets?";
        let sanitized = SideChannelDefense::sanitize_query(query, known_users);
        assert!(
            !sanitized.to_lowercase().contains("user-alice"),
            "user-alice should be removed: {sanitized}"
        );
        assert!(
            sanitized.contains("notes about budgets"),
            "Non-user content should be preserved: {sanitized}"
        );

        // Multiple users in one query
        let multi = "What does user-bob think about what user-carol said?";
        let sanitized_multi = SideChannelDefense::sanitize_query(multi, known_users);
        assert!(!sanitized_multi.to_lowercase().contains("user-bob"));
        assert!(!sanitized_multi.to_lowercase().contains("user-carol"));
    }

    /// Jitter must always fall within [TIMING_JITTER_MIN_MS, TIMING_JITTER_MAX_MS).
    #[test]
    fn test_jitter_within_bounds() {
        for _ in 0..50 {
            let delay = SideChannelDefense::jitter_delay();
            let ms = delay.as_millis() as u64;
            assert!(
                ms >= TIMING_JITTER_MIN_MS && ms < TIMING_JITTER_MAX_MS,
                "Jitter {ms}ms out of bounds [{TIMING_JITTER_MIN_MS}, {TIMING_JITTER_MAX_MS})"
            );
        }
    }

    /// Cross-user query detection should flag queries mentioning other users.
    #[test]
    fn test_cross_user_query_detection() {
        let known_users = &["user-alice", "user-bob"];

        // Query by user-alice that mentions user-bob → cross-user
        assert!(
            SideChannelDefense::is_cross_user_query(
                "Does user-bob have a shellfish allergy?",
                "user-alice",
                known_users,
            ),
            "Should detect cross-user query mentioning user-bob"
        );

        // Query by user-alice about herself → not cross-user
        assert!(
            !SideChannelDefense::is_cross_user_query(
                "What are user-alice's dietary preferences?",
                "user-alice",
                known_users,
            ),
            "Query about self should NOT be flagged as cross-user"
        );

        // Generic query with no user references → not cross-user
        assert!(
            !SideChannelDefense::is_cross_user_query(
                "What groceries do we need this week?",
                "user-alice",
                known_users,
            ),
            "Generic query should NOT be flagged as cross-user"
        );

        // Case-insensitive detection
        assert!(
            SideChannelDefense::is_cross_user_query(
                "What does USER-BOB prefer for breakfast?",
                "user-alice",
                known_users,
            ),
            "Cross-user detection should be case-insensitive"
        );
    }

    /// Two different users are tracked independently by the probe detector.
    #[test]
    fn test_probe_tracker_isolates_users() {
        let mut defense = SideChannelDefense::new();

        // Drive user-a to the threshold
        for _ in 0..PROBE_THRESHOLD {
            defense.check_probe("user-a");
        }

        // user-b should still be at Normal with zero history
        let status_b = defense.check_probe("user-b");
        assert_eq!(
            status_b,
            ProbeStatus::Normal,
            "user-b should be Normal; user-a's queries must not bleed over"
        );
    }

    /// Sanitization of a query that contains no known user IDs returns it unchanged
    /// (modulo whitespace normalization).
    #[test]
    fn test_sanitize_query_no_users_unchanged() {
        let known_users = &["user-alice", "user-bob"];
        let query = "What is the grocery list for this week?";
        let sanitized = SideChannelDefense::sanitize_query(query, known_users);
        assert_eq!(sanitized, query);
    }
}
