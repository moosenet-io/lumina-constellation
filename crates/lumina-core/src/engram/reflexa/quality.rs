//! EMEM-06: Quality assessment phase for Reflexa.
//!
//! Flags memories that are low-confidence, never accessed, and older than 30 days.
//! "Flagging" appends the tag "quality_review_needed" to the memory's tags field.
//! This does NOT delete anything — it marks entries for operator review.

use crate::engram::types::iso_now;
use crate::engram::EngramStore;
use crate::error::Result;
use rusqlite::params;

/// Confidence threshold below which a memory may be flagged.
pub const LOW_CONFIDENCE_THRESHOLD: f32 = 0.5;

/// Age in days beyond which an unaccessed, low-confidence memory is flagged.
pub const QUALITY_FLAG_AGE_DAYS: u64 = 30;

/// The tag appended to memories that need quality review.
pub const QUALITY_REVIEW_TAG: &str = "quality_review_needed";

/// Flag low-quality memories for a user by appending QUALITY_REVIEW_TAG to their tags.
///
/// Criteria (ALL must be true):
/// - confidence < LOW_CONFIDENCE_THRESHOLD (0.5)
/// - access_count == 0 (never retrieved)
/// - created_at older than QUALITY_FLAG_AGE_DAYS (30 days)
///
/// Per-user isolation: only processes memories belonging to `user_id`.
/// Does NOT delete — only updates the tags column.
///
/// Returns the count of memories flagged.
pub fn flag_low_quality(store: &EngramStore, user_id: &str) -> Result<usize> {
    // Compute cutoff date (30 days ago) without chrono.
    let cutoff = {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let cutoff_secs = secs.saturating_sub(QUALITY_FLAG_AGE_DAYS * 86400);
        crate::engram::types::unix_secs_to_iso(cutoff_secs)
    };

    // Fetch candidate memory IDs and current tags.
    let candidates: Vec<(String, String)> = {
        let mut stmt = store.conn.prepare(
            "SELECT id, COALESCE(tags, '[]') FROM memories_v2
             WHERE user_id = ?1
               AND confidence < ?2
               AND access_count = 0
               AND created_at < ?3
               AND superseded_by IS NULL"
        ).map_err(|e| crate::error::LuminaError::Internal(
            format!("quality flag prepare: {e}")
        ))?;

        let rows = stmt.query_map(
            params![user_id, LOW_CONFIDENCE_THRESHOLD, cutoff],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        ).map_err(|e| crate::error::LuminaError::Internal(
            format!("quality flag query: {e}")
        ))?;

        rows.filter_map(|r| r.ok()).collect()
    };

    if candidates.is_empty() {
        return Ok(0);
    }

    let now = iso_now();
    let mut flagged = 0usize;

    for (id, tags_json) in &candidates {
        // Parse current tags, skip if already flagged.
        let mut tags: Vec<String> = serde_json::from_str(tags_json).unwrap_or_default();
        if tags.iter().any(|t| t == QUALITY_REVIEW_TAG) {
            continue; // Already flagged — don't double-count.
        }

        tags.push(QUALITY_REVIEW_TAG.to_string());
        let new_tags_json = serde_json::to_string(&tags)
            .unwrap_or_else(|_| format!("[\"{QUALITY_REVIEW_TAG}\"]"));

        store.conn.execute(
            "UPDATE memories_v2 SET tags = ?1, updated_at = ?2 WHERE id = ?3 AND user_id = ?4",
            params![new_tags_json, now, id, user_id],
        ).map_err(|e| crate::error::LuminaError::Internal(
            format!("quality flag update: {e}")
        ))?;

        flagged += 1;
        eprintln!("REFLEXA: quality_flag user={user_id} memory_id={id}");
    }

    if flagged > 0 {
        eprintln!("REFLEXA: quality_assessment user={user_id} result={flagged}");
    }

    Ok(flagged)
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engram::types::{Memory, MemoryType, SensitivityCategory};
    use crate::engram::EngramStore;
    use rusqlite::params;

    fn test_key() -> Vec<u8> { vec![0u8; 32] }

    fn tmp_db(tag: &str) -> std::path::PathBuf {
        let p = std::path::PathBuf::from(format!("/tmp/lumina_quality_test_{tag}.db"));
        let _ = std::fs::remove_file(&p);
        p
    }

    /// Insert a memory with explicit confidence, access_count, and created_at.
    fn insert_with_meta(
        store: &EngramStore,
        user_id: &str,
        content: &str,
        confidence: f64,
        access_count: i64,
        created_at: &str,
    ) -> String {
        let id = crate::engram::types::new_uuid();
        store.conn.execute(
            "INSERT INTO memories_v2 (id, user_id, memory_type, visibility, sensitivity, content,
                confidence, access_count, created_at, updated_at)
             VALUES (?1, ?2, 'semantic', 'private', 'general', ?3, ?4, ?5, ?6, ?6)",
            params![id, user_id, content, confidence, access_count, created_at],
        ).unwrap();
        id
    }

    // ── test_quality_flag_marks_low_confidence_unaccessed ─────────────────────

    #[test]
    fn test_quality_flag_marks_low_confidence_unaccessed() {
        let path = tmp_db("flag_low_conf");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        // Qualifies: low confidence, never accessed, old
        let id = insert_with_meta(&store, "system", "old unaccessed fact", 0.3, 0, "2020-01-01T00:00:00Z");

        let flagged = flag_low_quality(&store, "system").unwrap();
        assert_eq!(flagged, 1, "should flag 1 qualifying memory");

        // Verify the tag was added
        let tags_json: String = store.conn.query_row(
            "SELECT COALESCE(tags, '[]') FROM memories_v2 WHERE id = ?1",
            params![id],
            |r| r.get(0),
        ).unwrap();
        assert!(tags_json.contains(QUALITY_REVIEW_TAG),
            "tags should contain quality_review_needed: {tags_json}");

        let _ = std::fs::remove_file(&path);
    }

    // ── test_quality_flag_skips_recent_memories ───────────────────────────────

    #[test]
    fn test_quality_flag_skips_recent_memories() {
        let path = tmp_db("flag_recent");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        // Low confidence, never accessed, but RECENT — should NOT be flagged
        let today = iso_now();
        insert_with_meta(&store, "system", "recent unaccessed fact", 0.3, 0, &today);

        let flagged = flag_low_quality(&store, "system").unwrap();
        assert_eq!(flagged, 0, "recent memories should not be flagged even if low confidence");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_quality_flag_skips_high_confidence() {
        let path = tmp_db("flag_high_conf");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        // High confidence — should NOT be flagged even if old and unaccessed
        insert_with_meta(&store, "system", "high confidence old fact", 0.9, 0, "2020-01-01T00:00:00Z");

        let flagged = flag_low_quality(&store, "system").unwrap();
        assert_eq!(flagged, 0, "high confidence memories should not be flagged");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_quality_flag_skips_accessed_memories() {
        let path = tmp_db("flag_accessed");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        // Low confidence, old, but HAS been accessed
        insert_with_meta(&store, "system", "accessed low conf fact", 0.3, 5, "2020-01-01T00:00:00Z");

        let flagged = flag_low_quality(&store, "system").unwrap();
        assert_eq!(flagged, 0, "accessed memories should not be flagged");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_quality_flag_not_double_flagged() {
        let path = tmp_db("flag_no_double");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        let id = insert_with_meta(&store, "system", "old low conf", 0.3, 0, "2020-01-01T00:00:00Z");

        // Flag once
        let first = flag_low_quality(&store, "system").unwrap();
        assert_eq!(first, 1);

        // Flag again — should not double-flag
        let second = flag_low_quality(&store, "system").unwrap();
        assert_eq!(second, 0, "already-flagged memories should not be counted again");

        // Verify only one tag in the array
        let tags_json: String = store.conn.query_row(
            "SELECT COALESCE(tags, '[]') FROM memories_v2 WHERE id = ?1",
            params![id],
            |r| r.get(0),
        ).unwrap();
        let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap();
        let flag_count = tags.iter().filter(|t| t.as_str() == QUALITY_REVIEW_TAG).count();
        assert_eq!(flag_count, 1, "quality_review_needed should appear exactly once");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_quality_flag_per_user_isolation() {
        let path = tmp_db("flag_isolation");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        // Insert qualifying memory for user A
        insert_with_meta(&store, "user-alice", "alice old fact", 0.3, 0, "2020-01-01T00:00:00Z");
        // Insert qualifying memory for user B
        insert_with_meta(&store, "user-bob", "bob old fact", 0.3, 0, "2020-01-01T00:00:00Z");

        // Flag only for user-alice
        let flagged = flag_low_quality(&store, "user-alice").unwrap();
        assert_eq!(flagged, 1, "should flag exactly alice's memory");

        // Bob's memory should NOT be flagged
        let bob_tags: String = store.conn.query_row(
            "SELECT COALESCE(tags, '[]') FROM memories_v2 WHERE user_id = 'user-bob'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert!(!bob_tags.contains(QUALITY_REVIEW_TAG),
            "bob's memory should not be flagged when only flagging for alice");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_quality_flag_memory_already_superseded_skipped() {
        let path = tmp_db("flag_superseded");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        // Insert a superseded memory (should be skipped)
        let id = crate::engram::types::new_uuid();
        store.conn.execute(
            "INSERT INTO memories_v2 (id, user_id, memory_type, visibility, sensitivity, content,
                confidence, access_count, created_at, updated_at, superseded_by)
             VALUES (?1, 'system', 'semantic', 'private', 'general', 'superseded fact',
                0.3, 0, '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z', 'some-newer-id')",
            params![id],
        ).unwrap();

        let flagged = flag_low_quality(&store, "system").unwrap();
        assert_eq!(flagged, 0, "superseded memories should not be flagged");

        let _ = std::fs::remove_file(&path);
    }
}
