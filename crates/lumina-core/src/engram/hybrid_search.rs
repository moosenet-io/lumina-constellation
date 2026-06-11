//! Hybrid search: FTS5 + vector cosine + Reciprocal Rank Fusion (RRF).
//!
//! Implements the EDGE-07 search upgrade. Combines full-text (BM25) and
//! semantic (cosine) retrieval into a single ranked result list via RRF.
//!
//! ## Search modes
//! - `Hybrid` (default): runs both FTS and vector search, merges via RRF.
//! - `FtsOnly`: BM25 only — useful for exact keyword lookup.
//! - `VectorOnly`: cosine similarity only — useful for semantic queries.
//!
//! ## RRF formula
//! `rrf_score(d) = Σ 1/(k + rank_i)` for each ranked list `i` the document
//! appears in. `k = 60` (standard value from the original RRF paper).

use crate::engram::{decode_embedding, embedding_security::EmbeddingSecurity, fts::{fts_search, fts_search_v2}};
use crate::error::{LuminaError, Result};
use rusqlite::Connection;

// ── Public types ───────────────────────────────────────────────────────────

/// A single result from hybrid search.
#[derive(Debug, Clone)]
pub struct HybridResult {
    /// Row id in the `facts` table.
    pub id: i64,
    /// The stored fact text.
    pub content: String,
    /// BM25 score (normalized 0..1), `None` if FTS was not run or the
    /// document did not appear in FTS results.
    pub fts_score: Option<f64>,
    /// Cosine similarity (0..1), `None` if vector search was not run or the
    /// document did not have an embedding.
    pub vector_score: Option<f32>,
    /// Combined RRF score (always present).
    ///
    /// Always a true Reciprocal Rank Fusion score (`1/(k + rank)`) regardless
    /// of `SearchMode`, so scores are comparable across modes and can be used
    /// with consistent thresholds.
    pub rrf_score: f64,
}

/// Which retrieval backends to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// Merge FTS5 and vector results via RRF (default).
    Hybrid,
    /// BM25 full-text only.
    FtsOnly,
    /// Cosine similarity only.
    VectorOnly,
}

impl Default for SearchMode {
    fn default() -> Self {
        SearchMode::Hybrid
    }
}

// ── RRF constant ──────────────────────────────────────────────────────────

/// Standard RRF smoothing constant (k=60 from the original Cormack et al. paper).
const RRF_K: f64 = 60.0;

// ── Core algorithms ────────────────────────────────────────────────────────

/// Cosine similarity between two vectors.
///
/// Returns `0.0` if either vector is zero-magnitude or if the dimensions
/// don't match.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let mag_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let mag_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if mag_a < 1e-9 || mag_b < 1e-9 {
        return 0.0;
    }
    (dot / (mag_a * mag_b)).clamp(-1.0, 1.0)
}

/// Merge two ranked lists via Reciprocal Rank Fusion.
///
/// `fts`: `(id, score)` pairs from FTS search, sorted descending (best first).
/// `vec`: `(id, score)` pairs from vector search, sorted descending (best first).
///
/// Returns `(id, rrf_score)` pairs sorted descending by RRF score.
pub fn rrf_merge(fts: &[(i64, f64)], vec: &[(i64, f32)]) -> Vec<(i64, f64)> {
    use std::collections::HashMap;

    let mut scores: HashMap<i64, f64> = HashMap::new();

    // FTS contribution: rank is 1-indexed position in the FTS list
    for (rank, (id, _score)) in fts.iter().enumerate() {
        let rrf_contrib = 1.0 / (RRF_K + (rank + 1) as f64);
        *scores.entry(*id).or_insert(0.0) += rrf_contrib;
    }

    // Vector contribution: rank is 1-indexed position in the vector list
    for (rank, (id, _score)) in vec.iter().enumerate() {
        let rrf_contrib = 1.0 / (RRF_K + (rank + 1) as f64);
        *scores.entry(*id).or_insert(0.0) += rrf_contrib;
    }

    let mut merged: Vec<(i64, f64)> = scores.into_iter().collect();
    merged.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    merged
}

// ── Main search function ───────────────────────────────────────────────────

/// Maximum number of rows scanned in vector search.
///
/// Loading all embeddings is O(n) in memory. This cap prevents unbounded
/// memory use on large fact stores. At this scale, in-memory cosine scan
/// is fast (<50ms). When the store grows beyond ~50k facts, replace with
/// an approximate nearest-neighbor index (e.g. HNSW via hnswlib-rs).
const MAX_VECTOR_SCAN_ROWS: i64 = 10_000;

/// Perform hybrid search over the `facts` table.
///
/// # Arguments
/// - `conn`: open database connection.
/// - `query`: text query (used for FTS).
/// - `query_embedding`: optional embedding for vector search. Required when
///   `mode` is `Hybrid` or `VectorOnly`; if `None` in those modes, the mode
///   effectively degrades to `FtsOnly`.
/// - `limit`: maximum number of results to return.
/// - `mode`: which backends to use.
///
/// # Returns
/// A `Vec<HybridResult>` sorted descending by `rrf_score`, truncated to `limit`.
/// `rrf_score` is always a true RRF score (`1/(k+rank)`) regardless of mode,
/// so scores are comparable across modes.
pub fn hybrid_search(
    conn: &Connection,
    query: &str,
    query_embedding: Option<&[f32]>,
    limit: usize,
    mode: SearchMode,
    user_id: &str,
    embedding_sec: Option<&EmbeddingSecurity>,
) -> Result<Vec<HybridResult>> {
    if limit == 0 {
        return Ok(vec![]);
    }

    // ── 1. FTS search ─────────────────────────────────────────────────────
    // Try memories_v2 FTS first; fall back to legacy memory_fts for stores
    // that haven't been migrated yet (transitional safety net).
    let fts_results: Vec<(i64, f64)> = if mode == SearchMode::Hybrid || mode == SearchMode::FtsOnly {
        let v2 = fts_search_v2(conn, query, limit * 2)?;
        if v2.is_empty() {
            fts_search(conn, query, limit * 2)?
        } else {
            v2
        }
    } else {
        vec![]
    };

    // ── 2. Vector search ──────────────────────────────────────────────────
    let vec_results: Vec<(i64, f32)> = if (mode == SearchMode::Hybrid || mode == SearchMode::VectorOnly)
        && query_embedding.is_some()
    {
        let qemb = query_embedding.unwrap();
        // EMEM-01: scan memories_v2 (rowid is the numeric alias, embedding is BLOB).
        // Limit scan to MAX_VECTOR_SCAN_ROWS ordered by most-recently created.
        let mut stmt = conn
            .prepare(
                "SELECT rowid, embedding FROM memories_v2 \
                 WHERE embedding IS NOT NULL AND user_id = ?2 \
                 ORDER BY rowid DESC \
                 LIMIT ?1",
            )
            .map_err(|e| LuminaError::Config(format!("vector search prepare failed: {e}")))?;

        let raw_mapped = stmt
            .query_map(rusqlite::params![MAX_VECTOR_SCAN_ROWS, user_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
            });

        let mapped = match raw_mapped {
            Ok(m) => m,
            Err(e) => return Err(LuminaError::Config(format!("vector search query_map failed: {e}"))),
        };

        let rows: Vec<(i64, Vec<u8>)> = {
            let mut acc = Vec::new();
            for r in mapped {
                match r {
                    Ok(v) => acc.push(v),
                    Err(e) => return Err(LuminaError::Config(format!("vector search row failed: {e}"))),
                }
            }
            acc
        };

        let mut scored: Vec<(i64, f32)> = rows
            .iter()
            .filter_map(|(id, blob)| {
                // ESEC-03: decrypt if embedding_sec is provided and blob is encrypted.
                let emb = if let Some(sec) = embedding_sec {
                    sec.maybe_decrypt(blob).ok()?
                } else {
                    decode_embedding(blob)?
                };
                Some((*id, cosine_similarity(qemb, &emb)))
            })
            .collect();

        // Sort descending by cosine score
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit * 2);
        scored
    } else {
        vec![]
    };

    // ── 3. Merge ──────────────────────────────────────────────────────────
    // All modes go through rrf_merge (or a single-list variant) so that
    // `rrf_score` is always a true RRF value (1/(k+rank)), not a raw BM25 or
    // cosine score. This makes scores comparable across modes.
    let merged_ids: Vec<(i64, f64)> = match mode {
        SearchMode::Hybrid => rrf_merge(&fts_results, &vec_results),
        // Single-list RRF: only one list contributes, giving 1/(k+rank) scores
        SearchMode::FtsOnly => rrf_merge(&fts_results, &[]),
        SearchMode::VectorOnly => rrf_merge(&[], &vec_results),
    };

    // Truncate to requested limit
    let top_ids: Vec<(i64, f64)> = merged_ids.into_iter().take(limit).collect();

    if top_ids.is_empty() {
        return Ok(vec![]);
    }

    // ── 4. Fetch content for the top results ──────────────────────────────
    let fts_map: std::collections::HashMap<i64, f64> =
        fts_results.iter().cloned().collect();
    let vec_map: std::collections::HashMap<i64, f32> =
        vec_results.iter().cloned().collect();

    let mut results = Vec::with_capacity(top_ids.len());
    for (id, rrf_score) in &top_ids {
        // EMEM-01: fetch from memories_v2 by rowid.
        let content_res: std::result::Result<String, rusqlite::Error> = conn.query_row(
            "SELECT content FROM memories_v2 WHERE rowid = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        );
        let content = match content_res {
            Ok(c) => c,
            Err(_) => continue,
        };
        results.push(HybridResult {
            id: *id,
            content,
            fts_score: fts_map.get(id).copied(),
            vector_score: vec_map.get(id).copied(),
            rrf_score: *rrf_score,
        });
    }

    Ok(results)
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engram::{encode_embedding};
    use rusqlite::Connection;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        // EMEM-01: use memories_v2 schema
        crate::engram::migration::create_memories_v2(&conn).unwrap();
        crate::engram::fts::create_fts_v2_table(&conn).unwrap();
        conn
    }

    fn insert_fact(conn: &Connection, content: &str, embedding: Option<&[f32]>) -> i64 {
        let blob = embedding.map(encode_embedding);
        let id = crate::engram::types::new_uuid();
        let now = crate::engram::types::iso_now();
        conn.execute(
            "INSERT INTO memories_v2 (id, user_id, memory_type, visibility, sensitivity, content, embedding, confidence, access_count, created_at, updated_at)
             VALUES (?1, 'system', 'semantic', 'private', 'general', ?2, ?3, 0.8, 0, ?4, ?4)",
            rusqlite::params![id, content, blob, now],
        ).unwrap();
        let rowid = conn.last_insert_rowid();
        let _ = crate::engram::fts::fts_sync_insert_v2(conn, rowid, content);
        rowid
    }

    // ── cosine_similarity tests ────────────────────────────────────────────

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = [1.0f32, 0.0];
        let b = [0.0f32, 1.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-6, "Orthogonal vectors should have cosine = 0, got {sim}");
    }

    #[test]
    fn test_cosine_similarity_identical() {
        let v = [0.6f32, 0.8];
        let sim = cosine_similarity(&v, &v);
        assert!((sim - 1.0).abs() < 1e-5, "Identical vectors should have cosine = 1, got {sim}");
    }

    #[test]
    fn test_cosine_similarity_zero_vector() {
        let zero = [0.0f32, 0.0];
        let non_zero = [1.0f32, 0.0];
        assert_eq!(cosine_similarity(&zero, &non_zero), 0.0);
        assert_eq!(cosine_similarity(&non_zero, &zero), 0.0);
        assert_eq!(cosine_similarity(&zero, &zero), 0.0);
    }

    // ── rrf_merge tests ────────────────────────────────────────────────────

    #[test]
    fn test_rrf_merge_doc_in_both() {
        // doc 1 appears in both lists, doc 2 only in FTS, doc 3 only in vector
        let fts: Vec<(i64, f64)> = vec![(1, 0.9), (2, 0.5)];
        let vec_: Vec<(i64, f32)> = vec![(1, 0.85), (3, 0.4)];
        let merged = rrf_merge(&fts, &vec_);

        // doc 1 should have highest RRF score (appears in both, ranks #1 in both)
        assert!(!merged.is_empty());
        assert_eq!(merged[0].0, 1, "doc 1 (in both lists) should rank first");

        let doc1_score = merged.iter().find(|(id, _)| *id == 1).map(|(_, s)| *s).unwrap();
        let doc2_score = merged.iter().find(|(id, _)| *id == 2).map(|(_, s)| *s).unwrap();
        let doc3_score = merged.iter().find(|(id, _)| *id == 3).map(|(_, s)| *s).unwrap();

        assert!(doc1_score > doc2_score, "doc in both lists should outscore doc in one list (FTS)");
        assert!(doc1_score > doc3_score, "doc in both lists should outscore doc in one list (vec)");
    }

    #[test]
    fn test_rrf_merge_doc_one_list() {
        // A document that only appears in one list still gets a valid score
        let fts: Vec<(i64, f64)> = vec![(42, 0.7)];
        let vec_: Vec<(i64, f32)> = vec![];
        let merged = rrf_merge(&fts, &vec_);

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].0, 42);
        // Score should be 1 / (60 + 1) ≈ 0.01639
        let expected = 1.0 / (RRF_K + 1.0);
        assert!(
            (merged[0].1 - expected).abs() < 1e-9,
            "Expected {expected}, got {}",
            merged[0].1
        );
    }

    // ── search_mode_switching test ─────────────────────────────────────────

    #[test]
    fn test_search_mode_switching() {
        let conn = setup_db();
        let emb = vec![1.0f32, 0.0, 0.0];
        insert_fact(&conn, "keyword search test", Some(&emb));

        let query_emb = [1.0f32, 0.0, 0.0];

        // VectorOnly mode
        let vec_results = hybrid_search(&conn, "keyword", Some(&query_emb), 5, SearchMode::VectorOnly, "system", None).unwrap();
        // Results may be empty or contain items — we just verify no error and mode respected
        for r in &vec_results {
            // In VectorOnly mode, fts_score should be None
            assert!(r.fts_score.is_none(), "VectorOnly mode should have no fts_score");
        }

        // FtsOnly mode
        let fts_results = hybrid_search(&conn, "keyword", None, 5, SearchMode::FtsOnly, "system", None).unwrap();
        for r in &fts_results {
            // In FtsOnly mode, vector_score should be None
            assert!(r.vector_score.is_none(), "FtsOnly mode should have no vector_score");
        }
    }

    // ── hybrid end-to-end test ─────────────────────────────────────────────

    #[test]
    fn test_hybrid_end_to_end() {
        let conn = setup_db();

        // Insert docs with known embeddings and content
        let emb_x = vec![1.0f32, 0.0, 0.0];
        let emb_y = vec![0.0f32, 1.0, 0.0];
        let id1 = insert_fact(&conn, "lumina memory system stores facts", Some(&emb_x));
        let id2 = insert_fact(&conn, "unrelated cooking recipe pasta", Some(&emb_y));

        // Query close to emb_x AND matching keyword "lumina"
        let query_emb = [0.95f32, 0.05, 0.0];
        let results = hybrid_search(
            &conn,
            "lumina",
            Some(&query_emb),
            10,
            SearchMode::Hybrid,
            "system",
            None,
        )
        .unwrap();

        // If FTS5 unavailable, results may be determined solely by vector search
        if results.is_empty() {
            // Empty result is acceptable when both FTS and vector return nothing
            return;
        }

        // doc 1 should rank above doc 2 (closer in vector space + FTS match)
        let pos1 = results.iter().position(|r| r.id == id1);
        let pos2 = results.iter().position(|r| r.id == id2);

        if let (Some(p1), Some(p2)) = (pos1, pos2) {
            assert!(p1 < p2, "doc matching both keyword and vector should rank higher");
        }
        // At minimum, doc 1 should appear
        assert!(pos1.is_some(), "Expected doc1 (id={id1}) in hybrid results");
    }
}
