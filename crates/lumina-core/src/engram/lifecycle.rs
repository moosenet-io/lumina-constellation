//! Engram memory lifecycle — pruning and consolidation.
//!
//! P2-14 + EMEM-01: operates on the `memories_v2` table.
//! - `migrate_lifecycle_columns_v2` — add columns if upgrading from pre-EMEM-01
//! - `record_access(conn, rowid)` — bump access_count and last_accessed
//! - `prune_older_than(days)` — remove stale memories
//! - `prune_least_accessed(keep)` — keep only the most-accessed memories
//! - `consolidate_similar(threshold)` — merge near-duplicate memories

use crate::error::{LuminaError, Result};
use crate::engram::hybrid_search::cosine_similarity;
use rusqlite::{params, Connection};

/// Keep the v1 migration function callable (safe no-op if facts table is gone).
pub fn migrate_lifecycle_columns(conn: &Connection) -> Result<()> {
    // Only relevant if the old facts table still exists (pre-EMEM-01 transition).
    let facts_exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='facts'",
        [], |r| r.get(0),
    ).unwrap_or(0);
    if facts_exists == 0 {
        return Ok(()); // Already on v2 schema
    }
    // Add columns to facts table if still in transition
    for (col, ddl) in [
        ("accessed_at", "ALTER TABLE facts ADD COLUMN accessed_at INTEGER;"),
        ("access_count", "ALTER TABLE facts ADD COLUMN access_count INTEGER NOT NULL DEFAULT 0;"),
    ] {
        let has: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM pragma_table_info('facts') WHERE name = '{col}'"),
            [], |r| r.get(0),
        ).unwrap_or(0);
        if has == 0 {
            conn.execute_batch(ddl)
                .map_err(|e| LuminaError::Internal(format!("lifecycle v1 migration {col}: {e}")))?;
        }
    }
    Ok(())
}

/// Ensure `memories_v2` has the lifecycle columns.
///
/// The memories_v2 schema defined in migration.rs already includes these columns,
/// but if an existing v2 table was created before them (shouldn't happen, but
/// defensive), this adds them safely.
pub fn migrate_lifecycle_columns_v2(conn: &Connection) -> Result<()> {
    // memories_v2 schema always has access_count and last_accessed — this is a
    // no-op for any DB created by EMEM-01. Only needed for DBs created by an
    // intermediate schema that predates this.
    for (col, ddl) in [
        ("access_count", "ALTER TABLE memories_v2 ADD COLUMN access_count INTEGER NOT NULL DEFAULT 0;"),
        ("last_accessed", "ALTER TABLE memories_v2 ADD COLUMN last_accessed TEXT;"),
    ] {
        let has: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM pragma_table_info('memories_v2') WHERE name = '{col}'"),
            [], |r| r.get(0),
        ).unwrap_or(1); // default 1 = assume present (don't add)
        if has == 0 {
            let _ = conn.execute_batch(ddl); // non-fatal
        }
    }
    Ok(())
}

/// Record an access — bumps access_count and sets last_accessed on memories_v2.
/// Uses rowid (numeric alias for the memories_v2 rowid) for compatibility.
pub fn record_access(conn: &Connection, fact_id: i64) -> Result<()> {
    let now = crate::engram::types::iso_now();
    conn.execute(
        "UPDATE memories_v2 SET access_count = access_count + 1, last_accessed = ?1 WHERE rowid = ?2",
        params![now, fact_id],
    ).map_err(|e| LuminaError::Internal(format!("record_access failed: {e}")))?;
    Ok(())
}

/// Delete all memories not accessed in the past `days` days.
/// Falls back to created_at if last_accessed is NULL.
/// Also removes deleted memories from the FTS index.
///
/// Returns the number of rows deleted.
pub fn prune_older_than(conn: &Connection, days: u64) -> Result<usize> {
    let cutoff = {
        let secs = i64::try_from(days).unwrap_or(i64::MAX).saturating_mul(86400);
        let now_secs = unix_secs_now();
        let cutoff_secs = now_secs.saturating_sub(secs);
        crate::engram::types::unix_secs_to_iso(cutoff_secs as u64)
    };
    // Collect rowids to clean from FTS before deleting
    let rowids: Vec<i64> = collect_rowids_where(
        conn,
        "SELECT rowid FROM memories_v2 WHERE COALESCE(last_accessed, created_at) < ?1",
        rusqlite::params![cutoff],
    )?;
    for rowid in &rowids {
        let _ = crate::engram::fts::fts_sync_delete_v2(conn, *rowid);
    }
    let n = conn.execute(
        "DELETE FROM memories_v2 WHERE COALESCE(last_accessed, created_at) < ?1",
        params![cutoff],
    ).map_err(|e| LuminaError::Internal(format!("prune_older_than failed: {e}")))?;
    Ok(n)
}

/// Keep only the `keep` most-accessed memories; delete the rest.
/// Also removes deleted memories from the FTS index.
///
/// Returns the number of rows deleted.
pub fn prune_least_accessed(conn: &Connection, keep: usize) -> Result<usize> {
    if keep == 0 {
        let rowids = collect_rowids_where(conn, "SELECT rowid FROM memories_v2", rusqlite::params![])?;
        for rowid in &rowids {
            let _ = crate::engram::fts::fts_sync_delete_v2(conn, *rowid);
        }
        let n = conn.execute("DELETE FROM memories_v2", [])
            .map_err(|e| LuminaError::Internal(format!("prune_least_accessed delete-all: {e}")))?;
        return Ok(n);
    }
    // Collect rowids to delete for FTS cleanup
    let rowids_to_delete = collect_rowids_where(
        conn,
        "SELECT rowid FROM memories_v2 WHERE id NOT IN (SELECT id FROM memories_v2 ORDER BY access_count DESC LIMIT ?1)",
        rusqlite::params![keep as i64],
    )?;
    for rowid in &rowids_to_delete {
        let _ = crate::engram::fts::fts_sync_delete_v2(conn, *rowid);
    }
    let n = conn.execute(
        "DELETE FROM memories_v2 WHERE id NOT IN (
            SELECT id FROM memories_v2 ORDER BY access_count DESC LIMIT ?1
        )",
        params![keep as i64],
    ).map_err(|e| LuminaError::Internal(format!("prune_least_accessed failed: {e}")))?;
    Ok(n)
}

/// Merge near-duplicate memories whose embeddings have cosine similarity ≥ `threshold`.
///
/// EMEM-01: operates on `memories_v2`. For each similar pair, the older memory's
/// content is appended to the newer one, then the older is deleted.
///
/// Returns the number of memories removed (one per merged pair).
pub fn consolidate_similar(conn: &Connection, threshold: f32) -> Result<usize> {
    let rows = {
        let mut stmt = conn.prepare(
            "SELECT id, content, embedding, created_at FROM memories_v2 WHERE embedding IS NOT NULL"
        ).map_err(|e| LuminaError::Internal(format!("consolidate prepare: {e}")))?;
        let rows: Vec<(String, String, Vec<u8>, String)> = stmt.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        }).map_err(|e| LuminaError::Internal(format!("consolidate query: {e}")))?
        .filter_map(|r| r.ok())
        .collect();
        rows
    };

    // Decode embeddings; skip malformed rows.
    let memories: Vec<(String, String, Vec<f32>, String)> = rows.into_iter()
        .filter_map(|(id, content, blob, created_at)| {
            crate::engram::decode_embedding(&blob).map(|emb| (id, content, emb, created_at))
        })
        .collect();

    let mut to_delete: Vec<(String, String)> = Vec::new(); // (older_id, newer_id)
    let mut consumed: std::collections::HashSet<String> = std::collections::HashSet::new();

    for i in 0..memories.len() {
        if consumed.contains(&memories[i].0) { continue; }
        for j in (i + 1)..memories.len() {
            if consumed.contains(&memories[j].0) { continue; }
            let sim = cosine_similarity(&memories[i].2, &memories[j].2);
            if sim >= threshold {
                let (older, newer) = if memories[i].3 <= memories[j].3 {
                    (memories[i].0.clone(), memories[j].0.clone())
                } else {
                    (memories[j].0.clone(), memories[i].0.clone())
                };
                to_delete.push((older.clone(), newer));
                consumed.insert(older);
                break;
            }
        }
    }

    if to_delete.is_empty() {
        return Ok(0);
    }

    let tx = conn.unchecked_transaction()
        .map_err(|e| LuminaError::Internal(format!("consolidate begin tx: {e}")))?;

    for (older_id, newer_id) in &to_delete {
        let older_content: String = tx.query_row(
            "SELECT content FROM memories_v2 WHERE id = ?1", params![older_id], |r| r.get(0)
        ).unwrap_or_default();

        if !older_content.is_empty() {
            tx.execute(
                "UPDATE memories_v2 SET content = content || '\n[merged: ' || ?1 || ']', updated_at = ?3 WHERE id = ?2",
                params![older_content, newer_id, crate::engram::types::iso_now()],
            ).map_err(|e| LuminaError::Internal(format!("consolidate update: {e}")))?;
        }

        // Collect rowid for FTS cleanup before deleting
        let rowid: Option<i64> = tx.query_row(
            "SELECT rowid FROM memories_v2 WHERE id = ?1", params![older_id], |r| r.get(0)
        ).ok();
        tx.execute("DELETE FROM memories_v2 WHERE id = ?1", params![older_id])
            .map_err(|e| LuminaError::Internal(format!("consolidate delete: {e}")))?;
        if let Some(rid) = rowid {
            let _ = crate::engram::fts::fts_sync_delete_v2(&tx, rid);
        }
    }

    tx.commit()
        .map_err(|e| LuminaError::Internal(format!("consolidate commit: {e}")))?;

    Ok(to_delete.len())
}

/// Build a scheduler `Routine` that runs memory consolidation + pruning nightly.
pub fn make_consolidation_routine() -> crate::scheduler::Routine {
    crate::scheduler::Routine {
        name: "memory_consolidation".to_string(),
        schedule: "0 3 * * *".to_string(),
        prompt: "Run memory consolidation: prune least-accessed memories and merge near-duplicates.".to_string(),
        model_override: None,
        channel: crate::scheduler::routine::RoutineChannel::Stdout,
        enabled: true,
        last_run: None,
        next_run: None,
        trigger: None,
    }
}

/// Collect rowids from a query. Avoids `?` + `MappedRows` lifetime issues by
/// collecting eagerly inside the helper so the Statement is dropped before return.
fn collect_rowids_where(conn: &Connection, sql: &str, params: impl rusqlite::Params) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(sql)
        .map_err(|e| LuminaError::Internal(format!("collect_rowids_where prepare: {e}")))?;
    let rows: Vec<i64> = stmt.query_map(params, |r| r.get(0))
        .map_err(|e| LuminaError::Internal(format!("collect_rowids_where query: {e}")))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

/// Current Unix timestamp in seconds.
fn unix_secs_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Open an in-memory DB with memories_v2 already set up.
    fn open_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::engram::migration::create_memories_v2(&conn).unwrap();
        conn
    }

    /// Insert a row into memories_v2 for testing lifecycle functions.
    fn insert_mem(conn: &Connection, content: &str, emb: &[f32], created_at: &str) -> i64 {
        let id = crate::engram::types::new_uuid();
        let blob = crate::engram::encode_embedding(emb);
        conn.execute(
            "INSERT INTO memories_v2 (id, user_id, memory_type, visibility, sensitivity, content, embedding, confidence, access_count, created_at, updated_at)
             VALUES (?1, 'system', 'semantic', 'private', 'general', ?2, ?3, 0.8, 0, ?4, ?4)",
            params![id, content, blob, created_at],
        ).unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn test_migrate_lifecycle_columns_v2_idempotent() {
        let conn = open_test_db();
        // Should be a no-op since memories_v2 already has the columns
        migrate_lifecycle_columns_v2(&conn).unwrap();
        migrate_lifecycle_columns_v2(&conn).unwrap();
        // Verify access_count column is present
        let has: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('memories_v2') WHERE name = 'access_count'",
            [], |r| r.get(0)
        ).unwrap();
        assert_eq!(has, 1);
    }

    #[test]
    fn test_migrate_lifecycle_columns_noop_no_facts() {
        let conn = open_test_db();
        // No facts table → should succeed silently
        migrate_lifecycle_columns(&conn).unwrap();
    }

    #[test]
    fn test_record_access_increments_count() {
        let conn = open_test_db();
        let rowid = insert_mem(&conn, "hello", &[1.0, 0.0], "2026-01-01T00:00:00Z");
        record_access(&conn, rowid).unwrap();
        record_access(&conn, rowid).unwrap();
        let count: i64 = conn.query_row(
            "SELECT access_count FROM memories_v2 WHERE rowid = ?1", params![rowid], |r| r.get(0)
        ).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_record_access_sets_last_accessed() {
        let conn = open_test_db();
        let rowid = insert_mem(&conn, "test", &[1.0, 0.0], "2026-01-01T00:00:00Z");
        record_access(&conn, rowid).unwrap();
        let last: Option<String> = conn.query_row(
            "SELECT last_accessed FROM memories_v2 WHERE rowid = ?1", params![rowid], |r| r.get(0)
        ).unwrap();
        assert!(last.is_some(), "last_accessed should be set after access");
    }

    #[test]
    fn test_prune_older_than_removes_stale() {
        let conn = open_test_db();
        insert_mem(&conn, "old", &[1.0, 0.0], "2020-01-01T00:00:00Z"); // very old
        insert_mem(&conn, "new", &[0.0, 1.0], "2026-06-01T00:00:00Z"); // recent
        let deleted = prune_older_than(&conn, 30).unwrap();
        assert_eq!(deleted, 1);
        let remaining: i64 = conn.query_row("SELECT COUNT(*) FROM memories_v2", [], |r| r.get(0)).unwrap();
        assert_eq!(remaining, 1);
    }

    #[test]
    fn test_prune_older_than_keeps_all_recent() {
        let conn = open_test_db();
        insert_mem(&conn, "a", &[1.0, 0.0], "2026-06-01T00:00:00Z");
        insert_mem(&conn, "b", &[0.0, 1.0], "2026-06-04T00:00:00Z");
        let deleted = prune_older_than(&conn, 30).unwrap();
        assert_eq!(deleted, 0);
    }

    #[test]
    fn test_prune_least_accessed_keeps_top_n() {
        let conn = open_test_db();
        let r1 = insert_mem(&conn, "popular", &[1.0, 0.0], "2026-01-01T00:00:00Z");
        let r2 = insert_mem(&conn, "medium",  &[0.5, 0.5], "2026-01-01T00:00:00Z");
        let r3 = insert_mem(&conn, "rare",    &[0.0, 1.0], "2026-01-01T00:00:00Z");
        for _ in 0..5 { record_access(&conn, r1).unwrap(); }
        for _ in 0..2 { record_access(&conn, r2).unwrap(); }
        let deleted = prune_least_accessed(&conn, 2).unwrap();
        assert_eq!(deleted, 1);
        // r3 (least-accessed) should be gone — check via rowid
        let remaining: i64 = conn.query_row("SELECT COUNT(*) FROM memories_v2", [], |r| r.get(0)).unwrap();
        assert_eq!(remaining, 2);
        let _ = r3; // suppress unused warning
    }

    #[test]
    fn test_consolidate_merges_similar_pair() {
        let conn = open_test_db();
        insert_mem(&conn, "fact A", &[1.0_f32, 0.0, 0.0], "2026-01-01T00:00:00Z"); // older
        insert_mem(&conn, "fact B", &[0.999, 0.001, 0.0], "2026-06-01T00:00:00Z"); // newer
        let merged = consolidate_similar(&conn, 0.99).unwrap();
        assert_eq!(merged, 1);
        let remaining: i64 = conn.query_row("SELECT COUNT(*) FROM memories_v2", [], |r| r.get(0)).unwrap();
        assert_eq!(remaining, 1);
    }

    #[test]
    fn test_consolidate_no_merge_orthogonal() {
        let conn = open_test_db();
        insert_mem(&conn, "A", &[1.0, 0.0], "2026-01-01T00:00:00Z");
        insert_mem(&conn, "B", &[0.0, 1.0], "2026-06-01T00:00:00Z");
        let merged = consolidate_similar(&conn, 0.9).unwrap();
        assert_eq!(merged, 0);
        let remaining: i64 = conn.query_row("SELECT COUNT(*) FROM memories_v2", [], |r| r.get(0)).unwrap();
        assert_eq!(remaining, 2);
    }

    #[test]
    fn test_consolidation_routine_schedule() {
        let r = make_consolidation_routine();
        assert_eq!(r.name, "memory_consolidation");
        assert_eq!(r.schedule, "0 3 * * *");
        assert!(r.enabled);
    }
}
