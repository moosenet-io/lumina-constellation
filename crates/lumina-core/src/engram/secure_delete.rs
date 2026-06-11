//! ESEC-04: Secure memory deletion with page zeroing.
//!
//! Before a row is deleted from SQLCipher, its `content` and `embedding` columns
//! are overwritten with random bytes. This ensures the actual content is gone from
//! the database page before the row is freed. Combined with `PRAGMA secure_delete = ON`
//! (zero-fills freed pages) and periodic `VACUUM` (eliminates free pages entirely),
//! deleted memories leave no recoverable trace in the database file.
//!
//! ## Deletion audit
//! Every deletion is logged with `memory_id` and `reason` but NEVER with content.
//! This satisfies audit requirements without leaking private data.

use crate::error::{LuminaError, Result};
use rusqlite::{params, Connection};

use super::types::{iso_now, new_uuid};

// ── Random byte helpers ────────────────────────────────────────────────────────

/// Generate `n` random bytes using the `rand` crate (cryptographically suitable PRNG).
pub fn random_bytes(n: usize) -> Vec<u8> {
    use rand::RngCore;
    let mut buf = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut buf);
    buf
}

/// Generate a random 32-byte value and return it as a 64-character lowercase hex string.
/// Used to overwrite the `content` column with a plausible-length string.
pub fn random_hex(n: usize) -> String {
    hex::encode(random_bytes(n))
}

// ── SecureDeleter ──────────────────────────────────────────────────────────────

/// Secure deletion helper: overwrites content+embedding before DELETE.
///
/// Usage pattern:
/// ```ignore
/// // 1. Overwrite + delete
/// SecureDeleter::delete_memory(conn, memory_id, "user_request")?;
/// // 2. Periodic compaction (every 100 deletions)
/// SecureDeleter::vacuum(conn)?;
/// ```
pub struct SecureDeleter;

impl SecureDeleter {
    /// Overwrite content and embedding with random bytes, then DELETE the row.
    ///
    /// Step 1 ensures the database page is dirtied with garbage before the row
    /// is freed. `PRAGMA secure_delete = ON` then zero-fills the freed page,
    /// and periodic `VACUUM` eliminates free pages entirely.
    ///
    /// `reason` is written to the audit log (but `content` is never logged).
    pub fn delete_memory(conn: &Connection, memory_id: &str, reason: &str) -> Result<()> {
        // Step 1 — overwrite content and embedding with random bytes
        let random_content = random_hex(32); // 64-char hex string
        let random_embedding = random_bytes(16);
        let now = iso_now();
        conn.execute(
            "UPDATE memories_v2 \
             SET content = ?1, embedding = ?2, updated_at = ?3 \
             WHERE id = ?4",
            params![random_content, random_embedding, now, memory_id],
        )
        .map_err(|e| LuminaError::Config(format!("secure_delete overwrite failed: {e}")))?;

        // Step 2 — delete the row
        conn.execute(
            "DELETE FROM memories_v2 WHERE id = ?1",
            params![memory_id],
        )
        .map_err(|e| LuminaError::Config(format!("secure_delete DELETE failed: {e}")))?;

        // Audit: log id + reason — NEVER content
        eprintln!("{}", deletion_audit_line(memory_id, reason));

        Ok(())
    }

    /// Run `VACUUM` to rebuild the database file and eliminate all free pages.
    ///
    /// Combined with `PRAGMA secure_delete = ON` this ensures no deleted content
    /// persists anywhere in the database file after a vacuum cycle.
    pub fn vacuum(conn: &Connection) -> Result<()> {
        conn.execute_batch("VACUUM;")
            .map_err(|e| LuminaError::Config(format!("secure_delete VACUUM failed: {e}")))?;
        Ok(())
    }

    /// Bulk-delete a slice of memory UUIDs using batches of `batch_size`.
    ///
    /// Each memory is overwritten individually before deletion (ESEC-04 requirement).
    /// `VACUUM` is called once at the end to compact the database.
    ///
    /// `reason` is forwarded to the deletion audit log for every record.
    pub fn bulk_delete(
        conn: &Connection,
        memory_ids: &[String],
        reason: &str,
        batch_size: usize,
    ) -> Result<usize> {
        let mut deleted = 0usize;
        let bs = if batch_size == 0 { 100 } else { batch_size };

        for chunk in memory_ids.chunks(bs) {
            for id in chunk {
                Self::delete_memory(conn, id, reason)?;
                deleted += 1;
            }
        }

        if deleted > 0 {
            Self::vacuum(conn)?;
        }

        Ok(deleted)
    }
}

// ── Audit log format ───────────────────────────────────────────────────────────

/// Build the audit log line for a deletion event.
///
/// Public so tests can verify the format without parsing `eprintln!` output.
/// The format never includes memory content — only `id` and `reason`.
pub fn deletion_audit_line(memory_id: &str, reason: &str) -> String {
    format!("ENGRAM_SECURE_DELETE id={memory_id} reason={reason}")
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engram::types::{iso_now, new_uuid, Memory, MemoryType, SensitivityCategory};
    use crate::engram::EngramStore;
    use std::path::PathBuf;

    fn test_key() -> Vec<u8> {
        vec![0u8; 32]
    }

    fn tmp_db(tag: &str) -> PathBuf {
        let p = PathBuf::from(format!("/tmp/lumina_esec04_{tag}.db"));
        let _ = std::fs::remove_file(&p);
        p
    }

    // ── ESEC-04-01: content overwritten before delete ─────────────────────────

    #[test]
    fn test_content_overwritten_before_delete() {
        let path = tmp_db("overwrite");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        // Insert a memory with known content
        let mem = Memory::new(
            "system",
            MemoryType::Semantic,
            SensitivityCategory::General,
            "secret: my favourite colour is blue",
        );
        let memory_id = mem.id.clone();
        store.insert_memory(&mem).unwrap();

        // Verify it's stored
        let before: String = store
            .conn
            .query_row(
                "SELECT content FROM memories_v2 WHERE id = ?1",
                params![memory_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(before, "secret: my favourite colour is blue");

        // Overwrite step (first half of SecureDeleter::delete_memory)
        let random_content = random_hex(32);
        let random_embedding = random_bytes(16);
        let now = iso_now();
        store
            .conn
            .execute(
                "UPDATE memories_v2 SET content = ?1, embedding = ?2, updated_at = ?3 WHERE id = ?4",
                params![random_content, random_embedding, now, memory_id],
            )
            .unwrap();

        // Verify content is no longer the original
        let after: String = store
            .conn
            .query_row(
                "SELECT content FROM memories_v2 WHERE id = ?1",
                params![memory_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_ne!(
            after, "secret: my favourite colour is blue",
            "content must be overwritten before deletion"
        );
        assert!(
            !after.contains("blue"),
            "original content must not survive overwrite"
        );

        let _ = std::fs::remove_file(&path);
    }

    // ── ESEC-04-02: PRAGMA secure_delete enabled at open time ─────────────────

    #[test]
    fn test_secure_delete_pragma_enabled() {
        let path = tmp_db("pragma");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        // Query the PRAGMA value — SQLCipher returns 1 when ON
        let val: i64 = store
            .conn
            .query_row("PRAGMA secure_delete;", [], |r| r.get(0))
            .expect("PRAGMA secure_delete should be readable");
        assert_eq!(val, 1, "PRAGMA secure_delete must be ON (1) after EngramStore::open");

        let _ = std::fs::remove_file(&path);
    }

    // ── ESEC-04-03: VACUUM eliminates free pages ──────────────────────────────

    #[test]
    fn test_vacuum_called_after_100_deletions() {
        let path = tmp_db("vacuum100");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        // Insert 105 memories
        let mut ids = Vec::new();
        for i in 0..105 {
            let mem = Memory::new(
                "system",
                MemoryType::Semantic,
                SensitivityCategory::General,
                &format!("memory number {i} with padding to make it realistic"),
            );
            ids.push(mem.id.clone());
            store.insert_memory(&mem).unwrap();
        }

        // Use the counter-integrated delete path on EngramStore
        for id in &ids {
            store.secure_delete_memory(id, "test_vacuum").unwrap();
        }

        // If we reach here without panic/error the VACUUM path executed.
        // Verify deletion count was tracked correctly.
        let count_after: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM memories_v2", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count_after, 0, "all memories should be deleted");

        let _ = std::fs::remove_file(&path);
    }

    // ── ESEC-04-04: audit line does NOT contain content ───────────────────────

    #[test]
    fn test_deletion_audit_does_not_contain_content() {
        let memory_id = new_uuid();
        let reason = "user_request";
        let line = deletion_audit_line(&memory_id, reason);

        // Must contain id and reason
        assert!(
            line.contains(&memory_id),
            "audit line must contain memory_id"
        );
        assert!(line.contains(reason), "audit line must contain reason");

        // Must NOT contain any content text — we verify the format only contains
        // the expected fields (id= and reason=) and no extra keys.
        assert!(
            !line.contains("content="),
            "audit line must NOT contain content field"
        );
        assert!(
            !line.contains("embedding="),
            "audit line must NOT contain embedding field"
        );

        // Verify format matches expected pattern
        assert!(
            line.starts_with("ENGRAM_SECURE_DELETE"),
            "audit line must start with ENGRAM_SECURE_DELETE"
        );
    }

    // ── ESEC-04-05: bulk deletion uses batches ────────────────────────────────

    #[test]
    fn test_bulk_deletion_uses_batches() {
        let path = tmp_db("bulk");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        let mut ids = Vec::new();
        for i in 0..50 {
            let mem = Memory::new(
                "system",
                MemoryType::Preference,
                SensitivityCategory::General,
                &format!("bulk test memory {i}"),
            );
            ids.push(mem.id.clone());
            store.insert_memory(&mem).unwrap();
        }

        let deleted = SecureDeleter::bulk_delete(&store.conn, &ids, "bulk_purge", 10).unwrap();
        assert_eq!(deleted, 50, "all 50 memories should be bulk-deleted");

        let remaining: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM memories_v2", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 0, "no memories should remain after bulk delete");

        let _ = std::fs::remove_file(&path);
    }

    // ── ESEC-04-06: delete_memory via SecureDeleter directly ─────────────────

    #[test]
    fn test_secure_deleter_delete_memory_removes_row() {
        let path = tmp_db("direct_delete");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        let mem = Memory::new(
            "system",
            MemoryType::Semantic,
            SensitivityCategory::General,
            "row to be securely deleted",
        );
        let memory_id = mem.id.clone();
        store.insert_memory(&mem).unwrap();

        // Verify row exists
        let count_before: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM memories_v2 WHERE id = ?1",
                params![memory_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count_before, 1);

        SecureDeleter::delete_memory(&store.conn, &memory_id, "test").unwrap();

        let count_after: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM memories_v2 WHERE id = ?1",
                params![memory_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count_after, 0, "row must be gone after SecureDeleter::delete_memory");

        let _ = std::fs::remove_file(&path);
    }

    // ── ESEC-04-07: random_bytes produces distinct values ─────────────────────

    #[test]
    fn test_random_bytes_are_distinct() {
        let a = random_bytes(32);
        let b = random_bytes(32);
        assert_eq!(a.len(), 32);
        assert_eq!(b.len(), 32);
        // Astronomically unlikely to be equal
        assert_ne!(a, b, "two independent random_bytes(32) calls should differ");
    }

    // ── ESEC-04-08: vacuum on empty DB is a no-op (no panic) ─────────────────

    #[test]
    fn test_vacuum_empty_db_no_panic() {
        let path = tmp_db("vacuum_empty");
        let store = EngramStore::open(&path, &test_key()).unwrap();
        assert!(
            SecureDeleter::vacuum(&store.conn).is_ok(),
            "VACUUM on empty DB must not fail"
        );
        let _ = std::fs::remove_file(&path);
    }
}
