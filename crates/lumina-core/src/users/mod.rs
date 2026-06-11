//! P2-01: User identity model and storage.
//!
//! Each user has a unique UUID, display name, role, and one or more channel
//! identities (Matrix user ID, Telegram ID, etc.). Users and their channel
//! identities are stored in a SQLCipher-encrypted database.
//!
//! P2-06: Also stores per-user key-value settings (user_settings table)
//! used by the CLI and Matrix commands for tool grants, system prompts, etc.
//!
//! Key behaviours:
//! - The first user created is automatically given the `Admin` role.
//! - Unknown channel identities create a `Guest` user on first contact.
//! - `LUMINA_ADMIN_MATRIX_ID` names the Matrix user that is auto-promoted to
//!   `Admin` when they send their first message (loaded from config at the
//!   call site — not read directly from the environment here).
//! - Disabled users are not returned by channel-identity lookups.

pub mod cli;
pub mod cost_caps;
pub mod identity;
pub mod matrix_commands;
pub mod permissions;
pub mod settings;
pub mod shared_resources;
pub mod vault_service;

use crate::error::{LuminaError, Result};
use crate::vault;
use rand::RngCore;
use rusqlite::{params, Connection, OptionalExtension};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

// ── Public data types ──────────────────────────────────────────────────────

/// Role granted to a user — controls which features are accessible.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UserRole {
    /// Full access: manage users, all tools, all data.
    Admin,
    /// Scoped access: configured tools, own data.
    Member,
    /// Read-only: no tools, no memory writes.
    Guest,
}

impl UserRole {
    /// Numeric rank — higher means more privileged.
    pub fn rank(&self) -> u8 {
        match self {
            UserRole::Admin => 2,
            UserRole::Member => 1,
            UserRole::Guest => 0,
        }
    }

    /// Serialize to the database string representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            UserRole::Admin => "admin",
            UserRole::Member => "member",
            UserRole::Guest => "guest",
        }
    }

    /// Deserialize from the database string representation.
    pub fn from_db_str(s: &str) -> Self {
        match s {
            "admin" => UserRole::Admin,
            "member" => UserRole::Member,
            _ => UserRole::Guest,
        }
    }
}

impl std::fmt::Display for UserRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A Lumina user account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserIdentity {
    /// Stable UUID — never changes.
    pub user_id: String,
    /// Human-readable name (max 100 chars).
    pub display_name: String,
    /// Primary Matrix user ID, e.g. `@alice:example.com`.
    pub matrix_user_id: Option<String>,
    /// User's role.
    pub role: UserRole,
    /// ISO-8601 timestamp (UTC) when the account was created.
    pub created_at: String,
    /// ISO-8601 timestamp (UTC) of the most recent activity, or `None`.
    pub last_seen: Option<String>,
    /// Whether the account is active.
    pub enabled: bool,
}

// ── UserStore ──────────────────────────────────────────────────────────────

/// SQLCipher-backed store for user accounts.
///
/// Both the `users` table and the `channel_identities` table (see
/// [`identity`]) live in the same encrypted database file and share the same
/// `Connection`. Channel-identity methods are provided as inherent methods via
/// `impl UserStore` in `identity.rs`.
pub struct UserStore {
    conn: Connection,
}

impl UserStore {
    /// Borrow the underlying connection — used by the `identity` sub-module.
    pub(super) fn conn(&self) -> &Connection {
        &self.conn
    }
}

impl UserStore {
    /// Open (or create) the SQLCipher database at `db_path` with `key`.
    ///
    /// On first open the schema is created. Subsequent opens verify the key
    /// with a harmless query — a wrong key produces a clear `SecurityViolation`.
    pub fn new(db_path: &Path, key: &[u8]) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                LuminaError::Config(format!("Cannot create users database directory: {}", e))
            })?;
        }

        let conn = Connection::open(db_path).map_err(|e| {
            LuminaError::Config(format!("Cannot open users database: {}", e))
        })?;

        // SQLCipher: set key before any other operation.
        let hex_key = hex::encode(key);
        conn.execute_batch(&format!("PRAGMA key = \"x'{}'\";", hex_key))
            .map_err(|e| {
                LuminaError::Config(format!("Failed to set users database key: {}", e))
            })?;

        // Verify the key is correct.
        conn.execute_batch("SELECT count(*) FROM sqlite_master;")
            .map_err(|_| {
                LuminaError::SecurityViolation(
                    "Users database key is incorrect — cannot open database".to_string(),
                )
            })?;

        conn.execute_batch("PRAGMA journal_mode = WAL;").map_err(|e| {
            LuminaError::Config(format!("Failed to enable WAL mode: {}", e))
        })?;

        let store = Self { conn };
        store.ensure_schema()?;
        Ok(store)
    }

    /// Open using the default path (`~/.lumina/users.db`) and key from vault.
    ///
    /// Auto-generates and persists the key on first run.
    pub fn open_default() -> Result<Self> {
        let db_path = default_db_path();
        let key = get_or_create_users_key()?;
        Self::new(&db_path, &key)
    }

    // ── Write operations ───────────────────────────────────────────────────

    /// Create a new user. Returns the `UserIdentity` with its generated UUID.
    ///
    /// If this is the first user in the database the role is forced to `Admin`
    /// regardless of what was passed in.
    ///
    /// Display names longer than 100 characters are silently truncated.
    pub fn create_user(
        &self,
        display_name: &str,
        matrix_user_id: Option<&str>,
        role: UserRole,
    ) -> Result<UserIdentity> {
        let display_name = truncate_display_name(display_name);

        // Force Admin for the very first user.
        let role = if self.count_users()? == 0 {
            UserRole::Admin
        } else {
            role
        };

        let user_id = Uuid::new_v4().to_string();
        let created_at = utc_now();

        self.conn
            .execute(
                "INSERT INTO users
                     (id, display_name, matrix_user_id, role, created_at, last_seen, enabled)
                 VALUES (?1, ?2, ?3, ?4, ?5, NULL, 1)",
                params![
                    user_id,
                    display_name,
                    matrix_user_id,
                    role.as_str(),
                    created_at,
                ],
            )
            .map_err(|e| LuminaError::Config(format!("Failed to create user: {}", e)))?;

        Ok(UserIdentity {
            user_id,
            display_name,
            matrix_user_id: matrix_user_id.map(|s| s.to_string()),
            role,
            created_at,
            last_seen: None,
            enabled: true,
        })
    }

    /// Delete a user by ID. Returns `true` if a row was removed.
    ///
    /// Also removes all `channel_identities` rows for this user so they
    /// cannot be returned by subsequent channel lookups.
    pub fn delete(&self, user_id: &str) -> Result<bool> {
        // Remove channel identities first (no FK cascade in SQLite without pragma).
        self.conn
            .execute(
                "DELETE FROM channel_identities WHERE user_id = ?1",
                params![user_id],
            )
            .map_err(|e| {
                LuminaError::Config(format!(
                    "Failed to delete channel identities for user: {}",
                    e
                ))
            })?;

        let n = self
            .conn
            .execute("DELETE FROM users WHERE id = ?1", params![user_id])
            .map_err(|e| LuminaError::Config(format!("Failed to delete user: {}", e)))?;
        Ok(n > 0)
    }

    /// Update the `last_seen` timestamp to the current UTC time.
    pub fn update_last_seen(&self, user_id: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE users SET last_seen = ?1 WHERE id = ?2",
                params![utc_now(), user_id],
            )
            .map_err(|e| LuminaError::Config(format!("Failed to update last_seen: {}", e)))?;
        Ok(())
    }

    /// Change the role of a user.
    ///
    /// Prevents the last Admin from being demoted — returns an error if the
    /// operation would leave the system with zero admin accounts.
    pub fn set_role(&self, user_id: &str, new_role: UserRole) -> Result<()> {
        // Guard: don't allow removing the last admin.
        if new_role != UserRole::Admin {
            let current = self
                .get_by_id(user_id)?
                .ok_or_else(|| LuminaError::Config(format!("User {} not found", user_id)))?;
            if current.role == UserRole::Admin && self.count_admins()? <= 1 {
                return Err(LuminaError::Config(
                    "Cannot demote the last admin — promote another user first".to_string(),
                ));
            }
        }

        self.conn
            .execute(
                "UPDATE users SET role = ?1 WHERE id = ?2",
                params![new_role.as_str(), user_id],
            )
            .map_err(|e| LuminaError::Config(format!("Failed to set role: {}", e)))?;
        Ok(())
    }

    /// Enable or disable a user account.
    ///
    /// Refuses to disable the last active admin.
    pub fn set_enabled(&self, user_id: &str, enabled: bool) -> Result<()> {
        if !enabled {
            let current = self
                .get_by_id(user_id)?
                .ok_or_else(|| LuminaError::Config(format!("User {} not found", user_id)))?;
            if current.role == UserRole::Admin && self.count_admins()? <= 1 {
                return Err(LuminaError::Config(
                    "Cannot disable the last admin — promote another user first".to_string(),
                ));
            }
        }

        self.conn
            .execute(
                "UPDATE users SET enabled = ?1 WHERE id = ?2",
                params![enabled as i32, user_id],
            )
            .map_err(|e| LuminaError::Config(format!("Failed to set enabled: {}", e)))?;
        Ok(())
    }

    // ── Read operations ────────────────────────────────────────────────────

    /// Retrieve a user by their UUID. Returns `None` if not found.
    pub fn get_by_id(&self, user_id: &str) -> Result<Option<UserIdentity>> {
        let result = self
            .conn
            .query_row(
                "SELECT id, display_name, matrix_user_id, role,
                        created_at, last_seen, enabled
                 FROM users WHERE id = ?1",
                params![user_id],
                row_to_user,
            )
            .optional()
            .map_err(|e| LuminaError::Config(format!("get_by_id failed: {}", e)))?;
        Ok(result)
    }

    /// Retrieve an **enabled** user by their Matrix user ID.
    ///
    /// Returns `None` if the user does not exist or is disabled.
    pub fn get_by_matrix_id(&self, matrix_user_id: &str) -> Result<Option<UserIdentity>> {
        let result = self
            .conn
            .query_row(
                "SELECT id, display_name, matrix_user_id, role,
                        created_at, last_seen, enabled
                 FROM users WHERE matrix_user_id = ?1 AND enabled = 1",
                params![matrix_user_id],
                row_to_user,
            )
            .optional()
            .map_err(|e| LuminaError::Config(format!("get_by_matrix_id failed: {}", e)))?;
        Ok(result)
    }

    /// Retrieve a user by their Matrix user ID regardless of enabled status.
    ///
    /// Used by admin operations (e.g. `/admin show`, `/admin promote`) where the
    /// admin needs to look up disabled accounts to re-enable or inspect them.
    pub fn get_by_matrix_id_any(&self, matrix_user_id: &str) -> Result<Option<UserIdentity>> {
        let result = self
            .conn
            .query_row(
                "SELECT id, display_name, matrix_user_id, role,
                        created_at, last_seen, enabled
                 FROM users WHERE matrix_user_id = ?1",
                params![matrix_user_id],
                row_to_user,
            )
            .optional()
            .map_err(|e| LuminaError::Config(format!("get_by_matrix_id_any failed: {}", e)))?;
        Ok(result)
    }

    /// Return all users (including disabled), ordered by `created_at`.
    pub fn list(&self) -> Result<Vec<UserIdentity>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, display_name, matrix_user_id, role,
                        created_at, last_seen, enabled
                 FROM users ORDER BY created_at ASC",
            )
            .map_err(|e| LuminaError::Config(format!("list prepare failed: {}", e)))?;

        let rows = stmt
            .query_map([], row_to_user)
            .map_err(|e| LuminaError::Config(format!("list query failed: {}", e)))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| LuminaError::Config(format!("list row error: {}", e)))?;

        Ok(rows)
    }

    // ── Auto-provisioning helpers ──────────────────────────────────────────

    /// Ensure the configured admin Matrix ID is promoted to `Admin`.
    ///
    /// `admin_matrix_id` comes from `LUMINA_ADMIN_MATRIX_ID` (surfaced via
    /// `Config`) — it is NOT read from the environment here. Called at
    /// startup with the value from config.
    ///
    /// If the user with that Matrix ID exists and is not yet `Admin`, their
    /// role is upgraded. If no such user exists, nothing happens (they will
    /// be promoted when they first message).
    pub fn apply_admin_matrix_promotion(&self, admin_matrix_id: &str) -> Result<()> {
        if admin_matrix_id.trim().is_empty() {
            return Ok(());
        }

        if let Some(user) = self.get_by_matrix_id(admin_matrix_id)? {
            if user.role != UserRole::Admin {
                // Bypass the last-admin guard — this is a startup promotion.
                self.conn
                    .execute(
                        "UPDATE users SET role = 'admin' WHERE id = ?1",
                        params![user.user_id],
                    )
                    .map_err(|e| {
                        LuminaError::Config(format!("Admin promotion failed: {}", e))
                    })?;
            }
        }
        Ok(())
    }

    /// Get or auto-provision a user for the given Matrix ID.
    ///
    /// - If a matching enabled user exists, return it (with `last_seen` updated).
    /// - If the Matrix ID matches `admin_matrix_id`, create with `Admin` role.
    /// - Otherwise, create a new `Guest` user.
    ///
    /// `admin_matrix_id` comes from `Config::admin_matrix_id()` — pass `""`
    /// or an empty `Option` if admin auto-promotion is not configured.
    pub fn get_or_create_matrix_user(
        &self,
        matrix_user_id: &str,
        display_name: &str,
        admin_matrix_id: &str,
    ) -> Result<UserIdentity> {
        if let Some(user) = self.get_by_matrix_id(matrix_user_id)? {
            self.update_last_seen(&user.user_id)?;
            return Ok(user);
        }

        let role = if !admin_matrix_id.is_empty() && matrix_user_id == admin_matrix_id {
            UserRole::Admin
        } else {
            UserRole::Guest
        };

        self.create_user(display_name, Some(matrix_user_id), role)
    }

    // ── Private helpers ────────────────────────────────────────────────────

    fn ensure_schema(&self) -> Result<()> {
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS users (
                    id              TEXT PRIMARY KEY,
                    display_name    TEXT NOT NULL,
                    matrix_user_id  TEXT,
                    role            TEXT NOT NULL DEFAULT 'guest',
                    created_at      TEXT NOT NULL,
                    last_seen       TEXT,
                    enabled         INTEGER NOT NULL DEFAULT 1
                );
                CREATE INDEX IF NOT EXISTS idx_users_matrix_id
                    ON users (matrix_user_id);
                CREATE INDEX IF NOT EXISTS idx_users_role
                    ON users (role);
                CREATE TABLE IF NOT EXISTS user_settings (
                    user_id TEXT NOT NULL,
                    key     TEXT NOT NULL,
                    value   TEXT NOT NULL,
                    PRIMARY KEY (user_id, key)
                );
                CREATE INDEX IF NOT EXISTS idx_user_settings_user
                    ON user_settings (user_id);",
            )
            .map_err(|e| LuminaError::Config(format!("Schema creation failed: {}", e)))?;

        // Channel identities share the same connection/database file.
        identity::ensure_channel_schema(&self.conn)?;
        Ok(())
    }

    // ── User settings (key-value per user) ────────────────────────────────────

    /// Set (or update) a per-user key-value setting.
    pub fn set_user_setting(&self, user_id: &str, key: &str, value: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO user_settings (user_id, key, value)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(user_id, key) DO UPDATE SET value = excluded.value",
                params![user_id, key, value],
            )
            .map_err(|e| LuminaError::Config(format!("set_user_setting failed: {}", e)))?;
        Ok(())
    }

    /// Retrieve a per-user setting by key. Returns `None` if not set.
    pub fn get_user_setting(&self, user_id: &str, key: &str) -> Result<Option<String>> {
        let result = self
            .conn
            .query_row(
                "SELECT value FROM user_settings WHERE user_id = ?1 AND key = ?2",
                params![user_id, key],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| LuminaError::Config(format!("get_user_setting failed: {}", e)))?;
        Ok(result)
    }

    fn count_users(&self) -> Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
            .map_err(|e| LuminaError::Config(format!("count_users failed: {}", e)))
    }

    fn count_admins(&self) -> Result<i64> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM users WHERE role = 'admin' AND enabled = 1",
                [],
                |r| r.get(0),
            )
            .map_err(|e| LuminaError::Config(format!("count_admins failed: {}", e)))
    }
}

// ── Module-level helpers ───────────────────────────────────────────────────

/// Return the default database path: `~/.lumina/users.db`.
pub fn default_db_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".lumina")
        .join("users.db")
}

/// Maximum allowed length for a `user_id` string.
///
/// Most Linux filesystems cap single path components at 255 bytes.
/// We use a tighter bound to keep paths reasonable.
const USER_ID_MAX_LEN: usize = 128;

/// Validate that a `user_id` string is safe to use as a filesystem path component.
///
/// Allows only ASCII alphanumeric characters, hyphens, and underscores, with a
/// maximum length of [`USER_ID_MAX_LEN`] characters.
/// This prevents path traversal attacks (e.g. `../etc`) since user IDs may
/// originate from external input (Matrix user IDs, HTTP auth tokens).
///
/// Returns `Ok(())` if the user ID is valid, or a `LuminaError::Config` otherwise.
pub fn validate_user_id(user_id: &str) -> crate::error::Result<()> {
    if user_id.is_empty() {
        return Err(crate::error::LuminaError::Config(
            "user_id must not be empty".to_string(),
        ));
    }
    if user_id.len() > USER_ID_MAX_LEN {
        return Err(crate::error::LuminaError::Config(format!(
            "user_id is too long ({} chars, max {})",
            user_id.len(),
            USER_ID_MAX_LEN
        )));
    }
    if !user_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(crate::error::LuminaError::Config(format!(
            "user_id '{}' contains invalid characters — only ASCII letters, digits, hyphens, and underscores are allowed",
            user_id
        )));
    }
    Ok(())
}

/// Derive a storage-safe id from an arbitrary caller id (e.g. a Matrix user id
/// like `@alice:home.org`, which contains `@`/`:`/`.`).
///
/// Maps every character outside `[A-Za-z0-9_-]` to `_`, so the result always
/// satisfies [`validate_user_id`] and is safe as a path segment / store key.
/// Stable per input; note the mapping is lossy (distinct raw ids can collapse to
/// the same slug, which is acceptable for the Matrix caller population). An
/// empty/all-invalid input maps to `"system"` (the anonymous/legacy slot). Use
/// at channel boundaries before threading a caller id into per-user stores
/// (Engram, training) or the conversation buffer.
pub fn to_storage_id(caller_id: &str) -> String {
    let cleaned: String = caller_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .take(USER_ID_MAX_LEN)
        .collect();
    if cleaned.is_empty() || cleaned.chars().all(|c| c == '_') {
        "system".to_string()
    } else {
        cleaned
    }
}

/// Return `base/users/{user_id}/` — the per-user data directory.
///
/// Restricted to `pub(crate)` to ensure callers always go through
/// [`validate_user_id`] before building the path. External callers that
/// need the path should call `validate_user_id(user_id)` first, then use
/// the store's own `open_for_user_at` constructor.
pub(crate) fn user_data_dir(base: &Path, user_id: &str) -> PathBuf {
    base.join("users").join(user_id)
}

/// Get the users DB key from vault, or generate and store one on first run.
///
/// On first run a new 32-byte key is generated and persisted to vault.
/// Returns an error if the vault cannot be opened for writing — using an
/// ephemeral key would make the database permanently unreadable on the next
/// restart.
pub fn get_or_create_users_key() -> Result<Vec<u8>> {
    // Try to load an existing key from vault.
    if let Ok(vault_store) = vault::VaultStore::load() {
        if let Some(stored) = vault_store.get("LUMINA_USERS_DB_KEY") {
            if let Ok(bytes) = hex::decode(stored.expose_secret()) {
                if bytes.len() >= 32 {
                    return Ok(bytes);
                }
            }
        }
    }

    // No existing key — generate a new one.
    let mut key = vec![0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut key);
    let hex_key = hex::encode(&key);

    // Persist to vault (mandatory — ephemeral keys leave the DB unreadable on
    // the next restart). Fail loudly rather than silently returning a throwaway key.
    let mut vault_store = vault::VaultStore::load().map_err(|e| {
        LuminaError::Config(format!(
            "Cannot open vault to persist new users DB key: {}. \
             Set LUMINA_USERS_DB_KEY in the vault before starting.",
            e
        ))
    })?;
    vault_store
        .set(
            "LUMINA_USERS_DB_KEY".to_string(),
            secrecy::SecretString::new(hex_key.into()),
        )
        .map_err(|e| {
            LuminaError::Config(format!(
                "Failed to save users DB key to vault: {}. \
                 Set LUMINA_USERS_DB_KEY manually.",
                e
            ))
        })?;

    Ok(key)
}

// ── Private helpers ────────────────────────────────────────────────────────

/// Current UTC time as an ISO-8601 string (seconds precision).
///
/// Uses std::time to avoid a heavy chrono/time dependency. The calculation is
/// straightforward Julian Day arithmetic — tested against leap years.
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

fn truncate_display_name(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() > 100 {
        chars[..100].iter().collect()
    } else {
        name.to_string()
    }
}

/// Map a rusqlite row to a `UserIdentity`.
fn row_to_user(row: &rusqlite::Row<'_>) -> rusqlite::Result<UserIdentity> {
    let role_str: String = row.get(3)?;
    let enabled_int: i32 = row.get(6)?;
    Ok(UserIdentity {
        user_id: row.get(0)?,
        display_name: row.get(1)?,
        matrix_user_id: row.get(2)?,
        role: UserRole::from_db_str(&role_str),
        created_at: row.get(4)?,
        last_seen: row.get(5)?,
        enabled: enabled_int != 0,
    })
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_key() -> Vec<u8> {
        vec![42u8; 32]
    }

    #[test]
    fn to_storage_id_makes_matrix_ids_valid() {
        let safe = to_storage_id("@alice:home.org");
        assert_eq!(safe, "_alice_home_org");
        // the result must always satisfy validate_user_id
        assert!(validate_user_id(&safe).is_ok());
        assert!(validate_user_id(&to_storage_id("@s78livetest:example.com")).is_ok());
        // already-safe ids pass through unchanged
        assert_eq!(to_storage_id("system"), "system");
        assert_eq!(to_storage_id("user-1_2"), "user-1_2");
        // empty / all-invalid → "system"
        assert_eq!(to_storage_id(""), "system");
        assert_eq!(to_storage_id("@:."), "system");
        // stable: same input → same output
        assert_eq!(to_storage_id("@bob:x.y"), to_storage_id("@bob:x.y"));
    }

    fn tmp_db(name: &str) -> PathBuf {
        let p = PathBuf::from(format!("/tmp/lumina_users_test_{}.db", name));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn open(name: &str) -> UserStore {
        UserStore::new(&tmp_db(name), &test_key()).expect("open UserStore")
    }

    // --- create / get / list / delete -----------------------------------------

    #[test]
    fn test_create_and_get_by_id() {
        let store = open("create_get");
        let user = store
            .create_user("Alice", Some("@alice:example.com"), UserRole::Member)
            .unwrap();

        // First user is always Admin regardless of requested role.
        assert_eq!(user.role, UserRole::Admin);
        assert!(user.enabled);

        let fetched = store.get_by_id(&user.user_id).unwrap().unwrap();
        assert_eq!(fetched.display_name, "Alice");
        assert_eq!(
            fetched.matrix_user_id,
            Some("@alice:example.com".to_string())
        );
        let _ = std::fs::remove_file(tmp_db("create_get"));
    }

    #[test]
    fn test_second_user_keeps_requested_role() {
        let store = open("second_role");
        store.create_user("Admin", None, UserRole::Guest).unwrap(); // first → Admin
        let u2 = store.create_user("Bob", None, UserRole::Member).unwrap();
        assert_eq!(u2.role, UserRole::Member);
        let _ = std::fs::remove_file(tmp_db("second_role"));
    }

    #[test]
    fn test_list_users() {
        let store = open("list");
        store.create_user("Alpha", None, UserRole::Admin).unwrap();
        store.create_user("Beta", None, UserRole::Member).unwrap();
        store.create_user("Gamma", None, UserRole::Guest).unwrap();

        let users = store.list().unwrap();
        assert_eq!(users.len(), 3);
        assert_eq!(users[0].display_name, "Alpha");
        let _ = std::fs::remove_file(tmp_db("list"));
    }

    #[test]
    fn test_delete_user() {
        let store = open("delete");
        let u = store.create_user("Temp", None, UserRole::Guest).unwrap();
        // Create a second admin so the first can be demoted + deleted.
        let _u2 = store.create_user("Perm", None, UserRole::Admin).unwrap();
        store.set_role(&u.user_id, UserRole::Guest).unwrap();

        let removed = store.delete(&u.user_id).unwrap();
        assert!(removed);
        assert!(store.get_by_id(&u.user_id).unwrap().is_none());
        let _ = std::fs::remove_file(tmp_db("delete"));
    }

    #[test]
    fn test_delete_nonexistent_returns_false() {
        let store = open("delete_none");
        let removed = store.delete("no-such-uuid").unwrap();
        assert!(!removed);
        let _ = std::fs::remove_file(tmp_db("delete_none"));
    }

    // --- matrix_id lookup -----------------------------------------------------

    #[test]
    fn test_get_by_matrix_id() {
        let store = open("matrix_id");
        store
            .create_user("Carol", Some("@carol:home.org"), UserRole::Member)
            .unwrap();
        let found = store.get_by_matrix_id("@carol:home.org").unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().display_name, "Carol");
        let _ = std::fs::remove_file(tmp_db("matrix_id"));
    }

    #[test]
    fn test_get_by_matrix_id_unknown_returns_none() {
        let store = open("matrix_id_none");
        let found = store.get_by_matrix_id("@nobody:home.org").unwrap();
        assert!(found.is_none());
        let _ = std::fs::remove_file(tmp_db("matrix_id_none"));
    }

    // --- default owner created when empty -------------------------------------

    #[test]
    fn test_default_owner_is_admin_when_first() {
        let store = open("default_owner");
        let user = store.create_user("Owner", None, UserRole::Guest).unwrap();
        // Even though Guest was requested, first user is Admin.
        assert_eq!(user.role, UserRole::Admin);
        let _ = std::fs::remove_file(tmp_db("default_owner"));
    }

    // --- role serialization round-trip ----------------------------------------

    #[test]
    fn test_role_serialization_round_trip() {
        let roles = [UserRole::Admin, UserRole::Member, UserRole::Guest];
        for role in &roles {
            let json = serde_json::to_string(role).unwrap();
            let back: UserRole = serde_json::from_str(&json).unwrap();
            assert_eq!(role, &back, "round-trip failed for {:?}", role);
        }
    }

    #[test]
    fn test_role_rank_ordering() {
        assert!(UserRole::Admin.rank() > UserRole::Member.rank());
        assert!(UserRole::Member.rank() > UserRole::Guest.rank());
    }

    // --- disabled user cannot be retrieved by channel identity ----------------

    #[test]
    fn test_disabled_user_not_returned_by_matrix_id() {
        let store = open("disabled");
        let u = store
            .create_user("DisabledUser", Some("@disabled:home.org"), UserRole::Member)
            .unwrap();
        // Need a second admin so the original can be disabled.
        let _admin2 = store.create_user("Admin2", None, UserRole::Admin).unwrap();
        store.set_enabled(&u.user_id, false).unwrap();

        let found = store.get_by_matrix_id("@disabled:home.org").unwrap();
        assert!(
            found.is_none(),
            "Disabled user should not be returned by get_by_matrix_id"
        );
        let _ = std::fs::remove_file(tmp_db("disabled"));
    }

    // --- admin Matrix ID auto-promoted ----------------------------------------

    #[test]
    fn test_admin_matrix_id_auto_promoted_on_create() {
        let store = open("admin_promo");
        // Seed one admin so the next user won't be forced to Admin.
        store.create_user("Seed", None, UserRole::Admin).unwrap();

        let user = store
            .get_or_create_matrix_user("@boss:home.org", "Boss", "@boss:home.org")
            .unwrap();
        assert_eq!(user.role, UserRole::Admin);
        let _ = std::fs::remove_file(tmp_db("admin_promo"));
    }

    // --- unknown channel identity creates new Guest user ----------------------

    #[test]
    fn test_unknown_channel_creates_guest() {
        let store = open("guest_create");
        // Seed first admin so the next user is not forced to Admin.
        store.create_user("Seed", None, UserRole::Admin).unwrap();

        let user = store
            .get_or_create_matrix_user("@stranger:home.org", "Stranger", "")
            .unwrap();
        assert_eq!(user.role, UserRole::Guest);
        let _ = std::fs::remove_file(tmp_db("guest_create"));
    }

    // --- display name truncation ----------------------------------------------

    #[test]
    fn test_display_name_truncated_to_100_chars() {
        let long_name: String = "A".repeat(150);
        let truncated = truncate_display_name(&long_name);
        assert_eq!(truncated.chars().count(), 100);
    }

    // --- last-admin guard -----------------------------------------------------

    #[test]
    fn test_cannot_demote_last_admin() {
        let store = open("last_admin");
        let u = store.create_user("Solo", None, UserRole::Admin).unwrap();
        let result = store.set_role(&u.user_id, UserRole::Guest);
        assert!(
            result.is_err(),
            "Should not be able to demote the last admin"
        );
        let _ = std::fs::remove_file(tmp_db("last_admin"));
    }

    #[test]
    fn test_cannot_disable_last_admin() {
        let store = open("last_admin_disable");
        let u = store.create_user("Solo", None, UserRole::Admin).unwrap();
        let result = store.set_enabled(&u.user_id, false);
        assert!(
            result.is_err(),
            "Should not be able to disable the last admin"
        );
        let _ = std::fs::remove_file(tmp_db("last_admin_disable"));
    }

    // --- delete cascades channel identities -----------------------------------

    #[test]
    fn test_delete_removes_channel_identities() {
        use crate::users::identity::ChannelType;
        let store = open("delete_cascade");
        // Create two users so the first can be deleted.
        let u = store.create_user("Deletable", None, UserRole::Admin).unwrap();
        let _u2 = store.create_user("Keeper", None, UserRole::Admin).unwrap();
        store.set_role(&u.user_id, UserRole::Member).unwrap();

        // Link a channel identity to the deletable user.
        store
            .link_channel(&u.user_id, ChannelType::Matrix, "@gone:home.org", false)
            .unwrap();

        // Delete the user.
        store.delete(&u.user_id).unwrap();

        // The channel identity should also be gone.
        let found = store
            .get_by_channel(ChannelType::Matrix, "@gone:home.org")
            .unwrap();
        assert!(
            found.is_none(),
            "Channel identity should be removed when user is deleted"
        );
        let _ = std::fs::remove_file(tmp_db("delete_cascade"));
    }

    // --- wrong key fails cleanly ----------------------------------------------

    #[test]
    fn test_wrong_key_rejected() {
        let path = tmp_db("wrong_key");
        UserStore::new(&path, &test_key()).unwrap();
        let wrong_key = vec![0xFFu8; 32];
        let result = UserStore::new(&path, &wrong_key);
        assert!(result.is_err());
        let _ = std::fs::remove_file(path);
    }

    // --- user_data_dir helper -------------------------------------------------

    #[test]
    fn test_user_data_dir() {
        let base = Path::new("/tmp/lumina_data");
        let uid = "some-uuid-here";
        let dir = user_data_dir(base, uid);
        assert_eq!(dir, PathBuf::from("/tmp/lumina_data/users/some-uuid-here"));
    }

    // --- apply_admin_matrix_promotion -----------------------------------------

    #[test]
    fn test_apply_admin_promotion_upgrades_existing_member() {
        let store = open("apply_promo");
        // Seed so next user is not force-Admin.
        store.create_user("Seed", None, UserRole::Admin).unwrap();

        let member = store
            .create_user("the operator", Some("@operator:home.org"), UserRole::Member)
            .unwrap();
        assert_eq!(member.role, UserRole::Member);

        store.apply_admin_matrix_promotion("@operator:home.org").unwrap();

        let promoted = store.get_by_id(&member.user_id).unwrap().unwrap();
        assert_eq!(promoted.role, UserRole::Admin);
        let _ = std::fs::remove_file(tmp_db("apply_promo"));
    }

    // --- utc_now and days_to_ymd calendar math --------------------------------

    #[test]
    fn test_utc_now_format() {
        let ts = utc_now();
        // Must match YYYY-MM-DDTHH:MM:SSZ
        assert_eq!(ts.len(), 20, "timestamp length wrong: {}", ts);
        assert_eq!(&ts[10..11], "T");
        assert_eq!(&ts[19..20], "Z");
    }

    #[test]
    fn test_days_to_ymd_epoch() {
        // Unix epoch is 1970-01-01
        assert_eq!(days_to_ymd(0), (1970, 1, 1));
    }

    #[test]
    fn test_days_to_ymd_leap_year() {
        // 2000-02-29 exists (2000 is a leap year)
        // Days from 1970-01-01 to 2000-02-29:
        // 30 years: 1970..1999, with leap years at 72,76,80,84,88,92,96 (7 leaps)
        // = 30*365 + 7 = 10957 + 60 - 1 (Feb 29 is the 60th day)
        // Actually let's just verify Feb 29 of a leap year parses correctly.
        let (y, m, d) = days_to_ymd(11016); // 2000-02-29
        assert_eq!((y, m, d), (2000, 2, 29));
    }

    #[test]
    fn test_is_leap() {
        assert!(is_leap(2000));
        assert!(is_leap(2024));
        assert!(!is_leap(1900));
        assert!(!is_leap(2023));
    }

    // --- validate_user_id (P2-02) ─────────────────────────────────────────

    #[test]
    fn test_validate_user_id_valid() {
        assert!(validate_user_id("alice").is_ok());
        assert!(validate_user_id("user-123").is_ok());
        assert!(validate_user_id("user_abc").is_ok());
        assert!(validate_user_id("system").is_ok());
        assert!(validate_user_id("ABC123").is_ok());
    }

    #[test]
    fn test_validate_user_id_empty_rejected() {
        assert!(validate_user_id("").is_err());
    }

    #[test]
    fn test_validate_user_id_path_traversal_rejected() {
        assert!(validate_user_id("../etc").is_err());
        assert!(validate_user_id("foo/bar").is_err());
        assert!(validate_user_id("foo\\bar").is_err());
        assert!(validate_user_id("@operator:home.org").is_err()); // colons not allowed
    }

    #[test]
    fn test_validate_user_id_spaces_rejected() {
        assert!(validate_user_id("user name").is_err());
        assert!(validate_user_id(" ").is_err());
    }

    #[test]
    fn test_validate_user_id_too_long_rejected() {
        let long_id = "a".repeat(129);
        assert!(validate_user_id(&long_id).is_err(), "128+ char user_id should be rejected");

        let max_ok = "a".repeat(128);
        assert!(validate_user_id(&max_ok).is_ok(), "128 char user_id should be accepted");
    }
}
