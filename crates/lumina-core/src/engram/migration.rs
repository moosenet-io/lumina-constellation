//! EMEM-01: Schema migration from Engram v1 (facts table) to v2 (memories_v2).
//!
//! Migration is idempotent and runs on every EngramStore open:
//!   1. If `memories_v2` doesn't exist → create it (fresh install or first migration).
//!   2. If `facts` table exists → copy all rows to `memories_v2` as Semantic/Private/General,
//!      then rename `facts` to `facts_v1_backup`.
//!   3. If `memories_v2` already exists and `facts` doesn't → nothing to do.

use crate::error::{LuminaError, Result};
use rusqlite::Connection;
use super::types::{new_uuid, iso_now};

/// Create the `memories_v2` table and its indexes if they don't already exist.
pub fn create_memories_v2(conn: &Connection) -> Result<()> {
    conn.execute_batch("
        CREATE TABLE IF NOT EXISTS memories_v2 (
            id                     TEXT PRIMARY KEY,
            user_id                TEXT NOT NULL,
            memory_type            TEXT NOT NULL DEFAULT 'semantic',
            visibility             TEXT NOT NULL DEFAULT 'private',
            sensitivity            TEXT NOT NULL DEFAULT 'general',
            content                TEXT NOT NULL,
            embedding              BLOB,
            source_conversation_id TEXT,
            source_turn_index      INTEGER,
            confidence             REAL NOT NULL DEFAULT 0.8,
            access_count           INTEGER NOT NULL DEFAULT 0,
            last_accessed          TEXT,
            created_at             TEXT NOT NULL,
            updated_at             TEXT NOT NULL,
            superseded_by          TEXT,
            tags                   TEXT NOT NULL DEFAULT '[]'
        );
        CREATE INDEX IF NOT EXISTS idx_mem2_user       ON memories_v2(user_id);
        CREATE INDEX IF NOT EXISTS idx_mem2_type       ON memories_v2(user_id, memory_type);
        CREATE INDEX IF NOT EXISTS idx_mem2_visibility ON memories_v2(visibility);
        CREATE INDEX IF NOT EXISTS idx_mem2_sensitivity ON memories_v2(sensitivity);
        CREATE INDEX IF NOT EXISTS idx_mem2_created    ON memories_v2(user_id, created_at);
    ").map_err(|e| LuminaError::Config(format!("create memories_v2 failed: {e}")))?;
    Ok(())
}

/// Migrate rows from the v1 `facts` table to `memories_v2`.
///
/// Each v1 fact becomes a Semantic/Private/General memory. The `ts` column
/// (Unix seconds) is converted to an ISO 8601 string for `created_at` and
/// `updated_at`. After migration, `facts` is renamed to `facts_v1_backup`.
///
/// Safe to call multiple times (idempotent): if `facts` doesn't exist,
/// or is already renamed, this is a no-op.
pub fn migrate_v1_to_v2(conn: &Connection) -> Result<()> {
    // Check if v1 `facts` table still exists (not yet migrated)
    let facts_exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='facts'",
        [],
        |r| r.get(0),
    ).unwrap_or(0);

    if facts_exists == 0 {
        return Ok(()); // Already migrated or fresh install
    }

    // Count v1 rows to migrate
    let row_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM facts",
        [],
        |r| r.get(0),
    ).unwrap_or(0);

    if row_count > 0 {
        // Check if memories_v2 already has rows (double-migration guard)
        let v2_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memories_v2",
            [],
            |r| r.get(0),
        ).unwrap_or(0);

        if v2_count == 0 {
            // Migrate in batches of 500 for large stores
            let batch_size = 500i64;
            let mut offset = 0i64;
            let fallback_ts = iso_now();

            loop {
                let mut stmt = conn.prepare(
                    "SELECT id, user_id, text, embedding, ts FROM facts LIMIT ?1 OFFSET ?2",
                ).map_err(|e| LuminaError::Config(format!("migration prepare failed: {e}")))?;

                let rows: Vec<(i64, String, String, Option<Vec<u8>>, i64)> = stmt.query_map(
                    rusqlite::params![batch_size, offset],
                    |row| Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<Vec<u8>>>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                ).map_err(|e| LuminaError::Config(format!("migration query failed: {e}")))?
                .filter_map(|r| r.ok())
                .collect();

                if rows.is_empty() {
                    break;
                }

                let batch_len = rows.len() as i64;
                for (_, user_id, text, embedding, ts) in rows {
                    let id = new_uuid();
                    let created_at = if ts > 0 {
                        super::types::unix_secs_to_iso(ts as u64)
                    } else {
                        fallback_ts.clone()
                    };
                    conn.execute(
                        "INSERT OR IGNORE INTO memories_v2
                         (id, user_id, memory_type, visibility, sensitivity, content, embedding,
                          confidence, access_count, created_at, updated_at)
                         VALUES (?1, ?2, 'semantic', 'private', 'general', ?3, ?4, 0.8, 0, ?5, ?5)",
                        rusqlite::params![id, user_id, text, embedding, created_at],
                    ).map_err(|e| LuminaError::Config(format!("migration insert failed: {e}")))?;
                    // Sync to FTS index so migrated memories are full-text searchable.
                    let rowid = conn.last_insert_rowid();
                    if rowid > 0 {
                        let _ = super::fts::fts_sync_insert_v2(conn, rowid, &text);
                    }
                }

                offset += batch_len;
                if batch_len < batch_size {
                    break;
                }
            }

            eprintln!("engram: migrated {} v1 facts to memories_v2", row_count);
        }
    }

    // Rename `facts` to `facts_v1_backup` (preserve history, free the name)
    // Use ALTER TABLE ... RENAME — safe even if facts is empty.
    conn.execute_batch(
        "ALTER TABLE facts RENAME TO facts_v1_backup;"
    ).map_err(|e| LuminaError::Config(format!("migration rename facts failed: {e}")))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn open_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA journal_mode = WAL;").unwrap();
        conn
    }

    #[test]
    fn test_create_memories_v2_idempotent() {
        let conn = open_conn();
        // First call creates table
        create_memories_v2(&conn).unwrap();
        // Second call is a no-op (IF NOT EXISTS)
        create_memories_v2(&conn).unwrap();

        // Table exists
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='memories_v2'",
            [], |r| r.get(0)
        ).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_create_memories_v2_indexes() {
        let conn = open_conn();
        create_memories_v2(&conn).unwrap();

        let idx_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND tbl_name='memories_v2'",
            [], |r| r.get(0)
        ).unwrap();
        assert!(idx_count >= 5, "Expected at least 5 indexes, got {idx_count}");
    }

    #[test]
    fn test_migrate_no_v1_table_is_noop() {
        let conn = open_conn();
        create_memories_v2(&conn).unwrap();
        // No `facts` table → should succeed and do nothing
        migrate_v1_to_v2(&conn).unwrap();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM memories_v2", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_migrate_v1_to_v2_preserves_content() {
        let conn = open_conn();
        create_memories_v2(&conn).unwrap();

        // Create a v1 facts table with two rows
        conn.execute_batch("
            CREATE TABLE facts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                text TEXT NOT NULL,
                embedding BLOB NOT NULL,
                ts INTEGER NOT NULL DEFAULT 0,
                user_id TEXT NOT NULL DEFAULT 'system'
            );
        ").unwrap();

        // Insert test facts with embeddings
        let emb: Vec<u8> = vec![0u8; 8]; // 2 f32s
        conn.execute(
            "INSERT INTO facts (text, embedding, ts, user_id) VALUES (?1, ?2, 1000000, 'alice')",
            rusqlite::params!["alice's fact", emb],
        ).unwrap();
        conn.execute(
            "INSERT INTO facts (text, embedding, ts, user_id) VALUES (?1, ?2, 2000000, 'bob')",
            rusqlite::params!["bob's fact", emb],
        ).unwrap();

        migrate_v1_to_v2(&conn).unwrap();

        // Both facts migrated
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM memories_v2", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 2);

        // Content preserved
        let texts: Vec<String> = {
            let mut stmt = conn.prepare("SELECT content FROM memories_v2 ORDER BY content ASC").unwrap();
            stmt.query_map([], |r| r.get(0)).unwrap().filter_map(|r| r.ok()).collect()
        };
        assert!(texts.contains(&"alice's fact".to_string()));
        assert!(texts.contains(&"bob's fact".to_string()));

        // Migration defaults applied
        let row = conn.query_row(
            "SELECT memory_type, visibility, sensitivity, confidence FROM memories_v2 LIMIT 1",
            [], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, f64>(3)?))
        ).unwrap();
        assert_eq!(row.0, "semantic");
        assert_eq!(row.1, "private");
        assert_eq!(row.2, "general");
        assert!((row.3 - 0.8).abs() < 1e-6);

        // User IDs preserved
        let alice_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memories_v2 WHERE user_id = 'alice'",
            [], |r| r.get(0)
        ).unwrap();
        assert_eq!(alice_count, 1);

        // facts table renamed to backup
        let facts_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='facts'",
            [], |r| r.get(0)
        ).unwrap();
        assert_eq!(facts_count, 0, "facts table should be renamed after migration");

        let backup_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='facts_v1_backup'",
            [], |r| r.get(0)
        ).unwrap();
        assert_eq!(backup_count, 1, "facts_v1_backup should exist after migration");
    }

    #[test]
    fn test_migrate_idempotent_no_double_insert() {
        let conn = open_conn();
        create_memories_v2(&conn).unwrap();

        // Create and populate v1 table
        conn.execute_batch("CREATE TABLE facts (id INTEGER PRIMARY KEY, text TEXT NOT NULL, embedding BLOB NOT NULL, ts INTEGER DEFAULT 0, user_id TEXT DEFAULT 'system');").unwrap();
        conn.execute("INSERT INTO facts (text, embedding, ts) VALUES ('fact1', x'00000000', 0)", []).unwrap();

        migrate_v1_to_v2(&conn).unwrap();

        // Second call should be idempotent (facts table no longer exists)
        migrate_v1_to_v2(&conn).unwrap();

        let count: i64 = conn.query_row("SELECT COUNT(*) FROM memories_v2", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1, "should have exactly 1 row, not 2");
    }

    #[test]
    fn test_migrate_empty_v1_table() {
        let conn = open_conn();
        create_memories_v2(&conn).unwrap();

        // Empty v1 table
        conn.execute_batch("CREATE TABLE facts (id INTEGER PRIMARY KEY, text TEXT NOT NULL, embedding BLOB NOT NULL, ts INTEGER DEFAULT 0, user_id TEXT DEFAULT 'system');").unwrap();

        migrate_v1_to_v2(&conn).unwrap();

        // Backup table created, v2 is empty
        let v2_count: i64 = conn.query_row("SELECT COUNT(*) FROM memories_v2", [], |r| r.get(0)).unwrap();
        assert_eq!(v2_count, 0);
        let backup: i64 = conn.query_row("SELECT COUNT(*) FROM sqlite_master WHERE name='facts_v1_backup'", [], |r| r.get(0)).unwrap();
        assert_eq!(backup, 1);
    }

    #[test]
    fn test_migrate_timestamp_conversion() {
        let conn = open_conn();
        create_memories_v2(&conn).unwrap();

        conn.execute_batch("CREATE TABLE facts (id INTEGER PRIMARY KEY, text TEXT NOT NULL, embedding BLOB NOT NULL, ts INTEGER DEFAULT 0, user_id TEXT DEFAULT 'system');").unwrap();
        // ts = 1780272000 should correspond to a 2026 date
        conn.execute("INSERT INTO facts (text, embedding, ts, user_id) VALUES ('fact', x'00000000', 1780272000, 'user1')", []).unwrap();

        migrate_v1_to_v2(&conn).unwrap();

        let created_at: String = conn.query_row("SELECT created_at FROM memories_v2 LIMIT 1", [], |r| r.get(0)).unwrap();
        assert!(created_at.starts_with("2026-"), "expected 2026 date, got: {created_at}");
    }
}
