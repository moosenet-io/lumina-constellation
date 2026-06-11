//! EMEM-06: Contradiction detection and resolution for Reflexa.
//!
//! Finds pairs of Semantic memories with cosine similarity > 0.85 and asks the
//! LLM whether they contradict each other. Stale memories are superseded (not
//! deleted) — `superseded_by` is set to the newer memory's ID.
//!
//! All LLM failures are non-fatal: log a warning and skip the pair.

use crate::chord::ChordClient;
use crate::engram::types::{Memory, iso_now};
use crate::engram::{cosine, EngramStore};
use crate::error::Result;
use rusqlite::params;

/// Cosine similarity threshold above which a pair is a contradiction candidate.
pub const CONTRADICTION_SIMILARITY_THRESHOLD: f32 = 0.85;

/// Model alias for contradiction judgment — lumina-fast per inference de-bloat rules.
pub const CONTRADICTION_MODEL: &str = "lumina-fast";

/// Build the contradiction-check prompt for two memories.
pub fn contradiction_prompt(content_a: &str, content_b: &str) -> String {
    format!(
        "Memory A: {content_a}\n\
         Memory B: {content_b}\n\n\
         Do these two memories contradict each other? If yes, which is more likely to be the current/correct one?\n\
         Reply with: CONTRADICT:A (Memory A is stale) or CONTRADICT:B (Memory B is stale) or NO_CONTRADICTION"
    )
}

/// Parse the LLM response from contradiction_prompt.
///
/// Returns:
/// - `Some(0)` — Memory A (index 0) is stale
/// - `Some(1)` — Memory B (index 1) is stale
/// - `None`    — no contradiction detected (or unparseable response)
pub fn parse_contradiction_response(response: &str) -> Option<usize> {
    let trimmed = response.trim();
    if trimmed.contains("CONTRADICT:A") {
        Some(0) // A is stale
    } else if trimmed.contains("CONTRADICT:B") {
        Some(1) // B is stale
    } else {
        None // NO_CONTRADICTION or unparseable
    }
}

/// Supersede a memory by setting `superseded_by` to `newer_id` and updating `updated_at`.
///
/// Per-user safety: verifies `stale_id` belongs to `user_id` before updating.
/// This is a soft supersession — the row stays in the DB for audit trail.
pub fn supersede_memory(
    store: &EngramStore,
    user_id: &str,
    stale_id: &str,
    newer_id: &str,
) -> Result<()> {
    let now = iso_now();
    // Verify ownership before modifying — per-user isolation.
    let owner: Option<String> = store.conn.query_row(
        "SELECT user_id FROM memories_v2 WHERE id = ?1",
        params![stale_id],
        |r| r.get(0),
    ).ok();

    if owner.as_deref() != Some(user_id) {
        // Memory doesn't exist or belongs to a different user — skip safely.
        eprintln!(
            "REFLEXA: supersede_memory skipped — id={stale_id} owner={owner:?} caller={user_id}"
        );
        return Ok(());
    }

    store.conn.execute(
        "UPDATE memories_v2 SET superseded_by = ?1, updated_at = ?2 WHERE id = ?3 AND user_id = ?4",
        params![newer_id, now, stale_id, user_id],
    ).map_err(|e| crate::error::LuminaError::Internal(format!("supersede_memory failed: {e}")))?;

    Ok(())
}

/// Detect and resolve contradictions in a user's Semantic memories.
///
/// Algorithm:
/// 1. Collect Semantic memories with embeddings (already filtered to user_id by store).
/// 2. For each pair with cosine similarity > CONTRADICTION_SIMILARITY_THRESHOLD:
///    - Ask LLM whether they contradict.
///    - If yes, supersede the stale memory.
/// 3. Return the count of resolved contradictions.
///
/// LLM failures are non-fatal — the pair is skipped with a warning.
pub async fn detect_contradictions(
    store: &EngramStore,
    chord: &ChordClient,
    user_id: &str,
    semantic_memories: &[Memory],
) -> Result<usize> {
    // Only consider memories with embeddings.
    let with_emb: Vec<&Memory> = semantic_memories
        .iter()
        .filter(|m| !m.embedding.is_empty() && m.superseded_by.is_none())
        .collect();

    let mut resolved = 0usize;
    let mut superseded_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    for i in 0..with_emb.len() {
        if superseded_ids.contains(&with_emb[i].id) {
            continue;
        }
        for j in (i + 1)..with_emb.len() {
            if superseded_ids.contains(&with_emb[j].id) {
                continue;
            }

            let sim = cosine(&with_emb[i].embedding, &with_emb[j].embedding);
            if sim <= CONTRADICTION_SIMILARITY_THRESHOLD {
                continue;
            }

            // High similarity pair — ask LLM if they contradict.
            let prompt = contradiction_prompt(&with_emb[i].content, &with_emb[j].content);
            let response = match chord.chat(CONTRADICTION_MODEL, &prompt).await {
                Ok(r) => r,
                Err(e) => {
                    eprintln!(
                        "REFLEXA: contradiction_check LLM failed user={user_id} \
                         pair=({},{}) error={e} (non-fatal)",
                        with_emb[i].id, with_emb[j].id
                    );
                    continue;
                }
            };

            match parse_contradiction_response(response.as_str()) {
                Some(0) => {
                    // A (index i) is stale — supersede it with B (index j).
                    supersede_memory(store, user_id, &with_emb[i].id, &with_emb[j].id)?;
                    superseded_ids.insert(with_emb[i].id.clone());
                    resolved += 1;
                    eprintln!(
                        "REFLEXA: contradiction_resolved user={user_id} stale={} newer={}",
                        with_emb[i].id, with_emb[j].id
                    );
                    break; // i is superseded — stop inner loop for this i
                }
                Some(1) => {
                    // B (index j) is stale — supersede it with A (index i).
                    supersede_memory(store, user_id, &with_emb[j].id, &with_emb[i].id)?;
                    superseded_ids.insert(with_emb[j].id.clone());
                    resolved += 1;
                    eprintln!(
                        "REFLEXA: contradiction_resolved user={user_id} stale={} newer={}",
                        with_emb[j].id, with_emb[i].id
                    );
                }
                _ => {
                    // NO_CONTRADICTION or parse failure — skip.
                }
            }
        }
    }

    Ok(resolved)
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engram::types::{Memory, MemoryType, SensitivityCategory};
    use crate::engram::EngramStore;

    fn test_key() -> Vec<u8> { vec![0u8; 32] }

    fn tmp_db(tag: &str) -> std::path::PathBuf {
        let p = std::path::PathBuf::from(format!("/tmp/lumina_contradiction_test_{tag}.db"));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn make_semantic(user_id: &str, content: &str, embedding: Vec<f32>) -> Memory {
        let mut m = Memory::new(user_id, MemoryType::Semantic, SensitivityCategory::General, content);
        m.embedding = embedding;
        m
    }

    // ── test_contradiction_prompt_format ──────────────────────────────────────

    #[test]
    fn test_contradiction_prompt_format() {
        let prompt = contradiction_prompt("I drink coffee", "I never drink coffee");
        assert!(prompt.contains("Memory A: I drink coffee"));
        assert!(prompt.contains("Memory B: I never drink coffee"));
        assert!(prompt.contains("CONTRADICT:A"));
        assert!(prompt.contains("CONTRADICT:B"));
        assert!(prompt.contains("NO_CONTRADICTION"));
    }

    // ── test_parse_contradiction_response ─────────────────────────────────────

    #[test]
    fn test_parse_response_contradict_a() {
        assert_eq!(parse_contradiction_response("CONTRADICT:A"), Some(0));
        assert_eq!(parse_contradiction_response("  CONTRADICT:A (Memory A is stale)"), Some(0));
    }

    #[test]
    fn test_parse_response_contradict_b() {
        assert_eq!(parse_contradiction_response("CONTRADICT:B"), Some(1));
        assert_eq!(parse_contradiction_response("CONTRADICT:B (Memory B is stale)"), Some(1));
    }

    #[test]
    fn test_parse_response_no_contradiction() {
        assert_eq!(parse_contradiction_response("NO_CONTRADICTION"), None);
        assert_eq!(parse_contradiction_response("I can't tell"), None);
        assert_eq!(parse_contradiction_response(""), None);
    }

    // ── test_supersede_memory ─────────────────────────────────────────────────

    #[test]
    fn test_supersede_memory_sets_superseded_by() {
        let path = tmp_db("supersede");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        let mem_a = make_semantic("system", "I drink coffee daily", vec![1.0, 0.0]);
        let mem_b = make_semantic("system", "I stopped drinking coffee", vec![0.99, 0.01]);
        store.insert_memory(&mem_a).unwrap();
        store.insert_memory(&mem_b).unwrap();

        supersede_memory(&store, "system", &mem_a.id, &mem_b.id).unwrap();

        // Verify superseded_by is set on mem_a
        let superseded_by: Option<String> = store.conn.query_row(
            "SELECT superseded_by FROM memories_v2 WHERE id = ?1",
            params![mem_a.id],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(superseded_by.as_deref(), Some(mem_b.id.as_str()),
            "stale memory's superseded_by should point to the newer memory");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_supersede_memory_wrong_user_skipped() {
        let path = tmp_db("supersede_wrong_user");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        // Insert memory owned by "system"
        let mem = make_semantic("system", "some fact", vec![1.0, 0.0]);
        store.insert_memory(&mem).unwrap();

        // Try to supersede as a different user — should be a no-op, not an error
        supersede_memory(&store, "other-user", &mem.id, "fake-newer-id").unwrap();

        // mem should NOT have superseded_by set
        let superseded_by: Option<String> = store.conn.query_row(
            "SELECT superseded_by FROM memories_v2 WHERE id = ?1",
            params![mem.id],
            |r| r.get(0),
        ).unwrap();
        assert!(superseded_by.is_none(), "wrong-user supersession should be a no-op");

        let _ = std::fs::remove_file(&path);
    }

    // ── test_contradiction_detection_finds_opposing_memories ─────────────────

    #[test]
    fn test_contradiction_no_high_similarity_pair() {
        // Orthogonal embeddings — no pair should exceed the threshold.
        let mem_a = make_semantic("user1", "I like coffee", vec![1.0, 0.0, 0.0]);
        let mem_b = make_semantic("user1", "I like tea", vec![0.0, 1.0, 0.0]);

        // cosine between orthogonal vectors is 0 — below threshold
        let sim = cosine(&mem_a.embedding, &mem_b.embedding);
        assert!(sim <= CONTRADICTION_SIMILARITY_THRESHOLD,
            "orthogonal memories should not be contradiction candidates");
    }

    #[test]
    fn test_contradiction_high_similarity_detected() {
        // Two nearly-identical embedding vectors should exceed the threshold.
        let emb_a = vec![1.0f32, 0.0, 0.0];
        let emb_b = vec![0.999f32, 0.001, 0.0];
        // Normalise for cosine calculation
        let norm_b: f32 = (0.999f32 * 0.999 + 0.001 * 0.001).sqrt();
        let emb_b = vec![emb_b[0] / norm_b, emb_b[1] / norm_b, emb_b[2]];

        let sim = cosine(&emb_a, &emb_b);
        assert!(sim > CONTRADICTION_SIMILARITY_THRESHOLD,
            "near-identical embeddings should exceed contradiction threshold: {sim}");
    }
}
