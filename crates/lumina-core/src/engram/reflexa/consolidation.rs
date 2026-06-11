//! EMEM-06: Consolidation phase for Reflexa.
//!
//! Merges near-duplicate Semantic memories using cosine similarity at threshold 0.80.
//! Uses a user-scoped query so Reflexa NEVER touches another user's memories.
//!
//! Note: lifecycle::consolidate_similar() has no user_id filter and cannot be
//! called safely from a per-user reflection context. This module reimplements
//! the merge logic with a mandatory user_id scope.

use crate::engram::hybrid_search::cosine_similarity;
use crate::engram::types::iso_now;
use crate::engram::{fts, EngramStore};
use crate::error::{LuminaError, Result};
use rusqlite::params;

/// Similarity threshold for consolidating Semantic memories.
pub const CONSOLIDATION_THRESHOLD: f32 = 0.80;

/// Consolidate similar Semantic memories for a specific user.
///
/// Fetches ONLY the calling user's memories (per-user isolation). For each pair
/// with cosine similarity >= CONSOLIDATION_THRESHOLD, the older memory's content
/// is appended to the newer one and the older row is deleted.
///
/// Returns the count of memories removed via consolidation.
pub fn merge_related_memories(store: &EngramStore, user_id: &str) -> Result<usize> {
    // Fetch only this user's memories with embeddings — per-user isolation enforced in SQL.
    let rows: Vec<(String, String, Vec<u8>, String)> = {
        let mut stmt = store.conn.prepare(
            "SELECT id, content, embedding, created_at
             FROM memories_v2
             WHERE user_id = ?1 AND embedding IS NOT NULL AND superseded_by IS NULL"
        ).map_err(|e| LuminaError::Internal(format!("consolidation prepare: {e}")))?;

        let rows = stmt.query_map(params![user_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        }).map_err(|e| LuminaError::Internal(format!("consolidation query: {e}")))?;

        rows.filter_map(|r| r.ok()).collect()
    };

    // Decode embeddings; skip malformed rows.
    // ESEC-03: use store.decrypt_embedding_blob so encrypted blobs are handled correctly.
    let memories: Vec<(String, String, Vec<f32>, String)> = rows.into_iter()
        .filter_map(|(id, content, blob, created_at)| {
            store.decrypt_embedding_blob(&blob).map(|emb| (id, content, emb, created_at))
        })
        .collect();

    if memories.len() < 2 {
        return Ok(0);
    }

    // Find pairs above threshold — greedy: each memory can only be consumed once.
    let mut to_merge: Vec<(String, String)> = Vec::new(); // (older_id, newer_id)
    let mut consumed: std::collections::HashSet<String> = std::collections::HashSet::new();

    for i in 0..memories.len() {
        if consumed.contains(&memories[i].0) { continue; }
        for j in (i + 1)..memories.len() {
            if consumed.contains(&memories[j].0) { continue; }
            let sim = cosine_similarity(&memories[i].2, &memories[j].2);
            if sim >= CONSOLIDATION_THRESHOLD {
                let (older, newer) = if memories[i].3 <= memories[j].3 {
                    (memories[i].0.clone(), memories[j].0.clone())
                } else {
                    (memories[j].0.clone(), memories[i].0.clone())
                };
                to_merge.push((older.clone(), newer));
                consumed.insert(older);
                break;
            }
        }
    }

    if to_merge.is_empty() {
        return Ok(0);
    }

    let now = iso_now();
    let tx = store.conn.unchecked_transaction()
        .map_err(|e| LuminaError::Internal(format!("consolidation begin tx: {e}")))?;

    for (older_id, newer_id) in &to_merge {
        // Double-check ownership before modifying — defence-in-depth.
        let owner: Option<String> = tx.query_row(
            "SELECT user_id FROM memories_v2 WHERE id = ?1",
            params![older_id],
            |r| r.get(0),
        ).ok();

        if owner.as_deref() != Some(user_id) {
            // Safety: skip any row that doesn't belong to this user.
            eprintln!(
                "REFLEXA: consolidation_skip_wrong_owner older={older_id} \
                 owner={owner:?} caller={user_id}"
            );
            continue;
        }

        // Append older content to newer before deleting.
        let older_content: String = tx.query_row(
            "SELECT content FROM memories_v2 WHERE id = ?1",
            params![older_id],
            |r| r.get(0),
        ).unwrap_or_default();

        if !older_content.is_empty() {
            tx.execute(
                "UPDATE memories_v2 SET content = content || '\n[merged: ' || ?1 || ']', \
                 updated_at = ?3 WHERE id = ?2 AND user_id = ?4",
                params![older_content, newer_id, now, user_id],
            ).map_err(|e| LuminaError::Internal(format!("consolidation update: {e}")))?;
        }

        // Remove FTS entry and delete the older row — scoped to this user.
        let rowid: Option<i64> = tx.query_row(
            "SELECT rowid FROM memories_v2 WHERE id = ?1 AND user_id = ?2",
            params![older_id, user_id],
            |r| r.get(0),
        ).ok();

        tx.execute(
            "DELETE FROM memories_v2 WHERE id = ?1 AND user_id = ?2",
            params![older_id, user_id],
        ).map_err(|e| LuminaError::Internal(format!("consolidation delete: {e}")))?;

        if let Some(rid) = rowid {
            let _ = fts::fts_sync_delete_v2(&tx, rid);
        }

        eprintln!("REFLEXA: consolidation_merged user={user_id} older={older_id} newer={newer_id}");
    }

    tx.commit()
        .map_err(|e| LuminaError::Internal(format!("consolidation commit: {e}")))?;

    if !to_merge.is_empty() {
        eprintln!("REFLEXA: consolidation user={user_id} result={}", to_merge.len());
    }

    Ok(to_merge.len())
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
        let p = std::path::PathBuf::from(format!("/tmp/lumina_consolidation_test_{tag}.db"));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn make_semantic_at(user_id: &str, content: &str, embedding: Vec<f32>, created_at: &str) -> Memory {
        let mut m = Memory::new(user_id, MemoryType::Semantic, SensitivityCategory::General, content);
        m.embedding = embedding;
        m.created_at = created_at.to_string();
        m.updated_at = created_at.to_string();
        m
    }

    // ── test_consolidation_merges_related_memories ────────────────────────────

    #[test]
    fn test_consolidation_merges_similar_pair() {
        let path = tmp_db("merge_similar");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        // Two near-identical embeddings — should be above CONSOLIDATION_THRESHOLD (0.80)
        let mem_a = make_semantic_at("system", "prefers dark roast", vec![1.0f32, 0.0, 0.0], "2026-01-01T00:00:00Z");
        let mem_b = make_semantic_at("system", "likes dark coffee", vec![0.99f32, 0.001, 0.0], "2026-06-01T00:00:00Z");
        store.insert_memory(&mem_a).unwrap();
        store.insert_memory(&mem_b).unwrap();

        let count = merge_related_memories(&store, "system").unwrap();
        assert!(count >= 1, "should consolidate at least 1 memory pair");

        let remaining: i64 = store.conn.query_row(
            "SELECT COUNT(*) FROM memories_v2", [], |r| r.get(0)
        ).unwrap();
        assert_eq!(remaining, 1, "one of the duplicate pair should be removed");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_consolidation_no_merge_orthogonal() {
        let path = tmp_db("merge_orthogonal");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        // Orthogonal embeddings — cosine = 0, well below threshold
        let mem_a = make_semantic_at("system", "likes running", vec![1.0f32, 0.0], "2026-01-01T00:00:00Z");
        let mem_b = make_semantic_at("system", "likes swimming", vec![0.0f32, 1.0], "2026-06-01T00:00:00Z");
        store.insert_memory(&mem_a).unwrap();
        store.insert_memory(&mem_b).unwrap();

        let count = merge_related_memories(&store, "system").unwrap();
        assert_eq!(count, 0, "orthogonal memories should not be consolidated");

        let remaining: i64 = store.conn.query_row(
            "SELECT COUNT(*) FROM memories_v2", [], |r| r.get(0)
        ).unwrap();
        assert_eq!(remaining, 2, "both orthogonal memories should remain");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_consolidation_empty_store_returns_zero() {
        let path = tmp_db("merge_empty");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        let count = merge_related_memories(&store, "system").unwrap();
        assert_eq!(count, 0, "empty store should return 0 consolidated");

        let _ = std::fs::remove_file(&path);
    }

    // ── test_consolidation_per_user_isolation ─────────────────────────────────

    #[test]
    fn test_consolidation_does_not_merge_other_users_memories() {
        let path = tmp_db("merge_isolation");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        // Alice: two near-identical memories
        let alice_a = make_semantic_at("user-alice", "alice coffee pref", vec![1.0f32, 0.0], "2026-01-01T00:00:00Z");
        let alice_b = make_semantic_at("user-alice", "alice dark roast pref", vec![0.9999f32, 0.0001], "2026-06-01T00:00:00Z");

        // Bob: two near-identical memories (same embeddings as Alice)
        let bob_a = make_semantic_at("user-bob", "bob coffee pref", vec![1.0f32, 0.0], "2026-01-01T00:00:00Z");
        let bob_b = make_semantic_at("user-bob", "bob dark roast pref", vec![0.9999f32, 0.0001], "2026-06-01T00:00:00Z");

        store.insert_memory(&alice_a).unwrap();
        store.insert_memory(&alice_b).unwrap();
        store.insert_memory(&bob_a).unwrap();
        store.insert_memory(&bob_b).unwrap();

        // Run consolidation for Alice only
        let alice_merged = merge_related_memories(&store, "user-alice").unwrap();
        assert!(alice_merged >= 1, "alice's memories should be consolidated");

        // Bob's memories must be untouched — still 2 rows for user-bob
        let bob_count: i64 = store.conn.query_row(
            "SELECT COUNT(*) FROM memories_v2 WHERE user_id = 'user-bob'",
            [], |r| r.get(0)
        ).unwrap();
        assert_eq!(bob_count, 2, "bob's memories must not be merged by alice's consolidation pass");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_consolidation_cross_user_boundary_not_merged() {
        let path = tmp_db("merge_cross_user");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        // Alice has one memory, Bob has one nearly identical memory.
        // Running consolidation for Alice should NOT remove Bob's memory.
        let mut alice_mem = Memory::new("user-alice", MemoryType::Semantic, SensitivityCategory::General, "alice fact");
        alice_mem.embedding = vec![1.0f32, 0.0];
        alice_mem.created_at = "2026-01-01T00:00:00Z".to_string();
        alice_mem.updated_at = "2026-01-01T00:00:00Z".to_string();

        let mut bob_mem = Memory::new("user-bob", MemoryType::Semantic, SensitivityCategory::General, "bob similar fact");
        bob_mem.embedding = vec![0.9999f32, 0.0001];
        let norm: f32 = (bob_mem.embedding[0].powi(2) + bob_mem.embedding[1].powi(2)).sqrt();
        bob_mem.embedding = vec![bob_mem.embedding[0] / norm, bob_mem.embedding[1] / norm];
        bob_mem.created_at = "2026-06-01T00:00:00Z".to_string();
        bob_mem.updated_at = "2026-06-01T00:00:00Z".to_string();

        store.insert_memory(&alice_mem).unwrap();
        store.insert_memory(&bob_mem).unwrap();

        // Consolidate for Alice — there's only ONE alice memory, so nothing should be merged
        let merged = merge_related_memories(&store, "user-alice").unwrap();
        assert_eq!(merged, 0, "consolidation should not merge across user boundaries");

        // Both rows must still exist
        let total: i64 = store.conn.query_row(
            "SELECT COUNT(*) FROM memories_v2", [], |r| r.get(0)
        ).unwrap();
        assert_eq!(total, 2, "both alice and bob memories must remain after scoped consolidation");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_consolidation_ownership_check_blocks_wrong_user_delete() {
        let path = tmp_db("merge_ownership");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        // Insert Bob's memory with a known ID
        let bob_id = crate::engram::types::new_uuid();
        store.conn.execute(
            "INSERT INTO memories_v2 (id, user_id, memory_type, visibility, sensitivity,
                content, embedding, confidence, access_count, created_at, updated_at)
             VALUES (?1, 'user-bob', 'semantic', 'private', 'general', 'bob private fact',
                 ?2, 0.8, 0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            params![bob_id, crate::engram::encode_embedding(&[1.0f32, 0.0])],
        ).unwrap();

        // Run consolidation for Alice — should not touch Bob's row
        let merged = merge_related_memories(&store, "user-alice").unwrap();
        assert_eq!(merged, 0);

        // Bob's memory still exists
        let bob_count: i64 = store.conn.query_row(
            "SELECT COUNT(*) FROM memories_v2 WHERE user_id = 'user-bob'",
            [], |r| r.get(0)
        ).unwrap();
        assert_eq!(bob_count, 1, "bob's memory must not be deleted by alice's consolidation");

        let _ = std::fs::remove_file(&path);
    }
}
