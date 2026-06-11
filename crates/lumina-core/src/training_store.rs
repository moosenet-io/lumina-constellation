//! FORGE-03: SQLCipher-encrypted training data store.
//!
//! Every conversation turn is logged with routing metadata for future LoRA
//! fine-tuning. The database is AES-256 encrypted at rest via SQLCipher.
//! The encryption key is stored in vault; auto-generated on first run.
//!
//! Also implements HARDEN-06 data retention: cleanup_expired() deletes
//! uncurated turns older than the configured retention window.
//!
//! P2-02: Per-user isolation. Each user has their own database file at
//! `~/.lumina/users/{user_id}/training.db`. The schema also includes a
//! `user_id` column so that migrations on shared databases remain safe.
//! Existing callers that do not supply a `user_id` pass `"default"` for
//! backward compatibility.

use crate::error::{LuminaError, Result};
use crate::users::user_data_dir;
use crate::vault;
use rand::RngCore;
use rusqlite::{params, Connection};
use secrecy::ExposeSecret;
use std::io::Write;
use std::path::{Path, PathBuf};

// ── Public data types ──────────────────────────────────────────────────────

/// One conversation turn to be stored.
#[derive(Debug, Clone)]
pub struct ConversationTurn {
    pub session_id: String,
    pub user_id: String,
    pub user_input: String,
    pub assistant_output: String,
    pub system_prompt: Option<String>,
    pub model_used: String,
    pub escalated: bool,
    pub router_decision: String,
    pub duration_ms: i64,
}

/// Aggregated statistics about the training dataset.
#[derive(Debug, Clone)]
pub struct TrainingStats {
    pub total_turns: i64,
    pub pending: i64,
    pub approved: i64,
    pub rejected: i64,
    pub edited: i64,
    pub oldest: Option<String>,
    pub newest: Option<String>,
}

// ── TrainingStore ──────────────────────────────────────────────────────────

pub struct TrainingStore {
    conn: Connection,
}

impl TrainingStore {
    /// Open (or create) the SQLCipher database at `db_path` with `key`.
    ///
    /// On first open the schema is created. On subsequent opens the key is
    /// verified by running a test query — a wrong key produces a clear error.
    pub fn open(db_path: &Path, key: &[u8]) -> Result<Self> {
        // Create parent directory if needed
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| LuminaError::Config(format!("Cannot create database directory: {}", e)))?;
        }

        let conn = Connection::open(db_path)
            .map_err(|e| LuminaError::Config(format!("Cannot open training database: {}", e)))?;

        // SQLCipher: set key before any other operation
        let hex_key = hex::encode(key);
        conn.execute_batch(&format!("PRAGMA key = \"x'{}'\";", hex_key))
            .map_err(|e| LuminaError::Config(format!("Failed to set database key: {}", e)))?;

        // Verify the key is correct by running a harmless query
        conn.execute_batch("SELECT count(*) FROM sqlite_master;")
            .map_err(|_| LuminaError::SecurityViolation(
                "Training database key is incorrect — cannot open database".to_string()
            ))?;

        // WAL mode: better concurrent write performance, no data loss on crash
        conn.execute_batch("PRAGMA journal_mode = WAL;")
            .map_err(|e| LuminaError::Config(format!("Failed to enable WAL mode: {}", e)))?;

        let store = Self { conn };
        store.ensure_schema()?;
        Ok(store)
    }

    /// Open (or create) a per-user database under `base/users/{user_id}/training.db`.
    ///
    /// `base` is typically `~/.lumina`. The directory is created automatically.
    /// `user_id` is validated to contain only safe path characters (ASCII alphanumeric,
    /// hyphens, underscores) — returns an error on invalid input.
    /// Pass `"system"` for the legacy/anonymous user slot (matches the DB column default).
    pub fn open_for_user_at(base: &Path, user_id: &str, key: &[u8]) -> Result<Self> {
        crate::users::validate_user_id(user_id)?;
        let dir = user_data_dir(base, user_id);
        std::fs::create_dir_all(&dir).map_err(|e| {
            LuminaError::Config(format!(
                "Cannot create training database directory for user {}: {}",
                user_id, e
            ))
        })?;
        let db_path = dir.join("training.db");
        Self::open(&db_path, key)
    }

    /// Open using the default path (~/.lumina/training.db) and key from vault.
    ///
    /// Auto-generates the key on first run and persists it to vault.
    pub fn open_default() -> Result<Self> {
        let db_path = default_db_path();
        let key = get_or_create_training_key()?;
        Self::open(&db_path, &key)
    }

    /// Open using a per-user namespaced path under `~/.lumina/users/{user_id}/training.db`.
    ///
    /// Uses the same vault key as the shared store. Pass `"system"` for the
    /// anonymous/legacy user slot (matches the DB column default).
    pub fn open_for_user(user_id: &str) -> Result<Self> {
        let base = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".lumina");
        let key = get_or_create_training_key()?;
        Self::open_for_user_at(&base, user_id, &key)
    }

    /// Insert one conversation turn. Returns the new row id.
    ///
    /// Insert failures are non-fatal for the agent loop — the caller should
    /// log and continue rather than propagating the error.
    pub fn insert_turn(&self, turn: &ConversationTurn) -> Result<i64> {
        let id = self.conn.execute(
            "INSERT INTO conversations (
                session_id, timestamp, user_input, assistant_output,
                system_prompt, model_used, escalated, router_decision,
                duration_ms, curation_status, created_at, user_id
            ) VALUES (?1, datetime('now'), ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending', datetime('now'), ?9)",
            params![
                turn.session_id,
                turn.user_input,
                turn.assistant_output,
                turn.system_prompt,
                turn.model_used,
                turn.escalated as i32,
                turn.router_decision,
                turn.duration_ms,
                turn.user_id,
            ],
        )
        .map_err(|e| LuminaError::Config(format!("Failed to insert training turn: {}", e)))?;

        Ok(self.conn.last_insert_rowid())
    }

    /// Export approved and edited turns to OpenAI fine-tuning JSONL.
    ///
    /// Uses edited_output when available, assistant_output otherwise.
    /// Returns the count of exported turns.
    pub fn export_jsonl(&self, path: &Path, system_prompt: &str) -> Result<usize> {
        let mut stmt = self.conn.prepare(
            "SELECT user_input, COALESCE(edited_output, assistant_output)
             FROM conversations
             WHERE curation_status IN ('approved', 'edited')
             ORDER BY created_at ASC",
        )
        .map_err(|e| LuminaError::Config(format!("Export query failed: {}", e)))?;

        let mut file = std::fs::File::create(path)
            .map_err(|e| LuminaError::Config(format!("Cannot create export file: {}", e)))?;

        let mut count = 0usize;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| LuminaError::Config(format!("Export iteration failed: {}", e)))?;

        for row in rows {
            let (user_input, output) = row
                .map_err(|e| LuminaError::Config(format!("Row read error: {}", e)))?;

            let record = serde_json::json!({
                "messages": [
                    {"role": "system",    "content": system_prompt},
                    {"role": "user",      "content": user_input},
                    {"role": "assistant", "content": output},
                ]
            });

            writeln!(file, "{}", record)
                .map_err(|e| LuminaError::Config(format!("Write error during export: {}", e)))?;
            count += 1;
        }

        Ok(count)
    }

    /// Return pending turns (not yet reviewed), oldest first.
    pub fn get_pending(&self, limit: usize) -> Result<Vec<(i64, ConversationTurn)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, user_input, assistant_output, system_prompt,
                    model_used, escalated, router_decision, duration_ms, user_id
             FROM conversations
             WHERE curation_status = 'pending'
             ORDER BY created_at ASC
             LIMIT ?1",
        )
        .map_err(|e| LuminaError::Config(format!("Pending query failed: {}", e)))?;

        let rows = stmt
            .query_map(params![limit as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    ConversationTurn {
                        session_id: row.get(1)?,
                        user_input: row.get(2)?,
                        assistant_output: row.get(3)?,
                        system_prompt: row.get(4)?,
                        model_used: row.get(5)?,
                        escalated: row.get::<_, i32>(6)? != 0,
                        router_decision: row.get(7)?,
                        duration_ms: row.get(8)?,
                        user_id: row.get(9)?,
                    },
                ))
            })
            .map_err(|e| LuminaError::Config(format!("Pending iteration failed: {}", e)))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| LuminaError::Config(format!("Row collection failed: {}", e)))?;

        Ok(rows)
    }

    /// Update curation status for a turn. `edited_output` is stored when status is "edited".
    pub fn mark_curated(
        &self,
        id: i64,
        status: &str,
        edited_output: Option<&str>,
    ) -> Result<()> {
        if !["approved", "rejected", "edited"].contains(&status) {
            return Err(LuminaError::Config(format!(
                "Invalid curation status '{}' — use approved, rejected, or edited",
                status
            )));
        }

        self.conn.execute(
            "UPDATE conversations SET curation_status = ?1, edited_output = ?2 WHERE id = ?3",
            params![status, edited_output, id],
        )
        .map_err(|e| LuminaError::Config(format!("mark_curated failed: {}", e)))?;

        Ok(())
    }

    /// Return dataset statistics.
    pub fn stats(&self) -> Result<TrainingStats> {
        let total: i64 = self.conn
            .query_row("SELECT COUNT(*) FROM conversations", [], |r| r.get(0))
            .map_err(|e| LuminaError::Config(format!("Stats query failed: {}", e)))?;

        let count_status = |s: &str| -> Result<i64> {
            self.conn
                .query_row(
                    "SELECT COUNT(*) FROM conversations WHERE curation_status = ?1",
                    params![s],
                    |r| r.get(0),
                )
                .map_err(|e| LuminaError::Config(format!("Status count failed: {}", e)))
        };

        let oldest: Option<String> = self.conn
            .query_row(
                "SELECT MIN(created_at) FROM conversations",
                [],
                |r| r.get(0),
            )
            .unwrap_or(None);

        let newest: Option<String> = self.conn
            .query_row(
                "SELECT MAX(created_at) FROM conversations",
                [],
                |r| r.get(0),
            )
            .unwrap_or(None);

        Ok(TrainingStats {
            total_turns: total,
            pending: count_status("pending")?,
            approved: count_status("approved")?,
            rejected: count_status("rejected")?,
            edited: count_status("edited")?,
            oldest,
            newest,
        })
    }

    /// HARDEN-06: Delete uncurated turns older than `retention_days` days.
    ///
    /// Approved and edited turns are never auto-deleted.
    pub fn cleanup_expired(&self, retention_days: u64) -> Result<usize> {
        let interval = format!("-{} days", retention_days);
        let deleted = self.conn.execute(
            "DELETE FROM conversations
             WHERE curation_status = 'pending'
             AND created_at < datetime('now', ?1)",
            params![interval],
        )
        .map_err(|e| LuminaError::Config(format!("Retention cleanup failed: {}", e)))?;
        Ok(deleted)
    }

    // ── Private helpers ────────────────────────────────────────────────────

    fn ensure_schema(&self) -> Result<()> {
        // Create table and indexes on columns that exist in all schema versions.
        // The user_id index is intentionally omitted here — it is created after the
        // migration below so that ALTER TABLE always runs before the index on the
        // new column (fixes ordering bug on pre-migration databases).
        self.conn.execute_batch("
            CREATE TABLE IF NOT EXISTS conversations (
                id               INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id       TEXT NOT NULL DEFAULT '',
                timestamp        TEXT NOT NULL,
                user_input       TEXT NOT NULL,
                assistant_output TEXT NOT NULL,
                system_prompt    TEXT,
                model_used       TEXT NOT NULL DEFAULT '',
                escalated        INTEGER NOT NULL DEFAULT 0,
                router_decision  TEXT NOT NULL DEFAULT '',
                duration_ms      INTEGER NOT NULL DEFAULT 0,
                curation_status  TEXT NOT NULL DEFAULT 'pending',
                edited_output    TEXT,
                created_at       TEXT NOT NULL,
                user_id          TEXT NOT NULL DEFAULT 'system'
            );
            CREATE INDEX IF NOT EXISTS idx_curation    ON conversations (curation_status);
            CREATE INDEX IF NOT EXISTS idx_created     ON conversations (created_at);
        ").map_err(|e| LuminaError::Config(format!("Schema creation failed: {}", e)))?;

        // P2-02 migration: add user_id column to existing databases that lack it.
        // ALTER TABLE in SQLite is fast — it does not rewrite rows.
        let has_col = self.conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('conversations') WHERE name = 'user_id'",
            [],
            |r| r.get::<_, i64>(0),
        ).unwrap_or(0);
        if has_col == 0 {
            self.conn.execute_batch(
                "ALTER TABLE conversations ADD COLUMN user_id TEXT NOT NULL DEFAULT 'system';"
            ).map_err(|e| LuminaError::Config(format!("Migration add user_id failed: {}", e)))?;
        }

        // Create the user_id index after the column is guaranteed to exist (fresh DB or
        // just-migrated DB). IF NOT EXISTS makes this a safe no-op on already-indexed DBs.
        self.conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_conv_user ON conversations (user_id);"
        ).map_err(|e| LuminaError::Config(format!("Migration create user_id index failed: {}", e)))?;

        Ok(())
    }
}

// ── Module-level helpers ───────────────────────────────────────────────────

/// Return the default database path: ~/.lumina/training.db
pub fn default_db_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".lumina")
        .join("training.db")
}

/// Get the training DB key from vault, or generate and store one on first run.
pub fn get_or_create_training_key() -> Result<Vec<u8>> {
    // Try vault
    if let Ok(vault_store) = vault::VaultStore::load() {
        if let Some(stored) = vault_store.get("LUMINA_TRAINING_DB_KEY") {
            if let Ok(bytes) = hex::decode(stored.expose_secret()) {
                if bytes.len() >= 32 {
                    return Ok(bytes);
                }
            }
        }
    }

    // Generate a new 32-byte key
    let mut key = vec![0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut key);
    let hex_key = hex::encode(&key);

    // Persist to vault (best-effort; if vault is unavailable, key is ephemeral)
    if let Ok(mut vault_store) = vault::VaultStore::load() {
        let _ = vault_store.set(
            "LUMINA_TRAINING_DB_KEY".to_string(),
            secrecy::SecretString::new(hex_key.into()),
        );
    }

    Ok(key)
}

/// HARDEN-06 (module-level): Delete rotated audit log files older than `retention_days`.
pub fn cleanup_old_rotated_logs(log_path: &Path, retention_days: u64) -> usize {
    use std::time::{Duration, SystemTime};

    let base = log_path.to_string_lossy().into_owned();
    let cutoff = match SystemTime::now().checked_sub(Duration::from_secs(retention_days * 86400)) {
        Some(t) => t,
        None => return 0,
    };

    let mut removed = 0;
    for i in 1..=99u32 {
        let gz = format!("{}.{}.gz", base, i);
        let p = Path::new(&gz);
        if !p.exists() {
            break;
        }
        if let Ok(meta) = std::fs::metadata(p) {
            if let Ok(modified) = meta.modified() {
                if modified < cutoff {
                    if std::fs::remove_file(p).is_ok() {
                        removed += 1;
                    }
                }
            }
        }
    }
    removed
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_key() -> Vec<u8> {
        vec![0u8; 32]
    }

    fn tmp_db(name: &str) -> PathBuf {
        let p = PathBuf::from(format!("/tmp/lumina_test_{}.db", name));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn sample_turn() -> ConversationTurn {
        ConversationTurn {
            session_id: "sess-1".to_string(),
            user_id: "system".to_string(),
            user_input: "hello, how are you?".to_string(),
            assistant_output: "I'm doing well, thank you!".to_string(),
            system_prompt: Some("You are Lumina.".to_string()),
            model_used: "lumina-fast".to_string(),
            escalated: false,
            router_decision: "category".to_string(),
            duration_ms: 120,
        }
    }

    #[test]
    fn test_open_create_schema() {
        let path = tmp_db("open");
        let store = TrainingStore::open(&path, &test_key()).unwrap();
        let stats = store.stats().unwrap();
        assert_eq!(stats.total_turns, 0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_insert_and_read_back() {
        let path = tmp_db("insert");
        let store = TrainingStore::open(&path, &test_key()).unwrap();

        let id = store.insert_turn(&sample_turn()).unwrap();
        assert!(id > 0);

        let stats = store.stats().unwrap();
        assert_eq!(stats.total_turns, 1);
        assert_eq!(stats.pending, 1);

        let pending = store.get_pending(10).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].1.user_input, "hello, how are you?");
        assert_eq!(pending[0].1.model_used, "lumina-fast");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_wrong_key_fails_with_clear_error() {
        let path = tmp_db("wrongkey");
        // Create with key A
        TrainingStore::open(&path, &test_key()).unwrap();

        // Reopen with different key B → should fail
        let wrong_key = vec![0xFFu8; 32];
        let result = TrainingStore::open(&path, &wrong_key);
        assert!(result.is_err(), "Wrong key should produce an error");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_export_jsonl_only_curated() {
        let path = tmp_db("export");
        let store = TrainingStore::open(&path, &test_key()).unwrap();

        let id = store.insert_turn(&sample_turn()).unwrap();

        // Export before approving → 0 rows
        let out = PathBuf::from("/tmp/lumina_test_export_out.jsonl");
        let count = store.export_jsonl(&out, "You are Lumina.").unwrap();
        assert_eq!(count, 0);

        // Approve then export → 1 row
        store.mark_curated(id, "approved", None).unwrap();
        let count = store.export_jsonl(&out, "You are Lumina.").unwrap();
        assert_eq!(count, 1);

        // Verify valid JSONL
        let content = std::fs::read_to_string(&out).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(parsed["messages"][0]["role"], "system");
        assert_eq!(parsed["messages"][1]["role"], "user");
        assert_eq!(parsed["messages"][2]["role"], "assistant");
        assert_eq!(parsed["messages"][1]["content"], "hello, how are you?");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn test_export_uses_edited_output() {
        let path = tmp_db("edited");
        let store = TrainingStore::open(&path, &test_key()).unwrap();

        let id = store.insert_turn(&sample_turn()).unwrap();
        store
            .mark_curated(id, "edited", Some("A better answer."))
            .unwrap();

        let out = PathBuf::from("/tmp/lumina_test_edited_out.jsonl");
        store.export_jsonl(&out, "System.").unwrap();

        let content = std::fs::read_to_string(&out).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(parsed["messages"][2]["content"], "A better answer.");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn test_export_rejected_turns_excluded() {
        let path = tmp_db("rejected");
        let store = TrainingStore::open(&path, &test_key()).unwrap();

        let id = store.insert_turn(&sample_turn()).unwrap();
        store.mark_curated(id, "rejected", None).unwrap();

        let out = PathBuf::from("/tmp/lumina_test_rejected_out.jsonl");
        let count = store.export_jsonl(&out, "System.").unwrap();
        assert_eq!(count, 0);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn test_mark_curated_invalid_status() {
        let path = tmp_db("badstatus");
        let store = TrainingStore::open(&path, &test_key()).unwrap();
        let id = store.insert_turn(&sample_turn()).unwrap();
        let result = store.mark_curated(id, "foobar", None);
        assert!(result.is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_stats_accurate() {
        let path = tmp_db("stats");
        let store = TrainingStore::open(&path, &test_key()).unwrap();

        let id1 = store.insert_turn(&sample_turn()).unwrap();
        let id2 = store.insert_turn(&sample_turn()).unwrap();
        let id3 = store.insert_turn(&sample_turn()).unwrap();
        let id4 = store.insert_turn(&sample_turn()).unwrap();

        store.mark_curated(id1, "approved", None).unwrap();
        store.mark_curated(id2, "rejected", None).unwrap();
        store.mark_curated(id3, "edited", Some("better")).unwrap();
        // id4 stays pending

        let stats = store.stats().unwrap();
        assert_eq!(stats.total_turns, 4);
        assert_eq!(stats.pending, 1);
        assert_eq!(stats.approved, 1);
        assert_eq!(stats.rejected, 1);
        assert_eq!(stats.edited, 1);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_cleanup_expired_pending_only() {
        let path = tmp_db("retention");
        let store = TrainingStore::open(&path, &test_key()).unwrap();

        let id = store.insert_turn(&sample_turn()).unwrap();
        store.mark_curated(id, "approved", None).unwrap();
        let _id2 = store.insert_turn(&sample_turn()).unwrap();

        // 0-day retention: delete pending records older than "now"
        // Insert with -1s to ensure they're before datetime('now', '-0 days')
        store.conn.execute(
            "UPDATE conversations SET created_at = datetime('now', '-1 second') WHERE curation_status = 'pending'",
            [],
        ).unwrap();

        let deleted = store.cleanup_expired(0).unwrap();
        assert_eq!(deleted, 1);

        let stats = store.stats().unwrap();
        assert_eq!(stats.total_turns, 1);  // approved turn survives
        assert_eq!(stats.approved, 1);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_database_is_encrypted() {
        let path = tmp_db("encrypted");
        let store = TrainingStore::open(&path, &test_key()).unwrap();
        store.insert_turn(&sample_turn()).unwrap();
        drop(store);

        // Read raw bytes and verify conversation text is NOT present as plaintext
        let raw = std::fs::read(&path).unwrap();
        let needle = b"hello, how are you?";
        let found = raw.windows(needle.len()).any(|w| w == needle);
        assert!(!found, "Plaintext conversation found in encrypted database file");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_cleanup_nonexistent_rotated_logs() {
        let path = PathBuf::from("/tmp/lumina_nonexistent_audit.jsonl");
        let removed = cleanup_old_rotated_logs(&path, 90);
        assert_eq!(removed, 0);
    }

    // ── P2-02: per-user isolation tests ────────────────────────────────────

    #[test]
    fn test_open_for_user_at_creates_namespaced_path() {
        let base = PathBuf::from("/tmp/lumina_p202_train_base");
        let _ = std::fs::remove_dir_all(&base);

        let store = TrainingStore::open_for_user_at(&base, "alice", &test_key()).unwrap();
        drop(store);

        let expected = base.join("users").join("alice").join("training.db");
        assert!(expected.exists(), "Per-user DB should exist at: {:?}", expected);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn test_different_users_have_separate_dbs() {
        let base = PathBuf::from("/tmp/lumina_p202_train_isolation");
        let _ = std::fs::remove_dir_all(&base);

        let store_alice = TrainingStore::open_for_user_at(&base, "alice", &test_key()).unwrap();
        store_alice.insert_turn(&sample_turn()).unwrap();
        drop(store_alice);

        let store_bob = TrainingStore::open_for_user_at(&base, "bob", &test_key()).unwrap();
        let stats_bob = store_bob.stats().unwrap();
        assert_eq!(stats_bob.total_turns, 0, "Bob should not see Alice's turns");
        drop(store_bob);

        let store_alice2 = TrainingStore::open_for_user_at(&base, "alice", &test_key()).unwrap();
        let stats_alice = store_alice2.stats().unwrap();
        assert_eq!(stats_alice.total_turns, 1, "Alice should see her own turn");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn test_system_user_is_valid() {
        let base = PathBuf::from("/tmp/lumina_p202_train_system");
        let _ = std::fs::remove_dir_all(&base);

        // "system" is the DB column default and a valid user_id
        let store = TrainingStore::open_for_user_at(&base, "system", &test_key()).unwrap();
        let id = store.insert_turn(&sample_turn()).unwrap();
        assert!(id > 0);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn test_invalid_user_id_rejected() {
        let base = PathBuf::from("/tmp/lumina_p202_train_invalid");
        let result = TrainingStore::open_for_user_at(&base, "../etc", &test_key());
        assert!(result.is_err(), "Path traversal user_id should be rejected");

        let result2 = TrainingStore::open_for_user_at(&base, "", &test_key());
        assert!(result2.is_err(), "Empty user_id should be rejected");
    }

    #[test]
    fn test_user_id_column_exists_after_migration() {
        // Verify user_id column exists by opening the store and doing an insert.
        // The schema migration sets up the column — if it failed, insert_turn would fail.
        let path = tmp_db("p202_migration_col");
        let store = TrainingStore::open(&path, &test_key()).unwrap();
        let id = store.insert_turn(&sample_turn()).unwrap();
        assert!(id > 0, "Insert should succeed with user_id column present");

        let _ = std::fs::remove_file(&path);
    }
}
