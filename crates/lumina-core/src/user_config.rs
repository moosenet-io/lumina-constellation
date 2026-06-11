//! P2-03: Per-user tool permissions and system prompt.
//!
//! Each user has their own tool permission set and system prompt override.
//!
//! ## Design
//!
//! - `UserToolConfig` holds the per-user delta over global policy:
//!   - `extra_allowed_tools`: tools added on top of global allowlist
//!   - `denied_tools`: tools blocked even if globally allowed
//!   - `system_prompt_override`: optional custom system prompt
//! - `UserConfig` is a SQLCipher-backed store at
//!   `{base}/users/{user_id}/config.db` (per-user database).
//!   All write operations are wrapped in transactions for atomicity.
//! - `check_user_permission` evaluates global allowlist + user config.
//!   Call via `ToolGate::check_user_permission` for the integrated path.
//!
//! ## Access rules
//! - Guest users always have zero tools (enforced by callers via `UserRole`).
//! - Admin users inherit all global tools; user overrides are additive.
//! - Member users start from their granted list (`extra_allowed_tools`),
//!   subject to global ceiling.
//! - A tool in `denied_tools` is blocked even when globally allowed.

use crate::error::{LuminaError, Result};
use crate::users::user_data_dir;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

// ── Data types ────────────────────────────────────────────────────────────────

/// Per-user tool permission configuration.
///
/// `user_id` is the stable identifier this config belongs to.
/// `extra_allowed_tools` and `denied_tools` use `HashSet` for O(1) lookup
/// since `check_user_permission` is called on every tool execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserToolConfig {
    /// User this config belongs to.
    pub user_id: String,
    /// Tool names explicitly granted to this user on top of global policy.
    /// For Admin users this is ignored (they already inherit everything).
    pub extra_allowed_tools: HashSet<String>,
    /// Tool names blocked for this user, even if globally allowed.
    pub denied_tools: HashSet<String>,
    /// Optional custom system prompt for this user.
    pub system_prompt_override: Option<String>,
}

impl Default for UserToolConfig {
    fn default() -> Self {
        Self {
            user_id: String::new(),
            extra_allowed_tools: HashSet::new(),
            denied_tools: HashSet::new(),
            system_prompt_override: None,
        }
    }
}

impl UserToolConfig {
    /// Return `true` if the tool is explicitly denied for this user.
    pub fn is_denied(&self, tool_name: &str) -> bool {
        self.denied_tools.contains(tool_name)
    }

    /// Return `true` if the tool is in this user's extra-allowed list.
    pub fn is_extra_allowed(&self, tool_name: &str) -> bool {
        self.extra_allowed_tools.contains(tool_name)
    }
}

// ── UserConfig store ──────────────────────────────────────────────────────────

/// SQLCipher-backed per-user configuration store.
///
/// Database path: `{base}/users/{user_id}/config.db`
pub struct UserConfig {
    conn: Connection,
}

impl UserConfig {
    /// Open (or create) the per-user config database.
    ///
    /// `db_path` is typically obtained from [`user_config_db_path`].
    /// `key` is a 32-byte SQLCipher encryption key.
    pub fn new(db_path: &Path, key: &[u8]) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                LuminaError::Config(format!("Cannot create user config directory: {}", e))
            })?;
        }

        let conn = Connection::open(db_path).map_err(|e| {
            LuminaError::Config(format!("Cannot open user config database: {}", e))
        })?;

        // SQLCipher: set key before any other operation.
        let hex_key = hex::encode(key);
        conn.execute_batch(&format!("PRAGMA key = \"x'{}'\";", hex_key))
            .map_err(|e| {
                LuminaError::Config(format!("Failed to set user config database key: {}", e))
            })?;

        // Verify key is correct.
        conn.execute_batch("SELECT count(*) FROM sqlite_master;")
            .map_err(|_| {
                LuminaError::SecurityViolation(
                    "User config database key is incorrect".to_string(),
                )
            })?;

        conn.execute_batch("PRAGMA journal_mode = WAL;").map_err(|e| {
            LuminaError::Config(format!("Failed to enable WAL mode on user config db: {}", e))
        })?;

        let store = Self { conn };
        store.ensure_schema()?;
        Ok(store)
    }

    // ── Tool config ───────────────────────────────────────────────────────────

    /// Retrieve the tool configuration for `user_id`.
    ///
    /// Returns a default (empty) `UserToolConfig` if nothing has been stored yet.
    pub fn get_tool_config(&self, user_id: &str) -> Result<UserToolConfig> {
        // Fetch all rows for this user from the user_tools table.
        let mut stmt = self
            .conn
            .prepare(
                "SELECT tool_name, permission_level FROM user_tools WHERE user_id = ?1",
            )
            .map_err(|e| LuminaError::Config(format!("get_tool_config prepare failed: {}", e)))?;

        let rows: Vec<(String, String)> = stmt
            .query_map(params![user_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| LuminaError::Config(format!("get_tool_config query failed: {}", e)))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| LuminaError::Config(format!("get_tool_config row error: {}", e)))?;

        let mut extra_allowed = HashSet::new();
        let mut denied = HashSet::new();

        for (tool_name, permission_level) in rows {
            match permission_level.as_str() {
                "allow" => { extra_allowed.insert(tool_name); }
                "deny" => { denied.insert(tool_name); }
                _ => {} // unknown level — ignore
            }
        }

        // Retrieve system prompt from key-value settings.
        let system_prompt = self.get_system_prompt(user_id)?;

        Ok(UserToolConfig {
            user_id: user_id.to_string(),
            extra_allowed_tools: extra_allowed,
            denied_tools: denied,
            system_prompt_override: system_prompt,
        })
    }

    /// Persist the full tool configuration for `user_id`.
    ///
    /// All changes are applied atomically in a single transaction so a
    /// partial failure cannot leave the user with a wiped config.
    ///
    /// Returns an error if a tool name appears in both `extra_allowed_tools`
    /// and `denied_tools` — conflicting state is rejected at write time.
    pub fn set_tool_config(&self, user_id: &str, config: &UserToolConfig) -> Result<()> {
        // Debug-assert that caller isn't accidentally mixing up user IDs.
        debug_assert!(
            config.user_id.is_empty() || config.user_id == user_id,
            "user_id parameter '{}' does not match config.user_id '{}'",
            user_id,
            config.user_id,
        );

        // Reject conflicting state: a tool cannot be both allowed and denied.
        for tool in &config.extra_allowed_tools {
            if config.denied_tools.contains(tool) {
                return Err(LuminaError::Config(format!(
                    "Tool '{}' cannot be in both extra_allowed_tools and denied_tools",
                    tool
                )));
            }
        }

        self.conn
            .execute_batch("BEGIN IMMEDIATE;")
            .map_err(|e| LuminaError::Config(format!("set_tool_config BEGIN failed: {}", e)))?;

        let result = self.set_tool_config_txn(user_id, config);

        if result.is_ok() {
            self.conn
                .execute_batch("COMMIT;")
                .map_err(|e| LuminaError::Config(format!("set_tool_config COMMIT failed: {}", e)))?;
        } else {
            // Best-effort rollback — ignore rollback errors so the original
            // error propagates cleanly to the caller.
            let _ = self.conn.execute_batch("ROLLBACK;");
        }

        result
    }

    /// Inner (non-transactional) body of `set_tool_config` — called inside
    /// the explicit transaction opened by `set_tool_config`.
    fn set_tool_config_txn(&self, user_id: &str, config: &UserToolConfig) -> Result<()> {
        // Remove existing rows for this user.
        self.conn
            .execute(
                "DELETE FROM user_tools WHERE user_id = ?1",
                params![user_id],
            )
            .map_err(|e| LuminaError::Config(format!("set_tool_config delete failed: {}", e)))?;

        // Insert allowed tools.
        for tool in &config.extra_allowed_tools {
            self.conn
                .execute(
                    "INSERT INTO user_tools (user_id, tool_name, permission_level)
                     VALUES (?1, ?2, 'allow')",
                    params![user_id, tool],
                )
                .map_err(|e| {
                    LuminaError::Config(format!("set_tool_config insert allow failed: {}", e))
                })?;
        }

        // Insert denied tools.
        for tool in &config.denied_tools {
            self.conn
                .execute(
                    "INSERT INTO user_tools (user_id, tool_name, permission_level)
                     VALUES (?1, ?2, 'deny')",
                    params![user_id, tool],
                )
                .map_err(|e| {
                    LuminaError::Config(format!("set_tool_config insert deny failed: {}", e))
                })?;
        }

        // Update system prompt.
        match &config.system_prompt_override {
            Some(prompt) => self.set_system_prompt(user_id, prompt)?,
            None => self.clear_system_prompt(user_id)?,
        }

        Ok(())
    }

    // ── System prompt ─────────────────────────────────────────────────────────

    /// Retrieve the custom system prompt for `user_id`, or `None` if not set.
    pub fn get_system_prompt(&self, user_id: &str) -> Result<Option<String>> {
        let result = self
            .conn
            .query_row(
                "SELECT value FROM user_config_settings WHERE user_id = ?1 AND key = 'system_prompt'",
                params![user_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| LuminaError::Config(format!("get_system_prompt failed: {}", e)))?;
        Ok(result)
    }

    /// Set the custom system prompt for `user_id`.
    ///
    /// The caller is responsible for enforcing any maximum length policy before
    /// calling this method.
    pub fn set_system_prompt(&self, user_id: &str, prompt: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO user_config_settings (user_id, key, value)
                 VALUES (?1, 'system_prompt', ?2)
                 ON CONFLICT(user_id, key) DO UPDATE SET value = excluded.value",
                params![user_id, prompt],
            )
            .map_err(|e| LuminaError::Config(format!("set_system_prompt failed: {}", e)))?;
        Ok(())
    }

    /// Remove a previously set system prompt, reverting to the global default.
    fn clear_system_prompt(&self, user_id: &str) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM user_config_settings WHERE user_id = ?1 AND key = 'system_prompt'",
                params![user_id],
            )
            .map_err(|e| LuminaError::Config(format!("clear_system_prompt failed: {}", e)))?;
        Ok(())
    }

    // ── Schema ────────────────────────────────────────────────────────────────

    fn ensure_schema(&self) -> Result<()> {
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS user_tools (
                    user_id          TEXT NOT NULL,
                    tool_name        TEXT NOT NULL,
                    permission_level TEXT NOT NULL,
                    PRIMARY KEY (user_id, tool_name)
                );
                CREATE INDEX IF NOT EXISTS idx_user_tools_user
                    ON user_tools (user_id);
                CREATE TABLE IF NOT EXISTS user_config_settings (
                    user_id TEXT NOT NULL,
                    key     TEXT NOT NULL,
                    value   TEXT NOT NULL,
                    PRIMARY KEY (user_id, key)
                );
                CREATE INDEX IF NOT EXISTS idx_user_cfg_settings_user
                    ON user_config_settings (user_id);",
            )
            .map_err(|e| LuminaError::Config(format!("UserConfig schema creation failed: {}", e)))
    }
}

// ── Path helpers ──────────────────────────────────────────────────────────────

/// Return the per-user config database path: `{base}/users/{user_id}/config.db`.
///
/// Visibility is `pub(crate)` to match [`crate::users::user_data_dir`] which
/// it wraps. Callers must validate `user_id` via
/// [`crate::users::validate_user_id`] before calling this function to
/// prevent path-traversal attacks from externally-supplied user IDs.
pub(crate) fn user_config_db_path(base: &Path, user_id: &str) -> PathBuf {
    user_data_dir(base, user_id).join("config.db")
}

// ── Permission check ──────────────────────────────────────────────────────────

/// Check whether `tool_name` is permitted given a per-user config and a
/// caller-supplied global allowed list.
///
/// This utility function is used by unit tests and by
/// [`crate::tool_gate::ToolGate::check_user_permission`], which supplies the
/// global list from the gate's internal allowlist (via `HashMap::contains_key`
/// directly for O(1) lookup — see that method for the production path).
///
/// ## Decision order
/// 1. Tool in `config.denied_tools` → `false` (user-deny always blocks).
/// 2. Tool in `global_allowed` → `true` (globally permitted, not user-denied).
/// 3. Tool in `config.extra_allowed_tools` → `true` (user grant adds capability).
/// 4. Otherwise → `false`.
///
/// ## Additive model (design decision)
/// This implementation is **additive**: all globally-allowed tools are available
/// to Member users unless explicitly denied, and `extra_allowed_tools` adds
/// further capability beyond the global ceiling. This differs from the spec
/// language ("ONLY listed tools available") which implies a restrictive model.
/// The additive model was chosen because:
/// - Restrictive model requires admin to replicate the entire global allowlist
///   for every new user — operationally burdensome.
/// - Deny-list (`denied_tools`) provides the restriction mechanism when needed.
/// - Per-user grants (`extra_allowed_tools`) enable fine-grained additions.
/// If a strict restrictive model is needed in future, add a `UserRole`-aware
/// branch here.
///
/// **Caller responsibility:** Guest users must be rejected before reaching
/// this function (return `false` based on role, not here).
pub(crate) fn check_user_permission(
    tool_name: &str,
    config: &UserToolConfig,
    global_allowed: &[String],
) -> bool {
    // Rule 1: user-denied tools are always blocked.
    if config.is_denied(tool_name) {
        return false;
    }

    // Rule 2: tool globally allowed.
    if global_allowed.iter().any(|t| t == tool_name) {
        return true;
    }

    // Rule 3: tool in user's extra-allowed list.
    if config.is_extra_allowed(tool_name) {
        return true;
    }

    false
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_key() -> Vec<u8> {
        vec![0xABu8; 32]
    }

    fn tmp_db(name: &str) -> PathBuf {
        let p = PathBuf::from(format!("/tmp/lumina_user_config_test_{}.db", name));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn open(name: &str) -> UserConfig {
        UserConfig::new(&tmp_db(name), &test_key()).expect("open UserConfig")
    }

    // ── get/set tool config ───────────────────────────────────────────────────

    #[test]
    fn test_get_tool_config_empty_returns_default() {
        let uc = open("empty_config");
        let cfg = uc.get_tool_config("user-1").unwrap();
        assert_eq!(cfg.user_id, "user-1");
        assert!(cfg.extra_allowed_tools.is_empty());
        assert!(cfg.denied_tools.is_empty());
        assert!(cfg.system_prompt_override.is_none());
        let _ = std::fs::remove_file(tmp_db("empty_config"));
    }

    #[test]
    fn test_set_and_get_tool_config_round_trip() {
        let uc = open("set_get");
        let config = UserToolConfig {
            user_id: "alice".to_string(),
            extra_allowed_tools: ["search".to_string(), "calendar".to_string()]
                .into_iter()
                .collect(),
            denied_tools: ["admin_panel".to_string()].into_iter().collect(),
            system_prompt_override: None,
        };
        uc.set_tool_config("alice", &config).unwrap();

        let loaded = uc.get_tool_config("alice").unwrap();
        assert_eq!(loaded.user_id, "alice");
        assert!(loaded.extra_allowed_tools.contains("search"));
        assert!(loaded.extra_allowed_tools.contains("calendar"));
        assert_eq!(loaded.extra_allowed_tools.len(), 2);
        assert!(loaded.denied_tools.contains("admin_panel"));
        assert!(loaded.system_prompt_override.is_none());
        let _ = std::fs::remove_file(tmp_db("set_get"));
    }

    #[test]
    fn test_set_tool_config_replaces_previous() {
        let uc = open("replace");
        let first = UserToolConfig {
            user_id: "bob".to_string(),
            extra_allowed_tools: ["old_tool".to_string()].into_iter().collect(),
            denied_tools: HashSet::new(),
            system_prompt_override: None,
        };
        uc.set_tool_config("bob", &first).unwrap();

        let second = UserToolConfig {
            user_id: "bob".to_string(),
            extra_allowed_tools: ["new_tool".to_string()].into_iter().collect(),
            denied_tools: ["blocked".to_string()].into_iter().collect(),
            system_prompt_override: None,
        };
        uc.set_tool_config("bob", &second).unwrap();

        let loaded = uc.get_tool_config("bob").unwrap();
        assert!(loaded.extra_allowed_tools.contains("new_tool"));
        assert!(loaded.denied_tools.contains("blocked"));
        // old_tool must be gone
        assert!(!loaded.extra_allowed_tools.contains("old_tool"));
        let _ = std::fs::remove_file(tmp_db("replace"));
    }

    // ── system prompt override ────────────────────────────────────────────────

    #[test]
    fn test_system_prompt_set_and_get() {
        let uc = open("prompt");
        uc.set_system_prompt("carol", "You are a helpful assistant named Carol.").unwrap();
        let prompt = uc.get_system_prompt("carol").unwrap();
        assert_eq!(
            prompt,
            Some("You are a helpful assistant named Carol.".to_string())
        );
        let _ = std::fs::remove_file(tmp_db("prompt"));
    }

    #[test]
    fn test_system_prompt_default_is_none() {
        let uc = open("prompt_none");
        let prompt = uc.get_system_prompt("dave").unwrap();
        assert!(prompt.is_none());
        let _ = std::fs::remove_file(tmp_db("prompt_none"));
    }

    #[test]
    fn test_system_prompt_via_tool_config() {
        let uc = open("prompt_via_config");
        let config = UserToolConfig {
            user_id: "eve".to_string(),
            extra_allowed_tools: HashSet::new(),
            denied_tools: HashSet::new(),
            system_prompt_override: Some("Custom Eve prompt.".to_string()),
        };
        uc.set_tool_config("eve", &config).unwrap();

        let loaded = uc.get_tool_config("eve").unwrap();
        assert_eq!(
            loaded.system_prompt_override,
            Some("Custom Eve prompt.".to_string())
        );
        let _ = std::fs::remove_file(tmp_db("prompt_via_config"));
    }

    #[test]
    fn test_clear_system_prompt_via_none_override() {
        let uc = open("clear_prompt");
        uc.set_system_prompt("frank", "Initial prompt").unwrap();
        assert!(uc.get_system_prompt("frank").unwrap().is_some());

        // Setting config with None removes the prompt.
        let config = UserToolConfig {
            user_id: "frank".to_string(),
            extra_allowed_tools: HashSet::new(),
            denied_tools: HashSet::new(),
            system_prompt_override: None,
        };
        uc.set_tool_config("frank", &config).unwrap();
        assert!(uc.get_system_prompt("frank").unwrap().is_none());
        let _ = std::fs::remove_file(tmp_db("clear_prompt"));
    }

    // ── check_user_permission ─────────────────────────────────────────────────

    #[test]
    fn test_deny_overrides_global_allow() {
        let config = UserToolConfig {
            user_id: "grace".to_string(),
            extra_allowed_tools: HashSet::new(),
            denied_tools: ["dangerous_tool".to_string()].into_iter().collect(),
            system_prompt_override: None,
        };
        let global = vec!["dangerous_tool".to_string(), "safe_tool".to_string()];

        // Tool is globally allowed but user-denied — must be blocked.
        assert!(
            !check_user_permission("dangerous_tool", &config, &global),
            "User-denied tool must be blocked even when globally allowed"
        );
    }

    #[test]
    fn test_globally_allowed_tool_permitted() {
        let config = UserToolConfig {
            user_id: "heidi".to_string(),
            extra_allowed_tools: HashSet::new(),
            denied_tools: HashSet::new(),
            system_prompt_override: None,
        };
        let global = vec!["safe_tool".to_string()];

        assert!(
            check_user_permission("safe_tool", &config, &global),
            "Globally allowed tool should pass when user has no deny"
        );
    }

    #[test]
    fn test_extra_allow_grants_access_beyond_global() {
        let config = UserToolConfig {
            user_id: "ivan".to_string(),
            extra_allowed_tools: ["special_tool".to_string()].into_iter().collect(),
            denied_tools: HashSet::new(),
            system_prompt_override: None,
        };
        let global: Vec<String> = vec![];

        // special_tool not globally allowed, but user has it extra-allowed.
        assert!(
            check_user_permission("special_tool", &config, &global),
            "Extra-allowed tool should be accessible even if not globally allowed"
        );
    }

    #[test]
    fn test_not_allowed_tool_blocked() {
        let config = UserToolConfig {
            user_id: "judy".to_string(),
            extra_allowed_tools: HashSet::new(),
            denied_tools: HashSet::new(),
            system_prompt_override: None,
        };
        let global: Vec<String> = vec!["other_tool".to_string()];

        assert!(
            !check_user_permission("unknown_tool", &config, &global),
            "Tool not in global or user allowlist should be blocked"
        );
    }

    #[test]
    fn test_set_tool_config_rejects_conflicting_allow_and_deny() {
        // Conflicting state (tool in both lists) must be rejected at write time.
        let uc = open("conflict");
        let config = UserToolConfig {
            user_id: "mallory".to_string(),
            extra_allowed_tools: ["contested_tool".to_string()].into_iter().collect(),
            denied_tools: ["contested_tool".to_string()].into_iter().collect(),
            system_prompt_override: None,
        };
        let result = uc.set_tool_config("mallory", &config);
        assert!(
            result.is_err(),
            "set_tool_config must reject a tool appearing in both allowed and denied"
        );
        let _ = std::fs::remove_file(tmp_db("conflict"));
    }

    #[test]
    fn test_deny_takes_priority_over_global_allow() {
        // check_user_permission: deny wins over global allowlist (no conflict).
        let config = UserToolConfig {
            user_id: "mallory".to_string(),
            extra_allowed_tools: HashSet::new(),
            denied_tools: ["global_tool".to_string()].into_iter().collect(),
            system_prompt_override: None,
        };
        let global: Vec<String> = vec!["global_tool".to_string()];

        assert!(
            !check_user_permission("global_tool", &config, &global),
            "Deny must win over global allow"
        );
    }

    // ── user_config_db_path ───────────────────────────────────────────────────

    #[test]
    fn test_user_config_db_path() {
        let base = Path::new("/tmp/lumina");
        let path = user_config_db_path(base, "abc-123");
        assert_eq!(
            path,
            PathBuf::from("/tmp/lumina/users/abc-123/config.db")
        );
    }

    // ── wrong key rejected ────────────────────────────────────────────────────

    #[test]
    fn test_wrong_key_rejected() {
        let path = tmp_db("wrong_key");
        UserConfig::new(&path, &test_key()).unwrap();
        let wrong_key = vec![0x00u8; 32];
        let result = UserConfig::new(&path, &wrong_key);
        assert!(result.is_err(), "Wrong key should be rejected");
        let _ = std::fs::remove_file(path);
    }
}
