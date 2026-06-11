//! P2-11: User settings store — key-value preferences per user.
//!
//! Stores user-configurable settings (timezone, location, calendar URL, etc.)
//! in a SQLCipher-encrypted database separate from the main users store.
//!
//! Schema:
//!   settings(user_id TEXT, key TEXT, value TEXT, updated_at TEXT,
//!            PRIMARY KEY(user_id, key))
//!
//! Sensitive settings (App Passwords, credentials) must NEVER be written
//! via this store — they belong in the vault and are set via CLI only.

use crate::error::{LuminaError, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};

// ── Well-known setting keys ────────────────────────────────────────────────

/// Timezone in IANA format, e.g. `America/Los_Angeles`.
pub const KEY_TIMEZONE: &str = "timezone";
/// Location for weather lookups, e.g. `Oakland, CA`.
pub const KEY_LOCATION: &str = "location";
/// CalDAV calendar URL.
pub const KEY_CALENDAR_URL: &str = "calendar_url";
/// Email address for IMAP.
pub const KEY_EMAIL: &str = "email";
/// Morning briefing time in `HH:MM` 24-hour format, or `"off"`.
pub const KEY_BRIEFING_TIME: &str = "briefing_time";
/// Preferred detail level: `"brief"`, `"normal"`, or `"verbose"`.
pub const KEY_DETAIL_LEVEL: &str = "detail_level";
/// Preferred response language, e.g. `"en"`, `"fr"`.
pub const KEY_LANGUAGE: &str = "language";

/// Keys that must NEVER be set via Matrix (homeserver could log them).
const SENSITIVE_KEYS: &[&str] = &[
    "app_password",
    "password",
    "secret",
    "token",
    "api_key",
    "imap_password",
    "caldav_password",
];

// ── SettingsStore ──────────────────────────────────────────────────────────

/// SQLCipher-backed store for per-user key-value settings.
///
/// Each user's settings are isolated by `user_id`. Keys are arbitrary UTF-8
/// strings; values are stored as text (callers serialize as needed).
pub struct SettingsStore {
    conn: Connection,
}

impl SettingsStore {
    /// Open (or create) the SQLCipher database at `db_path` using `key`.
    ///
    /// Creates the `settings` table on first open. Subsequent opens verify the
    /// key with a harmless query — a wrong key produces a `SecurityViolation`.
    pub fn new(db_path: &Path, key: &[u8]) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                LuminaError::Config(format!("Cannot create settings database directory: {}", e))
            })?;
        }

        let conn = Connection::open(db_path).map_err(|e| {
            LuminaError::Config(format!("Cannot open settings database: {}", e))
        })?;

        // SQLCipher: set key before any other operation.
        let hex_key = hex::encode(key);
        conn.execute_batch(&format!("PRAGMA key = \"x'{}'\";", hex_key))
            .map_err(|e| {
                LuminaError::Config(format!("Failed to set settings database key: {}", e))
            })?;

        // Verify the key is correct.
        conn.execute_batch("SELECT count(*) FROM sqlite_master;")
            .map_err(|_| {
                LuminaError::SecurityViolation(
                    "Settings database key is incorrect — cannot open database".to_string(),
                )
            })?;

        conn.execute_batch("PRAGMA journal_mode = WAL;").map_err(|e| {
            LuminaError::Config(format!("Failed to enable WAL mode: {}", e))
        })?;

        let store = Self { conn };
        store.ensure_schema()?;
        Ok(store)
    }

    /// Open the settings store at the default path (`~/.lumina/settings.db`).
    ///
    /// The key is loaded from the environment variable `LUMINA_SETTINGS_DB_KEY`
    /// (hex-encoded 32 bytes). Returns an error if the variable is absent or
    /// the key cannot be decoded.
    pub fn open_default() -> Result<Self> {
        let db_path = default_settings_db_path();
        let key = key_from_env()?;
        Self::new(&db_path, &key)
    }

    // ── Write operations ───────────────────────────────────────────────────

    /// Set (or update) a setting for `user_id`. The `updated_at` timestamp is
    /// recorded automatically.
    ///
    /// Returns an error if `key` is a sensitive key (those must be set via
    /// CLI / vault, never via this API).
    pub fn set(&self, user_id: &str, key: &str, value: &str) -> Result<()> {
        if is_sensitive(key) {
            return Err(LuminaError::SecurityViolation(format!(
                "Key '{}' is sensitive — set it via the CLI/vault, not via this API.",
                key
            )));
        }
        let updated_at = utc_now();
        self.conn
            .execute(
                "INSERT INTO settings (user_id, key, value, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(user_id, key) DO UPDATE
                     SET value = excluded.value,
                         updated_at = excluded.updated_at",
                params![user_id, key, value, updated_at],
            )
            .map_err(|e| LuminaError::Config(format!("settings set failed: {}", e)))?;
        Ok(())
    }

    /// Retrieve the value for a setting. Returns `None` if the key is not set.
    pub fn get(&self, user_id: &str, key: &str) -> Result<Option<String>> {
        let result = self
            .conn
            .query_row(
                "SELECT value FROM settings WHERE user_id = ?1 AND key = ?2",
                params![user_id, key],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| LuminaError::Config(format!("settings get failed: {}", e)))?;
        Ok(result)
    }

    /// Return all (key, value) pairs for a user, ordered by key.
    pub fn list(&self, user_id: &str) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT key, value FROM settings WHERE user_id = ?1 ORDER BY key ASC",
            )
            .map_err(|e| LuminaError::Config(format!("settings list prepare failed: {}", e)))?;

        let rows = stmt
            .query_map(params![user_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| LuminaError::Config(format!("settings list query failed: {}", e)))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| LuminaError::Config(format!("settings list row error: {}", e)))?;

        Ok(rows)
    }

    /// Delete a single setting for `user_id`. Returns `true` if a row was removed.
    pub fn reset(&self, user_id: &str, key: &str) -> Result<bool> {
        let n = self
            .conn
            .execute(
                "DELETE FROM settings WHERE user_id = ?1 AND key = ?2",
                params![user_id, key],
            )
            .map_err(|e| LuminaError::Config(format!("settings reset failed: {}", e)))?;
        Ok(n > 0)
    }

    /// Delete all settings for `user_id`. Returns the number of rows removed.
    pub fn reset_all(&self, user_id: &str) -> Result<usize> {
        let n = self
            .conn
            .execute(
                "DELETE FROM settings WHERE user_id = ?1",
                params![user_id],
            )
            .map_err(|e| LuminaError::Config(format!("settings reset_all failed: {}", e)))?;
        Ok(n)
    }

    // ── Private helpers ────────────────────────────────────────────────────

    fn ensure_schema(&self) -> Result<()> {
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS settings (
                    user_id    TEXT NOT NULL,
                    key        TEXT NOT NULL,
                    value      TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    PRIMARY KEY (user_id, key)
                );",
            )
            .map_err(|e| LuminaError::Config(format!("Settings schema creation failed: {}", e)))?;
        Ok(())
    }
}

// ── Validation helpers ─────────────────────────────────────────────────────

/// Return `true` if `key` matches a sensitive key exactly or starts/ends with
/// a sensitive word as an underscore-delimited segment.
///
/// For example, `"app_password"` and `"imap_password"` both match the
/// `"password"` segment. But `"password_reset_enabled"` would also match —
/// this is intentional: user-facing keys should never mention passwords/tokens
/// in any position.
fn is_sensitive(key: &str) -> bool {
    let lower = key.to_lowercase();
    // Split on underscores and check each segment for exact match.
    let segments: Vec<&str> = lower.split('_').collect();
    SENSITIVE_KEYS.iter().any(|sensitive| {
        // Match if the whole key equals a sensitive word, or any segment does.
        lower == *sensitive || segments.iter().any(|seg| *seg == *sensitive)
    })
}

/// Validate a timezone string: must be non-empty and contain only safe chars.
///
/// We allow ASCII letters, digits, `/`, `_`, `-`, and `+`. This covers all
/// valid IANA timezone names (e.g. `America/Los_Angeles`, `Etc/GMT+5`) while
/// rejecting shell metacharacters and path traversal sequences.
pub fn validate_timezone(tz: &str) -> Result<()> {
    if tz.is_empty() {
        return Err(LuminaError::Config("Timezone must not be empty.".to_string()));
    }
    // IANA timezone names never contain dots — reject them to block path traversal.
    if !tz.chars().all(|c| c.is_ascii_alphanumeric() || "/_+-".contains(c)) {
        return Err(LuminaError::Config(format!(
            "Timezone '{}' contains invalid characters. \
             Use IANA format, e.g. America/Los_Angeles",
            tz
        )));
    }
    Ok(())
}

/// Validate a briefing time: must be `"off"` or `HH:MM` in 24-hour format.
pub fn validate_briefing_time(time: &str) -> Result<()> {
    if time == "off" {
        return Ok(());
    }
    // Must be HH:MM
    let parts: Vec<&str> = time.split(':').collect();
    if parts.len() != 2 {
        return Err(LuminaError::Config(format!(
            "Invalid briefing time '{}'. Use HH:MM (24-hour) or 'off'.",
            time
        )));
    }
    let hours: u8 = parts[0].parse().map_err(|_| {
        LuminaError::Config(format!(
            "Invalid briefing time '{}'. Hours must be 0-23.",
            time
        ))
    })?;
    let mins: u8 = parts[1].parse().map_err(|_| {
        LuminaError::Config(format!(
            "Invalid briefing time '{}'. Minutes must be 0-59.",
            time
        ))
    })?;
    if hours > 23 || mins > 59 {
        return Err(LuminaError::Config(format!(
            "Invalid briefing time '{}'. Use HH:MM (24-hour) or 'off'.",
            time
        )));
    }
    Ok(())
}

/// Validate a detail level: must be `"brief"`, `"normal"`, or `"verbose"`.
pub fn validate_detail_level(level: &str) -> Result<()> {
    match level {
        "brief" | "normal" | "verbose" => Ok(()),
        other => Err(LuminaError::Config(format!(
            "Unknown detail level '{}'. Valid options: brief, normal, verbose",
            other
        ))),
    }
}

// ── Path and key helpers ───────────────────────────────────────────────────

/// Default path for the settings database: `~/.lumina/settings.db`.
pub fn default_settings_db_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".lumina")
        .join("settings.db")
}

/// Load the settings DB key from env var `LUMINA_SETTINGS_DB_KEY` (hex).
fn key_from_env() -> Result<Vec<u8>> {
    let hex = std::env::var("LUMINA_SETTINGS_DB_KEY").map_err(|_| {
        LuminaError::Config(
            "LUMINA_SETTINGS_DB_KEY is not set. Run `lumina init` to generate keys.".to_string(),
        )
    })?;
    hex::decode(&hex).map_err(|e| {
        LuminaError::Config(format!("LUMINA_SETTINGS_DB_KEY is not valid hex: {}", e))
    })
}

/// Current UTC time as an ISO-8601 string.
fn utc_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let sec = secs % 60;
    let min = (secs / 60) % 60;
    let hour = (secs / 3600) % 24;
    let days = secs / 86400;
    let (year, month, day) = days_to_ymd(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hour, min, sec
    )
}

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
        31, 30, 31, 30, 31, 31, 30, 31, 30, 31,
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

// ── Matrix command handler ─────────────────────────────────────────────────

/// Return the substring of `s` that starts after the first `n` whitespace-delimited
/// words. Returns `None` if `s` contains fewer than `n` words.
///
/// Used to extract the value portion of `!settings set <key> <value>` while
/// preserving spaces within values (e.g. "Oakland, CA").
fn find_nth_word_end(s: &str, n: usize) -> Option<&str> {
    let mut word_count = 0;
    let mut in_word = false;
    let mut idx = 0;
    for (i, c) in s.char_indices() {
        if c.is_whitespace() {
            if in_word {
                word_count += 1;
                if word_count == n {
                    // Skip leading whitespace after the n-th word.
                    idx = i;
                    break;
                }
            }
            in_word = false;
        } else {
            in_word = true;
        }
        idx = i + c.len_utf8();
    }
    // Handle the case where the string ends while still in a word.
    if in_word {
        word_count += 1;
    }
    if word_count < n {
        return None;
    }
    // idx now points to the first whitespace after the n-th word (or end).
    let remainder = s[idx..].trim_start();
    if remainder.is_empty() {
        None
    } else {
        Some(remainder)
    }
}

/// Result of parsing and dispatching a Matrix `!settings` command.
#[derive(Debug, PartialEq)]
pub enum SettingsCommandResult {
    /// The text was not a `!settings` command — caller should handle normally.
    NotSettingsCommand,
    /// Command executed; response to post back to the Matrix room.
    Response(String),
}

/// Attempt to parse and dispatch a `!settings ...` command.
///
/// - `user_id`: the Lumina user ID of the sender (resolved from Matrix ID by caller).
/// - `store`: an open `SettingsStore`.
/// - `text`: the raw message body (leading/trailing whitespace trimmed by caller).
/// - `user_enabled`: whether the user account is active. Commands from disabled
///   users are rejected.
///
/// Returns `SettingsCommandResult::NotSettingsCommand` if `text` does not start
/// with `!settings`.
///
/// Sensitive keys are blocked with a helpful error and guidance to use the CLI.
pub fn handle_settings_command(
    user_id: &str,
    store: &SettingsStore,
    text: &str,
    user_enabled: bool,
) -> Result<SettingsCommandResult> {
    let trimmed = text.trim();
    if !trimmed.starts_with("!settings") {
        return Ok(SettingsCommandResult::NotSettingsCommand);
    }

    if !user_enabled {
        return Ok(SettingsCommandResult::Response(
            "Your account is disabled. Contact an admin.".to_string(),
        ));
    }

    // Use split_whitespace to correctly collapse consecutive spaces from mobile
    // Matrix clients, then rebuild the value by advancing past the first 3 words.
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    // words[0] = "!settings", words[1] = subcommand, words[2] = key, words[3..] = value
    // For the value we need to find the text that comes after the first 3 words,
    // preserving spaces intentionally typed by the user (e.g. "Oakland, CA").
    let value_start = find_nth_word_end(trimmed, 3);

    let sub = words.get(1).copied().unwrap_or("help");

    // Build a token slice: [cmd, sub, key, value_remainder].
    // cmd_set reads tokens[2] as key, tokens[3] as value.
    let key_tok = words.get(2).copied().unwrap_or("");
    let val_tok = value_start.map(|s| s.trim()).unwrap_or("");
    let tokens: &[&str] = &[
        words.first().copied().unwrap_or("!settings"),
        sub,
        key_tok,
        val_tok,
    ];

    let response = match sub {
        "get"   => cmd_get(user_id, store, tokens)?,
        "set"   => cmd_set(user_id, store, tokens)?,
        "list"  => cmd_list(user_id, store)?,
        "reset" => cmd_reset(user_id, store, tokens)?,
        _       => settings_help(),
    };

    Ok(SettingsCommandResult::Response(response))
}

fn cmd_get(user_id: &str, store: &SettingsStore, tokens: &[&str]) -> Result<String> {
    let key = match tokens.get(2).map(|k| k.trim()).filter(|k| !k.is_empty()) {
        Some(k) => k,
        None => return Ok("Usage: !settings get <key>".to_string()),
    };

    if is_sensitive(key) {
        return Ok(format!(
            "Key `{}` is sensitive and cannot be read via Matrix. \
             Use the CLI: `lumina settings get {}`",
            key, key
        ));
    }

    match store.get(user_id, key)? {
        Some(val) => Ok(format!("`{}` = `{}`", key, val)),
        None => Ok(format!("`{}` is not set.", key)),
    }
}

fn cmd_set(user_id: &str, store: &SettingsStore, tokens: &[&str]) -> Result<String> {
    let key = match tokens.get(2).map(|k| k.trim()).filter(|k| !k.is_empty()) {
        Some(k) => k,
        None => return Ok("Usage: !settings set <key> <value>".to_string()),
    };
    let value = match tokens.get(3).map(|v| v.trim()).filter(|v| !v.is_empty()) {
        Some(v) => v,
        None => return Ok(format!("Usage: !settings set {} <value>", key)),
    };

    if is_sensitive(key) {
        return Ok(format!(
            "Key `{}` is sensitive — set it via the CLI to avoid exposure in Matrix logs:\n\
             `lumina settings set {} <value>`",
            key, key
        ));
    }

    // Per-key validation.
    let validation_result = match key {
        KEY_TIMEZONE => validate_timezone(value),
        KEY_BRIEFING_TIME => validate_briefing_time(value),
        KEY_DETAIL_LEVEL => validate_detail_level(value),
        _ => Ok(()),
    };

    if let Err(e) = validation_result {
        return Ok(format!("Invalid value: {}", e));
    }

    // Warn when calendar URL is set but no password hint exists.
    let mut extra = String::new();
    if key == KEY_CALENDAR_URL {
        extra = "\n\nCalendar URL saved. If you haven't configured authentication, \
                 run `lumina settings set caldav_password <password>` from the CLI."
            .to_string();
    }

    store.set(user_id, key, value)?;
    Ok(format!("`{}` set to `{}`.{}", key, value, extra))
}

fn cmd_list(user_id: &str, store: &SettingsStore) -> Result<String> {
    let settings = store.list(user_id)?;
    if settings.is_empty() {
        return Ok("No settings configured. Use `!settings set <key> <value>` to add one.".to_string());
    }
    let mut lines = vec!["**Your settings:**".to_string()];
    for (k, v) in &settings {
        lines.push(format!("- `{}` = `{}`", k, v));
    }
    Ok(lines.join("\n"))
}

fn cmd_reset(user_id: &str, store: &SettingsStore, tokens: &[&str]) -> Result<String> {
    let key = match tokens.get(2).map(|k| k.trim()).filter(|k| !k.is_empty()) {
        Some(k) => k,
        None => return Ok("Usage: !settings reset <key>".to_string()),
    };

    let removed = store.reset(user_id, key)?;
    if removed {
        Ok(format!("`{}` has been reset (deleted).", key))
    } else {
        Ok(format!("`{}` was not set.", key))
    }
}

fn settings_help() -> String {
    "!settings commands:\n\
     - `!settings list` — show all your settings\n\
     - `!settings get <key>` — get a single setting\n\
     - `!settings set <key> <value>` — set a setting\n\
     - `!settings reset <key>` — delete a setting\n\n\
     Common keys: timezone, location, calendar_url, email, briefing_time, detail_level, language\n\
     Sensitive settings (passwords, tokens) must be set via CLI: `lumina settings set <key> <value>`"
        .to_string()
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_key() -> Vec<u8> {
        vec![77u8; 32]
    }

    fn tmp_db(name: &str) -> PathBuf {
        let p = PathBuf::from(format!("/tmp/lumina_settings_test_{}.db", name));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn open_store(name: &str) -> SettingsStore {
        SettingsStore::new(&tmp_db(name), &test_key())
            .expect("open SettingsStore for test")
    }

    fn cleanup(name: &str) {
        let _ = std::fs::remove_file(tmp_db(name));
    }

    // ── get / set ──────────────────────────────────────────────────────────

    #[test]
    fn test_set_and_get() {
        let store = open_store("set_get");
        store.set("user1", KEY_TIMEZONE, "America/Los_Angeles").unwrap();
        let val = store.get("user1", KEY_TIMEZONE).unwrap();
        assert_eq!(val.as_deref(), Some("America/Los_Angeles"));
        cleanup("set_get");
    }

    #[test]
    fn test_get_nonexistent_key_returns_none() {
        let store = open_store("get_none");
        let val = store.get("user1", "does_not_exist").unwrap();
        assert!(val.is_none(), "Nonexistent key should return None");
        cleanup("get_none");
    }

    #[test]
    fn test_set_overwrites_existing_value() {
        let store = open_store("overwrite");
        store.set("user1", KEY_LOCATION, "Oakland, CA").unwrap();
        store.set("user1", KEY_LOCATION, "Portland, OR").unwrap();
        let val = store.get("user1", KEY_LOCATION).unwrap();
        assert_eq!(val.as_deref(), Some("Portland, OR"));
        cleanup("overwrite");
    }

    // ── list ───────────────────────────────────────────────────────────────

    #[test]
    fn test_list_returns_all_settings_for_user() {
        let store = open_store("list_all");
        store.set("user1", KEY_TIMEZONE, "UTC").unwrap();
        store.set("user1", KEY_LOCATION, "NYC").unwrap();
        store.set("user1", KEY_LANGUAGE, "en").unwrap();

        let settings = store.list("user1").unwrap();
        assert_eq!(settings.len(), 3);
        // Should be ordered alphabetically.
        assert_eq!(settings[0].0, KEY_LANGUAGE);   // language
        assert_eq!(settings[1].0, KEY_LOCATION);   // location
        assert_eq!(settings[2].0, KEY_TIMEZONE);   // timezone
        cleanup("list_all");
    }

    #[test]
    fn test_list_empty_returns_empty_vec() {
        let store = open_store("list_empty");
        let settings = store.list("nobody").unwrap();
        assert!(settings.is_empty());
        cleanup("list_empty");
    }

    #[test]
    fn test_list_isolates_by_user_id() {
        let store = open_store("list_isolation");
        store.set("user1", KEY_TIMEZONE, "UTC").unwrap();
        store.set("user2", KEY_TIMEZONE, "America/New_York").unwrap();

        let u1 = store.list("user1").unwrap();
        assert_eq!(u1.len(), 1);
        assert_eq!(u1[0].1, "UTC");

        let u2 = store.list("user2").unwrap();
        assert_eq!(u2.len(), 1);
        assert_eq!(u2[0].1, "America/New_York");
        cleanup("list_isolation");
    }

    // ── reset ──────────────────────────────────────────────────────────────

    #[test]
    fn test_reset_removes_key() {
        let store = open_store("reset_key");
        store.set("user1", KEY_LANGUAGE, "fr").unwrap();
        let removed = store.reset("user1", KEY_LANGUAGE).unwrap();
        assert!(removed, "Should return true when key existed");
        let val = store.get("user1", KEY_LANGUAGE).unwrap();
        assert!(val.is_none(), "Key should be gone after reset");
        cleanup("reset_key");
    }

    #[test]
    fn test_reset_nonexistent_key_returns_false() {
        let store = open_store("reset_none");
        let removed = store.reset("user1", "no_such_key").unwrap();
        assert!(!removed, "Should return false when key did not exist");
        cleanup("reset_none");
    }

    #[test]
    fn test_reset_all_removes_all_settings_for_user() {
        let store = open_store("reset_all");
        store.set("user1", KEY_TIMEZONE, "UTC").unwrap();
        store.set("user1", KEY_LOCATION, "NYC").unwrap();
        // Another user's settings should be unaffected.
        store.set("user2", KEY_TIMEZONE, "EST").unwrap();

        let n = store.reset_all("user1").unwrap();
        assert_eq!(n, 2, "Should have removed 2 rows");

        let u1 = store.list("user1").unwrap();
        assert!(u1.is_empty(), "user1 settings should all be gone");

        let u2 = store.list("user2").unwrap();
        assert_eq!(u2.len(), 1, "user2 settings should be unaffected");
        cleanup("reset_all");
    }

    // ── sensitive key guard ────────────────────────────────────────────────

    #[test]
    fn test_set_sensitive_key_rejected() {
        let store = open_store("sensitive");
        let result = store.set("user1", "app_password", "hunter2");
        assert!(result.is_err(), "Sensitive key should be rejected");
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("sensitive") || msg.contains("app_password"), "got: {}", msg);
        cleanup("sensitive");
    }

    #[test]
    fn test_set_password_variant_rejected() {
        let store = open_store("pwd_reject");
        let result = store.set("user1", "imap_password", "secret123");
        assert!(result.is_err(), "imap_password should be rejected");
        cleanup("pwd_reject");
    }

    // ── wrong key rejected ─────────────────────────────────────────────────

    #[test]
    fn test_wrong_key_rejected() {
        let path = tmp_db("wrong_key");
        SettingsStore::new(&path, &test_key()).unwrap();
        let wrong_key = vec![0xFFu8; 32];
        let result = SettingsStore::new(&path, &wrong_key);
        assert!(result.is_err(), "Wrong key should fail");
        let _ = std::fs::remove_file(&path);
    }

    // ── validation helpers ─────────────────────────────────────────────────

    #[test]
    fn test_validate_timezone_valid() {
        assert!(validate_timezone("America/Los_Angeles").is_ok());
        assert!(validate_timezone("UTC").is_ok());
        assert!(validate_timezone("Etc/GMT+5").is_ok());
        assert!(validate_timezone("Europe/London").is_ok());
    }

    #[test]
    fn test_validate_timezone_invalid() {
        assert!(validate_timezone("").is_err());
        assert!(validate_timezone("America; DROP TABLE").is_err());
        assert!(validate_timezone("../etc/passwd").is_err());
    }

    #[test]
    fn test_validate_briefing_time_valid() {
        assert!(validate_briefing_time("off").is_ok());
        assert!(validate_briefing_time("07:00").is_ok());
        assert!(validate_briefing_time("23:59").is_ok());
        assert!(validate_briefing_time("00:00").is_ok());
    }

    #[test]
    fn test_validate_briefing_time_invalid() {
        assert!(validate_briefing_time("24:00").is_err());
        assert!(validate_briefing_time("7am").is_err());
        assert!(validate_briefing_time("12:60").is_err());
        assert!(validate_briefing_time("").is_err());
    }

    #[test]
    fn test_validate_detail_level_valid() {
        assert!(validate_detail_level("brief").is_ok());
        assert!(validate_detail_level("normal").is_ok());
        assert!(validate_detail_level("verbose").is_ok());
    }

    #[test]
    fn test_validate_detail_level_invalid() {
        assert!(validate_detail_level("max").is_err());
        assert!(validate_detail_level("").is_err());
        assert!(validate_detail_level("VERBOSE").is_err());
    }

    // ── Matrix command handlers ────────────────────────────────────────────

    #[test]
    fn test_not_settings_command_returns_not_command() {
        let store = open_store("cmd_not");
        let result = handle_settings_command("user1", &store, "Hello world", true).unwrap();
        assert_eq!(result, SettingsCommandResult::NotSettingsCommand);
        cleanup("cmd_not");
    }

    #[test]
    fn test_disabled_user_rejected() {
        let store = open_store("cmd_disabled");
        let result = handle_settings_command("user1", &store, "!settings list", false).unwrap();
        match result {
            SettingsCommandResult::Response(msg) => {
                assert!(msg.contains("disabled") || msg.contains("admin"), "got: {}", msg);
            }
            other => panic!("Expected Response, got {:?}", other),
        }
        cleanup("cmd_disabled");
    }

    #[test]
    fn test_settings_list_no_settings() {
        let store = open_store("cmd_list_empty");
        let result = handle_settings_command("user1", &store, "!settings list", true).unwrap();
        match result {
            SettingsCommandResult::Response(msg) => {
                assert!(
                    msg.contains("No settings") || msg.contains("not set"),
                    "got: {}",
                    msg
                );
            }
            other => panic!("Expected Response, got {:?}", other),
        }
        cleanup("cmd_list_empty");
    }

    #[test]
    fn test_settings_set_and_list() {
        let store = open_store("cmd_set_list");
        let _ = handle_settings_command(
            "user1", &store, "!settings set timezone America/Chicago", true,
        ).unwrap();
        let result = handle_settings_command("user1", &store, "!settings list", true).unwrap();
        match result {
            SettingsCommandResult::Response(msg) => {
                assert!(msg.contains("timezone"), "got: {}", msg);
                assert!(msg.contains("America/Chicago"), "got: {}", msg);
            }
            other => panic!("Expected Response, got {:?}", other),
        }
        cleanup("cmd_set_list");
    }

    #[test]
    fn test_settings_get_existing() {
        let store = open_store("cmd_get_exist");
        store.set("user1", KEY_LOCATION, "Seattle, WA").unwrap();
        let result = handle_settings_command("user1", &store, "!settings get location", true).unwrap();
        match result {
            SettingsCommandResult::Response(msg) => {
                assert!(msg.contains("Seattle, WA"), "got: {}", msg);
            }
            other => panic!("Expected Response, got {:?}", other),
        }
        cleanup("cmd_get_exist");
    }

    #[test]
    fn test_settings_get_nonexistent() {
        let store = open_store("cmd_get_none");
        let result = handle_settings_command("user1", &store, "!settings get timezone", true).unwrap();
        match result {
            SettingsCommandResult::Response(msg) => {
                assert!(msg.contains("not set") || msg.contains("timezone"), "got: {}", msg);
            }
            other => panic!("Expected Response, got {:?}", other),
        }
        cleanup("cmd_get_none");
    }

    #[test]
    fn test_settings_reset_existing() {
        let store = open_store("cmd_reset");
        store.set("user1", KEY_TIMEZONE, "UTC").unwrap();
        let result = handle_settings_command("user1", &store, "!settings reset timezone", true).unwrap();
        match result {
            SettingsCommandResult::Response(msg) => {
                assert!(
                    msg.contains("reset") || msg.contains("deleted") || msg.contains("timezone"),
                    "got: {}",
                    msg
                );
            }
            other => panic!("Expected Response, got {:?}", other),
        }
        let val = store.get("user1", KEY_TIMEZONE).unwrap();
        assert!(val.is_none(), "Key should be gone after reset");
        cleanup("cmd_reset");
    }

    #[test]
    fn test_settings_set_sensitive_key_blocked() {
        let store = open_store("cmd_sensitive");
        let result = handle_settings_command(
            "user1", &store, "!settings set app_password hunter2", true,
        ).unwrap();
        match result {
            SettingsCommandResult::Response(msg) => {
                assert!(
                    msg.contains("sensitive") || msg.contains("CLI") || msg.contains("cli"),
                    "got: {}",
                    msg
                );
            }
            other => panic!("Expected Response, got {:?}", other),
        }
        cleanup("cmd_sensitive");
    }

    #[test]
    fn test_settings_set_invalid_timezone_rejected() {
        let store = open_store("cmd_bad_tz");
        let result = handle_settings_command(
            "user1", &store, "!settings set timezone Bogus/Zone!!", true,
        ).unwrap();
        match result {
            SettingsCommandResult::Response(msg) => {
                assert!(
                    msg.contains("Invalid") || msg.contains("invalid"),
                    "got: {}",
                    msg
                );
            }
            other => panic!("Expected Response, got {:?}", other),
        }
        cleanup("cmd_bad_tz");
    }

    #[test]
    fn test_settings_set_invalid_briefing_time_rejected() {
        let store = open_store("cmd_bad_time");
        let result = handle_settings_command(
            "user1", &store, "!settings set briefing_time 25:99", true,
        ).unwrap();
        match result {
            SettingsCommandResult::Response(msg) => {
                assert!(
                    msg.contains("Invalid") || msg.contains("invalid"),
                    "got: {}",
                    msg
                );
            }
            other => panic!("Expected Response, got {:?}", other),
        }
        cleanup("cmd_bad_time");
    }

    #[test]
    fn test_settings_calendar_url_warns_about_auth() {
        let store = open_store("cmd_cal");
        let result = handle_settings_command(
            "user1",
            &store,
            "!settings set calendar_url https://cal.example.com/dav",
            true,
        ).unwrap();
        match result {
            SettingsCommandResult::Response(msg) => {
                // Should save AND include a warning about authentication.
                assert!(
                    msg.contains("Calendar") || msg.contains("calendar") || msg.contains("auth"),
                    "got: {}",
                    msg
                );
            }
            other => panic!("Expected Response, got {:?}", other),
        }
        cleanup("cmd_cal");
    }

    #[test]
    fn test_settings_command_parsing() {
        let store = open_store("cmd_parse");
        // "!settings" with no subcommand should return help.
        let result = handle_settings_command("user1", &store, "!settings", true).unwrap();
        match result {
            SettingsCommandResult::Response(msg) => {
                assert!(
                    msg.contains("list") || msg.contains("set") || msg.contains("help"),
                    "Expected help text, got: {}",
                    msg
                );
            }
            other => panic!("Expected Response, got {:?}", other),
        }
        cleanup("cmd_parse");
    }

    #[test]
    fn test_settings_set_with_double_spaces_tokenizes_correctly() {
        // Regression: double spaces from mobile Matrix clients must not break parsing.
        let store = open_store("cmd_double_space");
        let result = handle_settings_command(
            "user1",
            &store,
            "!settings  set  timezone  America/Chicago",
            true,
        ).unwrap();
        match result {
            SettingsCommandResult::Response(msg) => {
                assert!(
                    msg.contains("America/Chicago") || msg.contains("set to"),
                    "Double-space broke set: {}",
                    msg
                );
            }
            other => panic!("Expected Response, got {:?}", other),
        }
        // Verify the value was stored correctly.
        let val = store.get("user1", "timezone").unwrap();
        assert_eq!(val.as_deref(), Some("America/Chicago"), "Stored value should not contain extra spaces");
        cleanup("cmd_double_space");
    }

    #[test]
    fn test_settings_set_value_with_spaces_preserved() {
        // "Oakland, CA" contains a space — it must be stored intact.
        let store = open_store("cmd_val_spaces");
        let result = handle_settings_command(
            "user1",
            &store,
            "!settings set location Oakland, CA",
            true,
        ).unwrap();
        match result {
            SettingsCommandResult::Response(msg) => {
                assert!(
                    msg.contains("Oakland") || msg.contains("set to"),
                    "Value with space broke set: {}",
                    msg
                );
            }
            other => panic!("Expected Response, got {:?}", other),
        }
        let val = store.get("user1", "location").unwrap();
        assert_eq!(val.as_deref(), Some("Oakland, CA"), "Spaces in value must be preserved");
        cleanup("cmd_val_spaces");
    }

    #[test]
    fn test_is_sensitive_exact_segment_match() {
        // "password" as a segment should match.
        assert!(is_sensitive("app_password"), "app_password should be sensitive");
        assert!(is_sensitive("imap_password"), "imap_password should be sensitive");
        assert!(is_sensitive("password"), "password alone should be sensitive");
        // "password_manager" — "password" is a segment, so it IS blocked.
        // This is intentional: user setting keys should not mention passwords.
        assert!(is_sensitive("password_reset_enabled"), "any password-containing key is blocked");
        // Keys that don't contain sensitive segments are allowed.
        assert!(!is_sensitive("timezone"), "timezone should not be sensitive");
        assert!(!is_sensitive("location"), "location should not be sensitive");
        assert!(!is_sensitive("briefing_time"), "briefing_time should not be sensitive");
    }
}
