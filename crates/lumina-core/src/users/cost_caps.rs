//! P2-16: Per-user inference cost caps.
//!
//! Tracks per-user daily turn counts and deep-model escalation counts.
//! Enforces configurable per-role default limits and per-user overrides.
//!
//! ## Design
//! - Counters are stored in a SQLCipher database (shared with users.db by default).
//! - On first turn of a new day (based on user's local timezone), counters reset.
//! - Admin users are unlimited by default.
//! - Regular (Member) users default to 200 turns/day, 50 deep escalations/day.
//! - Guest users default to 20 turns/day, 5 deep escalations/day.
//! - Per-user overrides can be set via CLI or admin commands.
//!
//! ## Deep escalation policy
//! When a user's deep budget is exhausted, the request is NOT rejected — it
//! degrades to the fast model instead. Only total turn limit causes rejection.

use crate::error::{LuminaError, Result};
use crate::users::UserRole;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ── Default limits by role ─────────────────────────────────────────────────

/// Default daily turn limit for Member users. Admin = unlimited (None).
pub const DEFAULT_MEMBER_DAILY_TURNS: u64 = 200;
/// Default daily deep-escalation limit for Member users.
pub const DEFAULT_MEMBER_DAILY_DEEP: u64 = 50;
/// Default daily turn limit for Guest users.
pub const DEFAULT_GUEST_DAILY_TURNS: u64 = 20;
/// Default daily deep-escalation limit for Guest users.
pub const DEFAULT_GUEST_DAILY_DEEP: u64 = 5;

// ── Public types ───────────────────────────────────────────────────────────

/// Result of a budget check.
#[derive(Debug, Clone, PartialEq)]
pub enum BudgetStatus {
    /// Under limit — request is allowed.
    Ok,
    /// Daily turn limit exceeded — request must be rejected.
    TurnLimitExceeded,
    /// Deep-model budget exhausted — caller should degrade to fast model.
    DeepBudgetExhausted,
}

/// Per-user cost/budget limits stored in the database.
///
/// `None` means "unlimited" (used for Admin by default).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UserCostLimit {
    /// User UUID this limit applies to.
    pub user_id: String,
    /// Max total turns per day. `None` = unlimited.
    pub daily_turn_limit: Option<u64>,
    /// Max deep-model escalations per day. `None` = unlimited.
    pub daily_deep_limit: Option<u64>,
}

impl UserCostLimit {
    /// Build the default limit for a given role.
    pub fn default_for_role(user_id: &str, role: &UserRole) -> Self {
        match role {
            UserRole::Admin => Self {
                user_id: user_id.to_string(),
                daily_turn_limit: None,   // unlimited
                daily_deep_limit: None,   // unlimited
            },
            UserRole::Member => Self {
                user_id: user_id.to_string(),
                daily_turn_limit: Some(DEFAULT_MEMBER_DAILY_TURNS),
                daily_deep_limit: Some(DEFAULT_MEMBER_DAILY_DEEP),
            },
            UserRole::Guest => Self {
                user_id: user_id.to_string(),
                daily_turn_limit: Some(DEFAULT_GUEST_DAILY_TURNS),
                daily_deep_limit: Some(DEFAULT_GUEST_DAILY_DEEP),
            },
        }
    }
}

/// Daily usage counters for one user on one date.
#[derive(Debug, Clone, PartialEq)]
pub struct DailyUsage {
    pub user_id: String,
    /// ISO-8601 date, e.g. "2026-06-05".
    pub date: String,
    /// Total turns used today.
    pub turns: u64,
    /// Deep-model escalations used today.
    pub deep_turns: u64,
}

// ── UserCostTracker ────────────────────────────────────────────────────────

/// SQLCipher-backed tracker for per-user daily inference budgets.
///
/// Uses the same database connection pattern as `UserStore` — pass a
/// `rusqlite::Connection` that has already had the cipher key set.
pub struct UserCostTracker {
    conn: Connection,
}

impl UserCostTracker {
    /// Open (or create) a cost-tracking database at `db_path` with `key`.
    ///
    /// Creates the schema on first use.
    pub fn new(db_path: &Path, key: &[u8]) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                LuminaError::Config(format!("Cannot create cost-caps database directory: {}", e))
            })?;
        }

        let conn = Connection::open(db_path).map_err(|e| {
            LuminaError::Config(format!("Cannot open cost-caps database: {}", e))
        })?;

        let hex_key = hex::encode(key);
        conn.execute_batch(&format!("PRAGMA key = \"x'{}'\";", hex_key))
            .map_err(|e| {
                LuminaError::Config(format!("Failed to set cost-caps database key: {}", e))
            })?;

        conn.execute_batch("SELECT count(*) FROM sqlite_master;")
            .map_err(|_| {
                LuminaError::SecurityViolation(
                    "Cost-caps database key is incorrect — cannot open database".to_string(),
                )
            })?;

        conn.execute_batch("PRAGMA journal_mode = WAL;").map_err(|e| {
            LuminaError::Config(format!("Failed to enable WAL mode: {}", e))
        })?;

        let tracker = Self { conn };
        tracker.ensure_schema()?;
        Ok(tracker)
    }

    /// Open using the default path (`~/.lumina/cost_caps.db`) and the users DB key.
    ///
    /// Reuses the same key as the users database for simplicity.
    pub fn open_default() -> Result<Self> {
        let db_path = default_db_path();
        let key = crate::users::get_or_create_users_key()?;
        Self::new(&db_path, &key)
    }

    // ── Budget check ───────────────────────────────────────────────────────

    /// Check whether a user may proceed with a turn.
    ///
    /// - If this user has a stored limit, it is used.
    /// - Otherwise, `role` determines the default limit.
    /// - If today's date (from `date_str`) differs from the stored date, counters
    ///   are reset automatically (lazy daily reset).
    ///
    /// Returns `BudgetStatus::Ok` if the user is under their limit,
    /// `BudgetStatus::TurnLimitExceeded` if the total turn limit is reached, or
    /// `BudgetStatus::DeepBudgetExhausted` if only the deep budget is exhausted
    /// (caller should degrade to fast model).
    pub fn check_budget(
        &self,
        user_id: &str,
        role: &UserRole,
        is_deep: bool,
        date_str: &str,
    ) -> Result<BudgetStatus> {
        // Admin is always unlimited — fast path.
        if *role == UserRole::Admin {
            return Ok(BudgetStatus::Ok);
        }

        let limit = self.get_limit(user_id, role)?;
        let usage = self.get_or_reset_usage(user_id, date_str)?;

        // Check total turn limit first.
        if let Some(max_turns) = limit.daily_turn_limit {
            if usage.turns >= max_turns {
                return Ok(BudgetStatus::TurnLimitExceeded);
            }
        }

        // Check deep budget only when this is a deep escalation request.
        if is_deep {
            if let Some(max_deep) = limit.daily_deep_limit {
                if usage.deep_turns >= max_deep {
                    return Ok(BudgetStatus::DeepBudgetExhausted);
                }
            }
        }

        Ok(BudgetStatus::Ok)
    }

    /// Record a completed turn for a user.
    ///
    /// Increments `turns` always. Also increments `deep_turns` when `was_deep`
    /// is true. Creates the row for today if it does not exist.
    ///
    /// This is a single atomic UPSERT — safe under concurrent connections because
    /// SQLite's WAL mode serialises writes and `turns + 1` is computed by the DB
    /// engine, not by the caller. Two concurrent calls each produce exactly +1.
    pub fn record_turn(&self, user_id: &str, was_deep: bool, date_str: &str) -> Result<()> {
        let deep_delta: i64 = if was_deep { 1 } else { 0 };
        self.conn.execute(
            "INSERT INTO user_cost_usage (user_id, date, turns, deep_turns)
             VALUES (?1, ?2, 1, ?3)
             ON CONFLICT(user_id, date) DO UPDATE SET
               turns      = turns      + 1,
               deep_turns = deep_turns + ?3",
            params![user_id, date_str, deep_delta],
        ).map_err(|e| LuminaError::Config(format!("record_turn upsert failed: {}", e)))?;

        Ok(())
    }

    // ── Limit management ───────────────────────────────────────────────────

    /// Get the stored limit for a user, or return the role default.
    pub fn get_limit(&self, user_id: &str, role: &UserRole) -> Result<UserCostLimit> {
        let row: Option<(Option<i64>, Option<i64>)> = self.conn.query_row(
            "SELECT daily_turn_limit, daily_deep_limit FROM user_cost_limits WHERE user_id = ?1",
            params![user_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ).optional()
         .map_err(|e| LuminaError::Config(format!("get_limit query failed: {}", e)))?;

        Ok(match row {
            Some((turn_limit, deep_limit)) => UserCostLimit {
                user_id: user_id.to_string(),
                daily_turn_limit: turn_limit.map(|v| v as u64),
                daily_deep_limit: deep_limit.map(|v| v as u64),
            },
            None => UserCostLimit::default_for_role(user_id, role),
        })
    }

    /// Set a per-user limit override. Pass `None` for a field to mean "unlimited".
    pub fn set_limit(
        &self,
        user_id: &str,
        daily_turn_limit: Option<u64>,
        daily_deep_limit: Option<u64>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO user_cost_limits (user_id, daily_turn_limit, daily_deep_limit)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(user_id) DO UPDATE SET
               daily_turn_limit = excluded.daily_turn_limit,
               daily_deep_limit = excluded.daily_deep_limit",
            params![
                user_id,
                daily_turn_limit.map(|v| v as i64),
                daily_deep_limit.map(|v| v as i64),
            ],
        ).map_err(|e| LuminaError::Config(format!("set_limit failed: {}", e)))?;
        Ok(())
    }

    /// Remove a per-user limit override (revert to role default).
    pub fn remove_limit(&self, user_id: &str) -> Result<bool> {
        let n = self.conn.execute(
            "DELETE FROM user_cost_limits WHERE user_id = ?1",
            params![user_id],
        ).map_err(|e| LuminaError::Config(format!("remove_limit failed: {}", e)))?;
        Ok(n > 0)
    }

    // ── Usage queries ──────────────────────────────────────────────────────

    /// Get today's usage for a user. Returns zeroed counters if no record exists.
    pub fn get_usage(&self, user_id: &str, date_str: &str) -> Result<DailyUsage> {
        let row: Option<(i64, i64)> = self.conn.query_row(
            "SELECT turns, deep_turns FROM user_cost_usage WHERE user_id = ?1 AND date = ?2",
            params![user_id, date_str],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ).optional()
         .map_err(|e| LuminaError::Config(format!("get_usage query failed: {}", e)))?;

        Ok(match row {
            Some((turns, deep_turns)) => DailyUsage {
                user_id: user_id.to_string(),
                date: date_str.to_string(),
                turns: turns as u64,
                deep_turns: deep_turns as u64,
            },
            None => DailyUsage {
                user_id: user_id.to_string(),
                date: date_str.to_string(),
                turns: 0,
                deep_turns: 0,
            },
        })
    }

    /// Format a usage summary for a user (for CLI / Matrix `/admin usage` command).
    pub fn usage_summary(
        &self,
        user_id: &str,
        role: &UserRole,
        date_str: &str,
    ) -> Result<String> {
        let usage = self.get_usage(user_id, date_str)?;
        let limit = self.get_limit(user_id, role)?;

        let turn_str = match limit.daily_turn_limit {
            Some(max) => format!("{}/{}", usage.turns, max),
            None => format!("{}/∞", usage.turns),
        };
        let deep_str = match limit.daily_deep_limit {
            Some(max) => format!("{}/{}", usage.deep_turns, max),
            None => format!("{}/∞", usage.deep_turns),
        };

        Ok(format!(
            "turns: {} | deep: {} | date: {}",
            turn_str, deep_str, date_str
        ))
    }

    // ── Private helpers ────────────────────────────────────────────────────

    /// Get or auto-reset usage for a user.
    ///
    /// If the most recent stored date differs from `date_str`, the counters
    /// for that old date are kept (for history) and a fresh row for today is
    /// returned with zeroed counters.
    fn get_or_reset_usage(&self, user_id: &str, date_str: &str) -> Result<DailyUsage> {
        self.get_usage(user_id, date_str)
    }

    fn ensure_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS user_cost_usage (
                user_id     TEXT NOT NULL,
                date        TEXT NOT NULL,   -- YYYY-MM-DD
                turns       INTEGER NOT NULL DEFAULT 0,
                deep_turns  INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (user_id, date)  -- composite PK already covers this lookup
            );

            CREATE TABLE IF NOT EXISTS user_cost_limits (
                user_id             TEXT PRIMARY KEY,
                daily_turn_limit    INTEGER,  -- NULL = unlimited
                daily_deep_limit    INTEGER   -- NULL = unlimited
            );",
        ).map_err(|e| LuminaError::Config(format!("cost_caps schema creation failed: {}", e)))?;
        Ok(())
    }
}

// ── Path helpers ───────────────────────────────────────────────────────────

/// Default path for the cost-caps database: `~/.lumina/cost_caps.db`.
pub fn default_db_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".lumina")
        .join("cost_caps.db")
}

// ── Date helper (no chrono) ────────────────────────────────────────────────

/// Return today's UTC date as "YYYY-MM-DD".
///
/// Uses `std::time` only — no external crate dependency.
///
/// # Timezone note
/// Budget counters reset at UTC midnight. The spec calls for user-local-time
/// resets (edge case: "Timezone for day boundary → use user's configured
/// timezone"). Full timezone support requires threading `user_settings` through
/// and a chrono/time dependency. This implementation uses UTC for all users as
/// a pragmatic simplification — the deviation is documented here and tracked for
/// a future enhancement. In practice, UTC midnight ≤ 12 hours off any user's
/// local midnight, which is acceptable for a daily inference budget.
pub fn today_utc() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = secs / 86400;
    let (year, month, day) = days_to_ymd(days);
    format!("{:04}-{:02}-{:02}", year, month, day)
}

/// Convert days-since-Unix-epoch to (year, month, day).
fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    let mut year = 1970u64;
    loop {
        let year_days = if is_leap(year) { 366 } else { 365 };
        if days < year_days {
            break;
        }
        days -= year_days;
        year += 1;
    }
    let month_days: [u64; 12] = [
        31,
        if is_leap(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1u64;
    for &md in &month_days {
        if days < md {
            break;
        }
        days -= md;
        month += 1;
    }
    (year, month, days + 1)
}

fn is_leap(year: u64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_key() -> Vec<u8> {
        vec![0xABu8; 32]
    }

    fn tmp_db(name: &str) -> PathBuf {
        let p = PathBuf::from(format!("/tmp/lumina_cost_caps_test_{}.db", name));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn open(name: &str) -> UserCostTracker {
        UserCostTracker::new(&tmp_db(name), &test_key()).expect("open UserCostTracker")
    }

    fn cleanup(name: &str) {
        let _ = std::fs::remove_file(tmp_db(name));
    }

    const TODAY: &str = "2026-06-05";
    const TOMORROW: &str = "2026-06-06";

    // ── Schema / open ──────────────────────────────────────────────────────

    #[test]
    fn test_open_creates_schema() {
        let tracker = open("open_schema");
        // Verify we can call get_usage without error (schema exists).
        let usage = tracker.get_usage("user-1", TODAY).unwrap();
        assert_eq!(usage.turns, 0);
        assert_eq!(usage.deep_turns, 0);
        cleanup("open_schema");
    }

    // ── Default limits by role ─────────────────────────────────────────────

    #[test]
    fn test_admin_default_limit_is_unlimited() {
        let limit = UserCostLimit::default_for_role("admin-1", &UserRole::Admin);
        assert!(limit.daily_turn_limit.is_none());
        assert!(limit.daily_deep_limit.is_none());
    }

    #[test]
    fn test_member_default_limits() {
        let limit = UserCostLimit::default_for_role("member-1", &UserRole::Member);
        assert_eq!(limit.daily_turn_limit, Some(DEFAULT_MEMBER_DAILY_TURNS));
        assert_eq!(limit.daily_deep_limit, Some(DEFAULT_MEMBER_DAILY_DEEP));
    }

    #[test]
    fn test_guest_default_limits() {
        let limit = UserCostLimit::default_for_role("guest-1", &UserRole::Guest);
        assert_eq!(limit.daily_turn_limit, Some(DEFAULT_GUEST_DAILY_TURNS));
        assert_eq!(limit.daily_deep_limit, Some(DEFAULT_GUEST_DAILY_DEEP));
    }

    // ── Budget check — Admin ───────────────────────────────────────────────

    #[test]
    fn test_admin_always_ok() {
        let tracker = open("admin_ok");
        // Even if admin somehow had a usage entry, check_budget returns Ok.
        tracker.record_turn("admin-1", true, TODAY).unwrap();
        let status = tracker.check_budget("admin-1", &UserRole::Admin, true, TODAY).unwrap();
        assert_eq!(status, BudgetStatus::Ok);
        cleanup("admin_ok");
    }

    // ── Budget check — Member under limit ─────────────────────────────────

    #[test]
    fn test_member_under_limit_ok() {
        let tracker = open("member_under");
        // Record 5 turns — well under the 200 default.
        for _ in 0..5 {
            tracker.record_turn("user-1", false, TODAY).unwrap();
        }
        let status = tracker.check_budget("user-1", &UserRole::Member, false, TODAY).unwrap();
        assert_eq!(status, BudgetStatus::Ok);
        cleanup("member_under");
    }

    // ── Budget check — Turn limit reached ─────────────────────────────────

    #[test]
    fn test_turn_limit_exceeded() {
        let tracker = open("turn_exceeded");
        // Set a low limit of 3 turns.
        tracker.set_limit("user-1", Some(3), Some(10)).unwrap();
        for _ in 0..3 {
            tracker.record_turn("user-1", false, TODAY).unwrap();
        }
        // At 3 turns, next check should reject.
        let status = tracker.check_budget("user-1", &UserRole::Member, false, TODAY).unwrap();
        assert_eq!(status, BudgetStatus::TurnLimitExceeded);
        cleanup("turn_exceeded");
    }

    #[test]
    fn test_under_limit_not_rejected() {
        let tracker = open("under_limit");
        tracker.set_limit("user-1", Some(5), Some(10)).unwrap();
        for _ in 0..4 {
            tracker.record_turn("user-1", false, TODAY).unwrap();
        }
        // At 4 turns with limit 5, should still be Ok.
        let status = tracker.check_budget("user-1", &UserRole::Member, false, TODAY).unwrap();
        assert_eq!(status, BudgetStatus::Ok);
        cleanup("under_limit");
    }

    // ── Budget check — Deep model budget exhausted ─────────────────────────

    #[test]
    fn test_deep_budget_exhausted_degrades_not_rejects() {
        let tracker = open("deep_exhausted");
        // Set deep limit to 2.
        tracker.set_limit("user-1", Some(100), Some(2)).unwrap();
        tracker.record_turn("user-1", true, TODAY).unwrap();
        tracker.record_turn("user-1", true, TODAY).unwrap();
        // Now at deep limit — deep request should return DeepBudgetExhausted.
        let status = tracker.check_budget("user-1", &UserRole::Member, true, TODAY).unwrap();
        assert_eq!(status, BudgetStatus::DeepBudgetExhausted);
        cleanup("deep_exhausted");
    }

    #[test]
    fn test_deep_budget_exhausted_does_not_block_fast_turns() {
        let tracker = open("deep_exhausted_fast_ok");
        tracker.set_limit("user-1", Some(100), Some(1)).unwrap();
        tracker.record_turn("user-1", true, TODAY).unwrap();
        // Deep budget exhausted, but a non-deep request should still be Ok.
        let status = tracker.check_budget("user-1", &UserRole::Member, false, TODAY).unwrap();
        assert_eq!(status, BudgetStatus::Ok);
        cleanup("deep_exhausted_fast_ok");
    }

    // ── Daily reset ────────────────────────────────────────────────────────

    #[test]
    fn test_daily_reset_new_day_starts_at_zero() {
        let tracker = open("daily_reset");
        tracker.set_limit("user-1", Some(3), Some(10)).unwrap();
        // Use up all turns on TODAY.
        for _ in 0..3 {
            tracker.record_turn("user-1", false, TODAY).unwrap();
        }
        let status = tracker.check_budget("user-1", &UserRole::Member, false, TODAY).unwrap();
        assert_eq!(status, BudgetStatus::TurnLimitExceeded);

        // On TOMORROW, counters start fresh — no usage recorded yet.
        let status_tomorrow = tracker.check_budget("user-1", &UserRole::Member, false, TOMORROW).unwrap();
        assert_eq!(status_tomorrow, BudgetStatus::Ok);
        cleanup("daily_reset");
    }

    #[test]
    fn test_usage_separate_per_day() {
        let tracker = open("usage_separate_days");
        tracker.record_turn("user-1", true, TODAY).unwrap();
        tracker.record_turn("user-1", true, TODAY).unwrap();
        tracker.record_turn("user-1", false, TOMORROW).unwrap();

        let today_usage = tracker.get_usage("user-1", TODAY).unwrap();
        let tomorrow_usage = tracker.get_usage("user-1", TOMORROW).unwrap();

        assert_eq!(today_usage.turns, 2);
        assert_eq!(today_usage.deep_turns, 2);
        assert_eq!(tomorrow_usage.turns, 1);
        assert_eq!(tomorrow_usage.deep_turns, 0);
        cleanup("usage_separate_days");
    }

    // ── Per-user limit overrides ───────────────────────────────────────────

    #[test]
    fn test_per_user_limit_override() {
        let tracker = open("limit_override");
        // Default Member limit is 200. Override to 500.
        tracker.set_limit("power-user", Some(500), Some(100)).unwrap();
        let limit = tracker.get_limit("power-user", &UserRole::Member).unwrap();
        assert_eq!(limit.daily_turn_limit, Some(500));
        assert_eq!(limit.daily_deep_limit, Some(100));
        cleanup("limit_override");
    }

    #[test]
    fn test_remove_limit_reverts_to_role_default() {
        let tracker = open("remove_limit");
        tracker.set_limit("user-1", Some(999), Some(999)).unwrap();
        tracker.remove_limit("user-1").unwrap();
        let limit = tracker.get_limit("user-1", &UserRole::Member).unwrap();
        assert_eq!(limit.daily_turn_limit, Some(DEFAULT_MEMBER_DAILY_TURNS));
        cleanup("remove_limit");
    }

    #[test]
    fn test_unlimited_override_with_none() {
        let tracker = open("unlimited_override");
        tracker.set_limit("vip-user", None, None).unwrap();
        let limit = tracker.get_limit("vip-user", &UserRole::Member).unwrap();
        assert!(limit.daily_turn_limit.is_none());
        assert!(limit.daily_deep_limit.is_none());
        cleanup("unlimited_override");
    }

    // ── Independent counters per user ──────────────────────────────────────

    #[test]
    fn test_independent_counters_per_user() {
        let tracker = open("independent");
        tracker.set_limit("alice", Some(5), Some(5)).unwrap();
        tracker.set_limit("bob", Some(5), Some(5)).unwrap();

        // alice uses 5 turns (hits limit).
        for _ in 0..5 {
            tracker.record_turn("alice", false, TODAY).unwrap();
        }
        // bob uses 1 turn.
        tracker.record_turn("bob", false, TODAY).unwrap();

        let alice_status = tracker.check_budget("alice", &UserRole::Member, false, TODAY).unwrap();
        let bob_status = tracker.check_budget("bob", &UserRole::Member, false, TODAY).unwrap();

        assert_eq!(alice_status, BudgetStatus::TurnLimitExceeded);
        assert_eq!(bob_status, BudgetStatus::Ok);
        cleanup("independent");
    }

    // ── Usage summary ──────────────────────────────────────────────────────

    #[test]
    fn test_usage_summary_format() {
        let tracker = open("usage_summary");
        tracker.record_turn("user-1", true, TODAY).unwrap();
        let summary = tracker.usage_summary("user-1", &UserRole::Member, TODAY).unwrap();
        // Should contain turns and deep counts.
        assert!(summary.contains("turns:"));
        assert!(summary.contains("deep:"));
        assert!(summary.contains(TODAY));
        cleanup("usage_summary");
    }

    // ── today_utc helper ──────────────────────────────────────────────────

    #[test]
    fn test_today_utc_format() {
        let d = today_utc();
        // Must be YYYY-MM-DD
        assert_eq!(d.len(), 10);
        assert_eq!(&d[4..5], "-");
        assert_eq!(&d[7..8], "-");
    }

    // ── Wrong key rejected ────────────────────────────────────────────────

    #[test]
    fn test_wrong_key_rejected() {
        let path = tmp_db("wrong_key_caps");
        UserCostTracker::new(&path, &test_key()).unwrap();
        let wrong_key = vec![0xFFu8; 32];
        let result = UserCostTracker::new(&path, &wrong_key);
        assert!(result.is_err());
        let _ = std::fs::remove_file(path);
    }
}
