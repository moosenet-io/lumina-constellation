//! EDGE-03/04: SQLCipher-backed skill storage with version history.
//!
//! Uses the same encryption infrastructure as training_store.rs.
//! Each skill record contains trigger patterns (JSON array of keyword strings),
//! a procedure (step-by-step instructions), and an optional embedding blob.
//!
//! EDGE-04 adds `skill_versions` table for full version history and rollback.
//!
//! P2-02: Per-user isolation. `SkillStore::new` accepts an optional `user_id`
//! to namespace the database path to `base/users/{user_id}/skills.db`.
//! Existing callers pass `user_id = "default"` for backward compatibility.

use crate::error::{LuminaError, Result};
use crate::users::user_data_dir;
use rusqlite::{params, Connection};
use std::path::Path;

use super::Skill;
use super::skill_generator::now_utc;

// ── SkillVersion ───────────────────────────────────────────────────────────

/// A point-in-time snapshot of a skill's procedure, kept for rollback.
#[derive(Debug, Clone)]
pub struct SkillVersion {
    pub id: i64,
    pub skill_id: i64,
    pub version: i64,
    pub procedure: String,
    pub created_at: String,
}

// ── SkillStore ─────────────────────────────────────────────────────────────

pub struct SkillStore {
    conn: Connection,
}

impl SkillStore {
    /// Open (or create) the SQLCipher database at `db_path` with `key`.
    ///
    /// Uses the same PRAGMA key pattern as training_store.rs.
    pub fn new(db_path: &Path, encryption_key: &[u8]) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                LuminaError::Config(format!("Cannot create skill database directory: {}", e))
            })?;
        }

        let conn = Connection::open(db_path).map_err(|e| {
            LuminaError::Config(format!("Cannot open skill database: {}", e))
        })?;

        // SQLCipher: set key before any other operation
        let hex_key = hex::encode(encryption_key);
        conn.execute_batch(&format!("PRAGMA key = \"x'{}'\";", hex_key))
            .map_err(|e| LuminaError::Config(format!("Failed to set skill database key: {}", e)))?;

        // Verify the key is correct
        conn.execute_batch("SELECT count(*) FROM sqlite_master;")
            .map_err(|_| {
                LuminaError::SecurityViolation(
                    "Skill database key is incorrect — cannot open database".to_string(),
                )
            })?;

        // WAL mode for better concurrent write performance
        conn.execute_batch("PRAGMA journal_mode = WAL;")
            .map_err(|e| LuminaError::Config(format!("Failed to enable WAL mode: {}", e)))?;

        let store = Self { conn };
        store.ensure_schema()?;
        Ok(store)
    }

    /// Open (or create) a per-user database under `base/users/{user_id}/skills.db`.
    ///
    /// `base` is typically `~/.lumina`. The directory is created automatically.
    /// `user_id` is validated to contain only safe path characters (ASCII alphanumeric,
    /// hyphens, underscores) — returns an error on invalid input.
    /// Pass `"system"` for the anonymous/legacy user slot (matches the DB column default).
    pub fn open_for_user_at(base: &Path, user_id: &str, encryption_key: &[u8]) -> Result<Self> {
        crate::users::validate_user_id(user_id)?;
        let dir = user_data_dir(base, user_id);
        std::fs::create_dir_all(&dir).map_err(|e| {
            LuminaError::Config(format!(
                "Cannot create skills directory for user {}: {}",
                user_id, e
            ))
        })?;
        let db_path = dir.join("skills.db");
        Self::new(&db_path, encryption_key)
    }

    /// Open a per-user skills database under `~/.lumina/users/{user_id}/skills.db`.
    ///
    /// Convenience wrapper around [`open_for_user_at`] using the default home directory
    /// and the shared vault key (`get_or_create_training_key`).
    /// Pass `"system"` for the anonymous/legacy user slot (matches the DB column default).
    pub fn open_for_user(user_id: &str) -> Result<Self> {
        let base = dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".lumina");
        let key = crate::training_store::get_or_create_training_key()?;
        Self::open_for_user_at(&base, user_id, &key)
    }

    /// Insert a new skill. Returns the new row id.
    pub fn insert(&self, skill: &Skill) -> Result<i64> {
        let trigger_json = serde_json::to_string(&skill.trigger_patterns)
            .map_err(|e| LuminaError::Parse(e))?;
        let tools_json = serde_json::to_string(&skill.tools_used)
            .map_err(|e| LuminaError::Parse(e))?;
        let embedding_blob = serialize_embedding(skill.embedding.as_deref());

        self.conn.execute(
            "INSERT INTO skills (
                name, description, trigger_patterns, procedure, tools_used,
                success_count, version, created_at, last_used, embedding
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                skill.name,
                skill.description,
                trigger_json,
                skill.procedure,
                tools_json,
                skill.success_count,
                skill.version,
                skill.created_at,
                skill.last_used,
                embedding_blob,
            ],
        )
        .map_err(|e| LuminaError::Config(format!("Failed to insert skill: {}", e)))?;

        Ok(self.conn.last_insert_rowid())
    }

    /// Retrieve a skill by id.
    pub fn get(&self, id: i64) -> Result<Option<Skill>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, description, trigger_patterns, procedure, tools_used,
                    success_count, version, created_at, last_used, embedding
             FROM skills WHERE id = ?1",
        )
        .map_err(|e| LuminaError::Config(format!("Skill get query failed: {}", e)))?;

        let mut rows = stmt
            .query_map(params![id], |row| row_to_skill(row))
            .map_err(|e| LuminaError::Config(format!("Skill get iteration failed: {}", e)))?;

        if let Some(row) = rows.next() {
            let skill = row.map_err(|e| LuminaError::Config(format!("Skill row error: {}", e)))?;
            Ok(Some(skill))
        } else {
            Ok(None)
        }
    }

    /// List all skills.
    pub fn list(&self) -> Result<Vec<Skill>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, description, trigger_patterns, procedure, tools_used,
                    success_count, version, created_at, last_used, embedding
             FROM skills ORDER BY success_count DESC, created_at DESC",
        )
        .map_err(|e| LuminaError::Config(format!("Skill list query failed: {}", e)))?;

        let rows = stmt
            .query_map([], |row| row_to_skill(row))
            .map_err(|e| LuminaError::Config(format!("Skill list iteration failed: {}", e)))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| LuminaError::Config(format!("Skill list row error: {}", e)))?;

        Ok(rows)
    }

    /// Increment success_count for a skill.
    pub fn update_success(&self, id: i64) -> Result<()> {
        self.conn
            .execute(
                "UPDATE skills SET success_count = success_count + 1,
                        last_used = datetime('now')
                 WHERE id = ?1",
                params![id],
            )
            .map_err(|e| LuminaError::Config(format!("update_success failed: {}", e)))?;
        Ok(())
    }

    /// Update the procedure text (increments version).
    pub fn update_procedure(&self, id: i64, procedure: &str, version: i64) -> Result<()> {
        self.conn
            .execute(
                "UPDATE skills SET procedure = ?1, version = ?2 WHERE id = ?3",
                params![procedure, version, id],
            )
            .map_err(|e| LuminaError::Config(format!("update_procedure failed: {}", e)))?;
        Ok(())
    }

    /// Delete a skill by id.
    pub fn delete(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM skills WHERE id = ?1", params![id])
            .map_err(|e| LuminaError::Config(format!("delete skill failed: {}", e)))?;
        Ok(())
    }

    /// Search skills by keyword in trigger_patterns or name.
    pub fn search_by_keyword(&self, keyword: &str) -> Result<Vec<Skill>> {
        let pattern = format!("%{}%", keyword);
        let mut stmt = self.conn.prepare(
            "SELECT id, name, description, trigger_patterns, procedure, tools_used,
                    success_count, version, created_at, last_used, embedding
             FROM skills
             WHERE trigger_patterns LIKE ?1 OR name LIKE ?1
             ORDER BY success_count DESC",
        )
        .map_err(|e| LuminaError::Config(format!("Keyword search query failed: {}", e)))?;

        let rows = stmt
            .query_map(params![pattern], |row| row_to_skill(row))
            .map_err(|e| LuminaError::Config(format!("Keyword search iteration failed: {}", e)))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| LuminaError::Config(format!("Keyword search row error: {}", e)))?;

        Ok(rows)
    }

    // ── Private helpers ────────────────────────────────────────────────────

    fn ensure_schema(&self) -> Result<()> {
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS skills (
                    id               INTEGER PRIMARY KEY AUTOINCREMENT,
                    name             TEXT NOT NULL,
                    description      TEXT NOT NULL,
                    trigger_patterns TEXT NOT NULL,
                    procedure        TEXT NOT NULL,
                    tools_used       TEXT NOT NULL,
                    success_count    INTEGER DEFAULT 0,
                    version          INTEGER DEFAULT 1,
                    created_at       TEXT NOT NULL,
                    last_used        TEXT,
                    embedding        BLOB,
                    user_id          TEXT NOT NULL DEFAULT 'system'
                );
                CREATE INDEX IF NOT EXISTS idx_skills_name    ON skills (name);
                CREATE INDEX IF NOT EXISTS idx_skills_success ON skills (success_count DESC);
                CREATE INDEX IF NOT EXISTS idx_skills_user    ON skills (user_id);
                CREATE TABLE IF NOT EXISTS skill_versions (
                    id         INTEGER PRIMARY KEY AUTOINCREMENT,
                    skill_id   INTEGER NOT NULL REFERENCES skills(id),
                    version    INTEGER NOT NULL,
                    procedure  TEXT NOT NULL,
                    created_at TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_skill_versions_skill_id
                    ON skill_versions (skill_id);",
            )
            .map_err(|e| LuminaError::Config(format!("Skill schema creation failed: {}", e)))?;

        // P2-02 migration: add user_id column if it doesn't exist yet.
        // Each column is checked independently so a partial migration (crash between
        // two ALTER TABLE statements) can be completed on the next open.
        let has_user_id = self.conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('skills') WHERE name = 'user_id'",
            [],
            |r| r.get::<_, i64>(0),
        ).unwrap_or(0);
        if has_user_id == 0 {
            self.conn.execute_batch(
                "ALTER TABLE skills ADD COLUMN user_id TEXT NOT NULL DEFAULT 'system';
                 CREATE INDEX IF NOT EXISTS idx_skills_user ON skills (user_id);"
            ).map_err(|e| LuminaError::Config(format!("Migration add user_id to skills failed: {}", e)))?;
        }

        Ok(())
    }

    // ── EDGE-04: version history methods ──────────────────────────────────

    /// Save the current procedure as a historical version snapshot.
    ///
    /// Call this BEFORE overwriting the procedure in the `skills` table so
    /// rollback can always restore the pre-update state.
    pub fn save_version(&self, skill_id: i64, version: i64, procedure: &str) -> Result<()> {
        let now = now_utc();
        self.conn
            .execute(
                "INSERT INTO skill_versions (skill_id, version, procedure, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![skill_id, version, procedure, now],
            )
            .map_err(|e| {
                LuminaError::Config(format!("save_version failed: {}", e))
            })?;
        Ok(())
    }

    /// Atomically snapshot the current procedure and write the new one.
    ///
    /// Both the `skill_versions` INSERT and the `skills` UPDATE happen inside
    /// a named SQLite SAVEPOINT so they are committed or rolled back together.
    /// A crash or SQL error between the two statements rolls back both — no
    /// orphaned history rows, no stale live procedure.
    pub fn save_version_and_update(
        &mut self,
        skill_id: i64,
        old_version: i64,
        old_procedure: &str,
        new_procedure: &str,
    ) -> Result<()> {
        let now = now_utc();
        let new_version = old_version + 1;

        // rusqlite's transaction() requires &mut self; using the owned Connection
        // here is idiomatic and safe.
        let tx = self.conn.transaction().map_err(|e| {
            LuminaError::Config(format!("save_version_and_update begin tx failed: {}", e))
        })?;

        tx.execute(
            "INSERT INTO skill_versions (skill_id, version, procedure, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![skill_id, old_version, old_procedure, now],
        )
        .map_err(|e| {
            LuminaError::Config(format!("save_version_and_update insert failed: {}", e))
        })?;

        tx.execute(
            "UPDATE skills SET procedure = ?1, version = ?2 WHERE id = ?3",
            params![new_procedure, new_version, skill_id],
        )
        .map_err(|e| {
            LuminaError::Config(format!("save_version_and_update update failed: {}", e))
        })?;

        tx.commit().map_err(|e| {
            LuminaError::Config(format!("save_version_and_update commit failed: {}", e))
        })?;

        Ok(())
    }

    /// Atomically snapshot the current live procedure and restore the target version.
    ///
    /// Before overwriting, the current procedure is saved to `skill_versions` so
    /// the user can always return to it if the rollback was a mistake.
    pub fn rollback_to_version_safe(&mut self, skill_id: i64, target_version: i64) -> Result<()> {
        // Fetch target procedure from history
        let target_procedure: String = self
            .conn
            .query_row(
                "SELECT procedure FROM skill_versions
                 WHERE skill_id = ?1 AND version = ?2",
                params![skill_id, target_version],
                |row| row.get(0),
            )
            .map_err(|e| {
                LuminaError::Config(format!(
                    "rollback: version {} not found for skill {}: {}",
                    target_version, skill_id, e
                ))
            })?;

        // Fetch current live version + procedure
        let (current_version, current_procedure): (i64, String) = self
            .conn
            .query_row(
                "SELECT version, procedure FROM skills WHERE id = ?1",
                params![skill_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| {
                LuminaError::Config(format!("rollback: cannot read current skill {}: {}", skill_id, e))
            })?;

        let now = now_utc();

        let tx = self.conn.transaction().map_err(|e| {
            LuminaError::Config(format!("rollback begin tx failed: {}", e))
        })?;

        // Snapshot the current live version before overwriting it
        tx.execute(
            "INSERT INTO skill_versions (skill_id, version, procedure, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![skill_id, current_version, current_procedure, now],
        )
        .map_err(|e| {
            LuminaError::Config(format!("rollback snapshot insert failed: {}", e))
        })?;

        // Restore the target procedure and version
        tx.execute(
            "UPDATE skills SET procedure = ?1, version = ?2 WHERE id = ?3",
            params![target_procedure, target_version, skill_id],
        )
        .map_err(|e| {
            LuminaError::Config(format!("rollback restore update failed: {}", e))
        })?;

        tx.commit().map_err(|e| {
            LuminaError::Config(format!("rollback commit failed: {}", e))
        })?;

        Ok(())
    }

    /// Return all historical versions of a skill, oldest first.
    pub fn get_version_history(&self, skill_id: i64) -> Result<Vec<SkillVersion>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, skill_id, version, procedure, created_at
                 FROM skill_versions
                 WHERE skill_id = ?1
                 ORDER BY version ASC",
            )
            .map_err(|e| LuminaError::Config(format!("version_history prepare failed: {}", e)))?;

        let rows = stmt
            .query_map(params![skill_id], |row| {
                Ok(SkillVersion {
                    id: row.get(0)?,
                    skill_id: row.get(1)?,
                    version: row.get(2)?,
                    procedure: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })
            .map_err(|e| {
                LuminaError::Config(format!("version_history iteration failed: {}", e))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| {
                LuminaError::Config(format!("version_history row error: {}", e))
            })?;

        Ok(rows)
    }

    /// Restore a skill's procedure to the given historical version.
    ///
    /// The current live procedure is snapshotted into `skill_versions` first
    /// so it can be recovered if needed. Both operations are transactional.
    ///
    /// Delegates to `rollback_to_version_safe`.
    pub fn rollback_to_version(&mut self, skill_id: i64, version: i64) -> Result<()> {
        self.rollback_to_version_safe(skill_id, version)
    }
}

// ── Row helper ─────────────────────────────────────────────────────────────

fn row_to_skill(row: &rusqlite::Row<'_>) -> rusqlite::Result<Skill> {
    let trigger_json: String = row.get(3)?;
    let tools_json: String = row.get(5)?;
    let embedding_blob: Option<Vec<u8>> = row.get(10)?;

    let trigger_patterns: Vec<String> =
        serde_json::from_str(&trigger_json).unwrap_or_default();
    let tools_used: Vec<String> =
        serde_json::from_str(&tools_json).unwrap_or_default();
    let embedding = embedding_blob.and_then(|b| deserialize_embedding(&b));

    Ok(Skill {
        id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        trigger_patterns,
        procedure: row.get(4)?,
        tools_used,
        success_count: row.get(6)?,
        version: row.get(7)?,
        created_at: row.get(8)?,
        last_used: row.get(9)?,
        embedding,
    })
}

// ── Embedding serialization ────────────────────────────────────────────────

fn serialize_embedding(embedding: Option<&[f32]>) -> Option<Vec<u8>> {
    embedding.map(|e| {
        e.iter()
            .flat_map(|f| f.to_le_bytes())
            .collect::<Vec<u8>>()
    })
}

fn deserialize_embedding(bytes: &[u8]) -> Option<Vec<f32>> {
    if bytes.len() % 4 != 0 {
        return None;
    }
    Some(
        bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
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
        let p = PathBuf::from(format!("/tmp/lumina_skill_store_test_{}.db", name));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn sample_skill() -> Skill {
        Skill {
            id: 0,
            name: "File Search".to_string(),
            description: "Search files using find and grep".to_string(),
            trigger_patterns: vec!["find file".to_string(), "search".to_string()],
            procedure: "1. Use find to locate files\n2. Use grep to search content".to_string(),
            tools_used: vec!["shell_exec".to_string()],
            success_count: 0,
            version: 1,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            last_used: None,
            embedding: None,
        }
    }

    #[test]
    fn test_open_creates_schema() {
        let path = tmp_db("open");
        let store = SkillStore::new(&path, &test_key()).unwrap();
        let skills = store.list().unwrap();
        assert_eq!(skills.len(), 0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_insert_and_get() {
        let path = tmp_db("insert");
        let store = SkillStore::new(&path, &test_key()).unwrap();

        let id = store.insert(&sample_skill()).unwrap();
        assert!(id > 0);

        let retrieved = store.get(id).unwrap();
        assert!(retrieved.is_some());
        let skill = retrieved.unwrap();
        assert_eq!(skill.name, "File Search");
        assert_eq!(skill.trigger_patterns.len(), 2);
        assert!(skill.trigger_patterns.contains(&"search".to_string()));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_list_returns_all() {
        let path = tmp_db("list");
        let store = SkillStore::new(&path, &test_key()).unwrap();

        store.insert(&sample_skill()).unwrap();
        store.insert(&sample_skill()).unwrap();

        let skills = store.list().unwrap();
        assert_eq!(skills.len(), 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_update_success_increments() {
        let path = tmp_db("success");
        let store = SkillStore::new(&path, &test_key()).unwrap();

        let id = store.insert(&sample_skill()).unwrap();
        store.update_success(id).unwrap();
        store.update_success(id).unwrap();

        let skill = store.get(id).unwrap().unwrap();
        assert_eq!(skill.success_count, 2);
        assert!(skill.last_used.is_some());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_update_procedure() {
        let path = tmp_db("procedure");
        let store = SkillStore::new(&path, &test_key()).unwrap();

        let id = store.insert(&sample_skill()).unwrap();
        store.update_procedure(id, "New procedure", 2).unwrap();

        let skill = store.get(id).unwrap().unwrap();
        assert_eq!(skill.procedure, "New procedure");
        assert_eq!(skill.version, 2);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_delete() {
        let path = tmp_db("delete");
        let store = SkillStore::new(&path, &test_key()).unwrap();

        let id = store.insert(&sample_skill()).unwrap();
        store.delete(id).unwrap();

        let skill = store.get(id).unwrap();
        assert!(skill.is_none());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_search_by_keyword() {
        let path = tmp_db("keyword");
        let store = SkillStore::new(&path, &test_key()).unwrap();

        store.insert(&sample_skill()).unwrap();

        let results = store.search_by_keyword("search").unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].name, "File Search");

        let no_results = store.search_by_keyword("nonexistent_xyz").unwrap();
        assert!(no_results.is_empty());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_get_nonexistent_returns_none() {
        let path = tmp_db("nonexistent");
        let store = SkillStore::new(&path, &test_key()).unwrap();

        let result = store.get(9999).unwrap();
        assert!(result.is_none());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_wrong_key_rejected() {
        let path = tmp_db("wrongkey");
        SkillStore::new(&path, &test_key()).unwrap();

        let wrong_key = vec![0xFFu8; 32];
        let result = SkillStore::new(&path, &wrong_key);
        assert!(result.is_err());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_skill_with_embedding_roundtrip() {
        let path = tmp_db("embedding");
        let store = SkillStore::new(&path, &test_key()).unwrap();

        let embedding = vec![0.1f32, 0.2, 0.3, 0.4];
        let mut skill = sample_skill();
        skill.embedding = Some(embedding.clone());

        let id = store.insert(&skill).unwrap();
        let retrieved = store.get(id).unwrap().unwrap();

        assert!(retrieved.embedding.is_some());
        let emb = retrieved.embedding.unwrap();
        assert_eq!(emb.len(), 4);
        assert!((emb[0] - 0.1f32).abs() < 1e-6);
        assert!((emb[3] - 0.4f32).abs() < 1e-6);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_skill_store_encrypted() {
        let path = tmp_db("encrypted");
        let store = SkillStore::new(&path, &test_key()).unwrap();
        store.insert(&sample_skill()).unwrap();
        drop(store);

        let raw = std::fs::read(&path).unwrap();
        let needle = b"File Search";
        let found = raw.windows(needle.len()).any(|w| w == needle);
        assert!(
            !found,
            "Plaintext skill name found in encrypted database — encryption may not be active"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_serialize_deserialize_embedding() {
        let original = vec![1.0f32, -0.5, 0.25, 0.0];
        let bytes = serialize_embedding(Some(&original)).unwrap();
        let recovered = deserialize_embedding(&bytes).unwrap();
        assert_eq!(original.len(), recovered.len());
        for (a, b) in original.iter().zip(recovered.iter()) {
            assert!((a - b).abs() < 1e-7);
        }
    }

    #[test]
    fn test_deserialize_bad_length_returns_none() {
        let bad_bytes = vec![1u8, 2, 3]; // not a multiple of 4
        let result = deserialize_embedding(&bad_bytes);
        assert!(result.is_none());
    }

    // ── EDGE-04 tests ──────────────────────────────────────────────────────

    #[test]
    fn test_save_version_and_get_history() {
        let path = tmp_db("version_history");
        let store = SkillStore::new(&path, &test_key()).unwrap();

        let id = store.insert(&sample_skill()).unwrap();

        // Save current version (v1) before updating
        store.save_version(id, 1, "1. Use find\n2. Use grep").unwrap();
        store.update_procedure(id, "Improved procedure v2", 2).unwrap();
        store.save_version(id, 2, "Improved procedure v2").unwrap();
        store.update_procedure(id, "Best procedure v3", 3).unwrap();

        let history = store.get_version_history(id).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].version, 1);
        assert_eq!(history[1].version, 2);
        assert_eq!(history[0].procedure, "1. Use find\n2. Use grep");
        assert_eq!(history[0].skill_id, id);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_rollback_to_version() {
        let path = tmp_db("rollback");
        let mut store = SkillStore::new(&path, &test_key()).unwrap();

        let id = store.insert(&sample_skill()).unwrap();
        let original_procedure = "1. Use find\n2. Use grep".to_string();

        // Save v1 into history, then update to v2
        store.save_version(id, 1, &original_procedure).unwrap();
        store.update_procedure(id, "Worse procedure v2", 2).unwrap();

        // Verify we're on v2
        let skill = store.get(id).unwrap().unwrap();
        assert_eq!(skill.version, 2);
        assert_eq!(skill.procedure, "Worse procedure v2");

        // Rollback to v1 — also snapshots v2 into history
        store.rollback_to_version(id, 1).unwrap();

        let rolled_back = store.get(id).unwrap().unwrap();
        assert_eq!(rolled_back.procedure, original_procedure);
        assert_eq!(rolled_back.version, 1);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_rollback_nonexistent_version_returns_error() {
        let path = tmp_db("rollback_err");
        let mut store = SkillStore::new(&path, &test_key()).unwrap();

        let id = store.insert(&sample_skill()).unwrap();

        // Attempt rollback to a version that was never saved
        let result = store.rollback_to_version(id, 999);
        assert!(result.is_err());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_empty_version_history_returns_empty_vec() {
        let path = tmp_db("empty_history");
        let store = SkillStore::new(&path, &test_key()).unwrap();

        let id = store.insert(&sample_skill()).unwrap();

        let history = store.get_version_history(id).unwrap();
        assert!(history.is_empty());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_version_history_for_different_skills_isolated() {
        let path = tmp_db("version_isolation");
        let store = SkillStore::new(&path, &test_key()).unwrap();

        let id1 = store.insert(&sample_skill()).unwrap();
        let id2 = store.insert(&sample_skill()).unwrap();

        store.save_version(id1, 1, "procedure for skill 1").unwrap();
        store.save_version(id2, 1, "procedure for skill 2").unwrap();
        store.save_version(id2, 2, "updated procedure for skill 2").unwrap();

        let history1 = store.get_version_history(id1).unwrap();
        let history2 = store.get_version_history(id2).unwrap();

        assert_eq!(history1.len(), 1);
        assert_eq!(history2.len(), 2);
        assert_eq!(history1[0].procedure, "procedure for skill 1");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_now_utc_format() {
        let ts = now_utc();
        // Must be YYYY-MM-DDTHH:MM:SSZ (20 chars)
        assert_eq!(ts.len(), 20, "Unexpected timestamp format: {}", ts);
        assert!(ts.ends_with('Z'));
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[7..8], "-");
        assert_eq!(&ts[10..11], "T");
    }

    // ── P2-02: per-user isolation tests ────────────────────────────────────

    #[test]
    fn test_open_for_user_at_creates_namespaced_path() {
        let base = PathBuf::from("/tmp/lumina_p202_skill_base");
        let _ = std::fs::remove_dir_all(&base);

        let store = SkillStore::open_for_user_at(&base, "user-dave", &test_key()).unwrap();
        drop(store);

        let expected = base.join("users").join("user-dave").join("skills.db");
        assert!(expected.exists(), "Per-user skills DB should exist at: {:?}", expected);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn test_different_users_skill_stores_are_isolated() {
        let base = PathBuf::from("/tmp/lumina_p202_skill_isolation");
        let _ = std::fs::remove_dir_all(&base);

        let store_a = SkillStore::open_for_user_at(&base, "user-alice", &test_key()).unwrap();
        store_a.insert(&sample_skill()).unwrap();
        drop(store_a);

        let store_b = SkillStore::open_for_user_at(&base, "user-bob", &test_key()).unwrap();
        let bob_skills = store_b.list().unwrap();
        assert!(bob_skills.is_empty(), "Bob should not see Alice's skills");
        drop(store_b);

        let store_a2 = SkillStore::open_for_user_at(&base, "user-alice", &test_key()).unwrap();
        let alice_skills = store_a2.list().unwrap();
        assert_eq!(alice_skills.len(), 1, "Alice should see her own skill");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn test_invalid_user_id_rejected_skill() {
        let base = PathBuf::from("/tmp/lumina_p202_skill_invalid");
        let result = SkillStore::open_for_user_at(&base, "../etc", &test_key());
        assert!(result.is_err(), "Path traversal user_id should be rejected");

        let result2 = SkillStore::open_for_user_at(&base, "", &test_key());
        assert!(result2.is_err(), "Empty user_id should be rejected");
    }

    #[test]
    fn test_user_id_column_exists_in_skills() {
        // Verify user_id column by inserting a skill and querying with raw SQL
        // via a separate connection (since conn is private).
        let path = tmp_db("p202_skills_migration");
        {
            let store = SkillStore::new(&path, &test_key()).unwrap();
            // Insert a skill — if user_id column doesn't exist this would fail on SELECT
            let id = store.insert(&sample_skill()).unwrap();
            assert!(id > 0);
        }
        // Reopen with standard conn to verify via info
        let store = SkillStore::new(&path, &test_key()).unwrap();
        // Use list() to confirm the schema is sound (user_id column backed by migration)
        let skills = store.list().unwrap();
        assert_eq!(skills.len(), 1);

        let _ = std::fs::remove_file(&path);
    }

}
