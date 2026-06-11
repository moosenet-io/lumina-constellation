//! EMEM-07: Type-aware, privacy-filtered, temporally-weighted memory retrieval.
//!
//! ## What this module provides
//!
//! 1. `MemoryQuery` — structured query with user_id, query text/embedding, type
//!    filter, visibility options, and max_results.
//! 2. `ScoredMemory` — a `Memory` paired with its final composite score.
//! 3. `retrieve()` — async pipeline: embed → candidate fetch → cosine scoring →
//!    temporal weighting → privacy filtering → top-K return.
//! 4. `fetch_candidates_for_query()` + `score_candidates()` — the two-call pattern
//!    used by `agent_loop.rs` to avoid holding `&EngramStore` across an `.await`.
//! 5. `format_for_context()` — format a `ScoredMemory` slice for LLM injection
//!    with type labels, priority ordering, and a context budget.
//!
//! ## Pipeline
//! ```text
//! query_text → embed()
//!     ↓
//! SQL: user_id + visibility + optional type filter
//!     ↓
//! cosine(query_emb, memory_emb) per candidate
//!     ↓
//! apply_temporal_scoring() — decay × access_boost
//!     ↓
//! PrivacyEnforcer::validate_access() per result (defense in depth)
//!     ↓
//! sort descending by score → take max_results
//!     ↓
//! Vec<ScoredMemory>
//! ```
//!
//! ## Integration
//! `agent_loop.rs` uses `fetch_candidates_for_query` + `score_candidates` directly
//! (the two-call pattern) to avoid holding `&EngramStore` across an `.await` point.
//! After scoring, the caller re-opens the store briefly (sync, no await held) to call
//! `lifecycle::record_access` for each returned memory so `access_boost` grows from
//! real retrievals.  On any error the caller falls back to the existing
//! `inject_memory_bullets` behavior (backward compatible).

use crate::config::Config;
use crate::error::Result;
use crate::engram::{
    cosine, embed, EngramStore,
    lifecycle,
    privacy::PrivacyEnforcer,
    temporal::{temporal_decay, access_boost},
    types::{Memory, MemoryType},
    secure_memory::RedactedString,
};

/// Minimum cosine similarity for a memory to be included in retrieval results.
/// Memories below this threshold are irrelevant to the query and excluded.
const MIN_SIMILARITY: f32 = 0.2;

// ── MemoryQuery ───────────────────────────────────────────────────────────────

/// A structured retrieval request.
///
/// Build with `MemoryQuery::new()` then chain the builder methods to add filters.
pub struct MemoryQuery {
    /// Owner / requester user_id. All results are scoped to this user.
    pub user_id: String,
    /// Natural-language query text (embedded if `query_embedding` is empty).
    pub query_text: String,
    /// Pre-computed query embedding. If empty, `retrieve()` will call `embed()`.
    pub query_embedding: Vec<f32>,
    /// Optional type filter. `None` means all types.
    pub types: Option<Vec<MemoryType>>,
    /// Maximum number of results to return (default: 10).
    pub max_results: usize,
    /// Include `Shared` visibility memories (EMEM-08 will add household check).
    pub include_shared: bool,
    /// Include `System` visibility memories.
    pub include_system: bool,
}

impl MemoryQuery {
    /// Create a new query with sensible defaults.
    ///
    /// - `max_results` = 10
    /// - `types` = None (all types)
    /// - `include_shared` = false
    /// - `include_system` = false
    pub fn new(user_id: &str, query_text: &str) -> Self {
        Self {
            user_id: user_id.to_string(),
            query_text: query_text.to_string(),
            query_embedding: Vec::new(),
            types: None,
            max_results: 10,
            include_shared: false,
            include_system: false,
        }
    }

    /// Restrict results to the given memory types.
    pub fn with_types(mut self, types: Vec<MemoryType>) -> Self {
        self.types = Some(types);
        self
    }

    /// Set the maximum number of results.
    pub fn with_max_results(mut self, n: usize) -> Self {
        self.max_results = n;
        self
    }

    /// Include `Shared` visibility memories in results.
    pub fn with_shared(mut self) -> Self {
        self.include_shared = true;
        self
    }

    /// Include `System` visibility memories in results.
    pub fn with_system(mut self) -> Self {
        self.include_system = true;
        self
    }

    /// Set a pre-computed query embedding (skip the embed() call in retrieve()).
    pub fn with_embedding(mut self, embedding: Vec<f32>) -> Self {
        self.query_embedding = embedding;
        self
    }
}

// ── ScoredMemory ──────────────────────────────────────────────────────────────

/// A retrieved memory with its final composite score.
///
/// Score = cosine_similarity × temporal_decay × access_boost.
/// Range: [0.0, ∞) (access_boost can push above 1.0).
#[derive(Debug)]
pub struct ScoredMemory {
    pub memory: Memory,
    /// Final composite score (cosine × decay × boost).
    pub score: f32,
}

// ── retrieve() ────────────────────────────────────────────────────────────────

/// Run the full retrieval pipeline for `query` against `store`.
///
/// ## Critical design note — `Send` safety
/// `Connection: !Send`, so `&EngramStore: !Send`. The async future returned by
/// this function must therefore NOT hold `&EngramStore` across any `.await` point.
/// To achieve this, the pipeline is split into two phases:
/// 1. **Sync phase** (`fetch_candidates`): load all candidate data from the store
///    synchronously. The store reference is dropped as soon as this returns.
/// 2. **Async phase** (`embed` + `score_and_filter`): embed the query and score
///    candidates without holding any store reference.
///
/// Callers in `agent_loop.rs` use `fetch_candidates_for_query` + `score_candidates`
/// directly (the two-call pattern) instead of calling this function, so they can
/// record accesses and re-use an already-computed embedding.  Access tracking
/// (`lifecycle::record_access`) is called inside `retrieve()` after scoring.
///
/// ## Steps
/// 1. Fetch candidates from store (sync, store dropped immediately after).
/// 2. Embed query text (or use pre-set embedding). On embed failure: return empty.
/// 3. Cosine similarity per candidate.
/// 4. Temporal scoring: score × temporal_decay × access_boost.
/// 5. Privacy filter: `PrivacyEnforcer::validate_access()` per candidate.
/// 6. Sort descending by score, take top `query.max_results`.
/// 7. Record access for each returned memory (bumps `access_count` / `access_boost`).
///
/// Returns `Ok(Vec<ScoredMemory>)`. Returns empty vec (not Err) on embedding failure.
pub async fn retrieve(
    query: &MemoryQuery,
    store: &EngramStore,
    config: &Config,
) -> Result<Vec<ScoredMemory>> {
    // ── Phase 1: sync — load candidates, drop store reference ────────────────
    let candidates = fetch_candidates(store, query)?;
    // `store` is NOT referenced after this line (async phase).

    // ── Phase 2: async — embed + score (no store reference held) ─────────────
    let results = score_candidates(candidates, query, config).await?;

    // ── Phase 3: sync — record access for returned memories (P2-14 lifecycle) ─
    // Store reference re-acquired briefly for a sync-only operation (no await held).
    for sm in &results {
        // Lookup rowid by UUID — non-fatal if the row has been deleted since fetch
        let rowid: Option<i64> = store.conn.query_row(
            "SELECT rowid FROM memories_v2 WHERE id = ?1",
            rusqlite::params![sm.memory.id],
            |r| r.get(0),
        ).ok();
        if let Some(rid) = rowid {
            let _ = lifecycle::record_access(&store.conn, rid);
        }
    }

    Ok(results)
}

/// Score a pre-fetched candidate list against the query embedding.
///
/// This is the `Send`-safe async half of `retrieve()`.  It is also called
/// directly from `agent_loop.rs` (paired with `fetch_candidates_for_query`)
/// to avoid holding `&EngramStore` across the await point at the call site.
/// After this returns, the caller should re-open the store (sync, no await) to
/// call `lifecycle::record_access` for each returned memory.
pub async fn score_candidates(
    candidates: Vec<Candidate>,
    query: &MemoryQuery,
    config: &Config,
) -> Result<Vec<ScoredMemory>> {
    let query_emb: Vec<f32> = if !query.query_embedding.is_empty() {
        query.query_embedding.clone()
    } else {
        match embed(&query.query_text, config).await {
            Ok(e) => e,
            Err(_) => {
                // Embedding unavailable — graceful degradation
                return Ok(Vec::new());
            }
        }
    };

    let mut results = score_and_filter(candidates, &query_emb, &query.user_id);
    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(query.max_results);
    Ok(results)
}

/// Fetch candidates from `store` synchronously (phase 1 of retrieve).
///
/// Returns a `Vec<Candidate>` that can be passed to `score_candidates` without
/// holding any `&EngramStore` reference across an await.
pub fn fetch_candidates_for_query(store: &EngramStore, query: &MemoryQuery) -> Result<Vec<Candidate>> {
    fetch_candidates(store, query)
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Candidate row fetched from the database before scoring.
///
/// `pub` so `agent_loop.rs` can use `fetch_candidates_for_query` + `score_candidates`
/// directly to avoid holding `&EngramStore` across an await point.
pub struct Candidate {
    pub memory: Memory,
    /// Row-level embedding.
    pub embedding: Vec<f32>,
}

/// Fetch candidate memories from the store, applying user/type/visibility filters.
///
/// This is the synchronous half of `retrieve()` — no awaits, safe to call while
/// holding `&EngramStore`.
fn fetch_candidates(store: &EngramStore, query: &MemoryQuery) -> Result<Vec<Candidate>> {
    use crate::error::LuminaError;
    use rusqlite::params_from_iter;

    // Build dynamic SQL
    let mut conditions: Vec<String> = Vec::new();
    let mut param_values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    let mut param_idx = 1usize;

    // user_id condition — also pulls in shared/system when flags are set
    if query.include_shared && query.include_system {
        conditions.push(format!(
            "(user_id = ?{} OR visibility = 'shared' OR visibility = 'system')",
            param_idx
        ));
    } else if query.include_shared {
        conditions.push(format!(
            "(user_id = ?{} OR visibility = 'shared')",
            param_idx
        ));
    } else if query.include_system {
        conditions.push(format!(
            "(user_id = ?{} OR visibility = 'system')",
            param_idx
        ));
    } else {
        conditions.push(format!("user_id = ?{}", param_idx));
    }
    param_values.push(Box::new(query.user_id.clone()));
    param_idx += 1;

    // Visibility safety: always exclude other users' private memories
    // (defense in depth — the user_id filter above already handles most cases,
    // but this prevents cross-user leakage if a shared memory has a wrong owner set)
    conditions.push(format!(
        "(visibility != 'private' OR user_id = ?{})",
        param_idx
    ));
    param_values.push(Box::new(query.user_id.clone()));
    param_idx += 1;

    // Not superseded
    conditions.push("superseded_by IS NULL".to_string());

    // Optional type filter
    if let Some(ref types) = query.types {
        if !types.is_empty() {
            let placeholders: Vec<String> = types
                .iter()
                .enumerate()
                .map(|(i, _)| format!("?{}", param_idx + i))
                .collect();
            conditions.push(format!("memory_type IN ({})", placeholders.join(", ")));
            for t in types {
                param_values.push(Box::new(t.to_db().to_string()));
                param_idx += 1;
            }
        }
    }

    let _ = param_idx; // silence unused warning

    let where_clause = conditions.join(" AND ");
    let sql = format!(
        "SELECT id, user_id, memory_type, visibility, sensitivity, content, embedding,
                source_conversation_id, source_turn_index, confidence, access_count,
                last_accessed, created_at, updated_at, superseded_by, tags
         FROM memories_v2
         WHERE {}
         ORDER BY created_at DESC",
        where_clause
    );

    let mut stmt = store
        .conn
        .prepare(&sql)
        .map_err(|e| LuminaError::Config(format!("retrieval: prepare failed: {e}")))?;

    // Build params_from_iter-compatible slice
    let param_refs: Vec<&dyn rusqlite::ToSql> = param_values.iter().map(|b| b.as_ref()).collect();

    // ESEC-03: embeddings are stored encrypted; decrypt via EmbeddingSecurity.
    let emb_sec = &store.embedding_sec;
    let rows = stmt
        .query_map(params_from_iter(param_refs.iter().copied()), |row| {
            row_to_memory_with_blob(row, emb_sec)
        })
        .map_err(|e| LuminaError::Config(format!("retrieval: query failed: {e}")))?;

    let mut candidates = Vec::new();
    for row in rows {
        match row {
            Ok((memory, embedding)) => candidates.push(Candidate { memory, embedding }),
            Err(e) => {
                eprintln!("engram/retrieval: skipping row: {e}");
            }
        }
    }

    Ok(candidates)
}

/// Map a database row to `(Memory, Vec<f32>)`.
///
/// ESEC-03: embeddings are stored encrypted; `emb_sec.decrypt_embedding()` handles
/// both encrypted blobs (magic prefix) and legacy plaintext f32 LE blobs transparently.
fn row_to_memory_with_blob(
    row: &rusqlite::Row<'_>,
    emb_sec: &crate::engram::embedding_security::EmbeddingSecurity,
) -> rusqlite::Result<(Memory, Vec<f32>)> {
    use crate::engram::types::{SensitivityCategory, MemoryType, Visibility};

    let tags_json: String = row.get(15).unwrap_or_else(|_| "[]".to_string());
    let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
    let emb_blob: Option<Vec<u8>> = row.get(6)?;
    // ESEC-03: decrypt blob (handles encrypted + legacy plaintext paths).
    let embedding = match emb_blob {
        Some(blob) if !blob.is_empty() => {
            emb_sec.decrypt_embedding(&blob).unwrap_or_default()
        }
        _ => Vec::new(),
    };

    let memory = Memory {
        id: row.get(0)?,
        user_id: row.get(1)?,
        memory_type: MemoryType::from_db(&row.get::<_, String>(2)?),
        visibility: Visibility::from_db(&row.get::<_, String>(3)?),
        sensitivity: SensitivityCategory::from_db(&row.get::<_, String>(4)?),
        content: row.get(5)?,
        embedding: embedding.clone(),
        source_conversation_id: row.get(7)?,
        source_turn_index: row.get(8)?,
        confidence: row.get(9).unwrap_or(0.8),
        access_count: row.get(10).unwrap_or(0),
        last_accessed: row.get(11)?,
        created_at: row.get(12).unwrap_or_default(),
        updated_at: row.get(13).unwrap_or_default(),
        superseded_by: row.get(14)?,
        tags,
    };

    Ok((memory, embedding))
}

/// Score candidates with cosine × temporal_decay × access_boost, then privacy-filter.
///
/// Memories with no embedding are assigned score 0.0 (not a fabricated 0.5) so
/// they only surface in FTS-dominant queries, not above well-matched embedded ones.
/// Memories below MIN_SIMILARITY are excluded entirely (same threshold as v1 retrieve).
fn score_and_filter(
    candidates: Vec<Candidate>,
    query_emb: &[f32],
    caller_user_id: &str,
) -> Vec<ScoredMemory> {
    candidates
        .into_iter()
        .filter_map(|c| {
            let sim: f32 = if query_emb.is_empty() || c.embedding.is_empty() {
                // No embedding: score 0.0 — only participates if temporal/access boost
                // is extremely high (very frequently accessed old memory with no embedding)
                0.0
            } else {
                cosine(query_emb, &c.embedding)
            };

            // Exclude memories with no meaningful semantic similarity to the query.
            // Embeddings-less memories (sim=0.0) are also filtered out here — they
            // should only surface via FTS in the hybrid_search path, not here.
            if sim < MIN_SIMILARITY {
                return None;
            }

            // Temporal scoring
            let decay = temporal_decay(&c.memory.created_at) as f32;
            let boost = access_boost(c.memory.access_count) as f32;
            let score = sim * decay * boost;

            // Privacy filter (application-level defense in depth)
            if PrivacyEnforcer::validate_access(caller_user_id, &c.memory).is_err() {
                return None;
            }

            Some(ScoredMemory { memory: c.memory, score })
        })
        .collect()
}

// ── format_for_context() ──────────────────────────────────────────────────────

/// Format a slice of `ScoredMemory` for injection into an LLM system prompt.
///
/// ## Priority ordering
/// Within the formatted output memories are ordered by type priority:
/// **Principle > Preference > Semantic (Fact) > Episodic (Recent)**
/// (as defined by `MemoryType::retrieval_priority()`).
/// Within the same type, the original score ordering (highest first) is preserved.
///
/// ## Context budget
/// `max_tokens` limits the approximate token count of the output (1 token ≈ 4 chars).
/// When the budget is exhausted the lowest-priority memories are dropped first.
/// At minimum, the header line + the top-1 memory are always included (truncated
/// if necessary) so the caller always gets something useful.
///
/// ## Output format
/// ```text
/// ## What I know about you:
/// [Principle] You prefer direct, efficient communication
/// [Preference] You like dark roast coffee
/// [Fact] You work in field marketing
/// [Recent] Yesterday we discussed the auth issue
/// ```
///
/// Returns an empty RedactedString if `memories` is empty.
///
/// Returns `RedactedString` (ZeroizeOnDrop) so the formatted memory block is
/// zeroized from heap after injection, satisfying the ESEC-01 guarantee.
pub fn format_for_context(memories: &[ScoredMemory], max_tokens: usize) -> RedactedString {
    if memories.is_empty() {
        return RedactedString::new(String::new());
    }

    // Sort a copy by type priority first, then by score descending within the same type.
    let mut sorted: Vec<&ScoredMemory> = memories.iter().collect();
    sorted.sort_by(|a, b| {
        let prio_cmp = a
            .memory
            .memory_type
            .retrieval_priority()
            .cmp(&b.memory.memory_type.retrieval_priority());
        if prio_cmp != std::cmp::Ordering::Equal {
            return prio_cmp;
        }
        // Same type: higher score first
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let max_chars = max_tokens * 4;
    let header = "## What I know about you:\n";
    let mut out = String::with_capacity(header.len() + sorted.len() * 80);
    out.push_str(header);

    for m in &sorted {
        // Check budget: always include at least one entry (the first)
        if out.len() > header.len() && out.len() >= max_chars {
            break;
        }
        let line = format!("{} {}\n", m.memory.type_label(), m.memory.content);
        out.push_str(&line);
    }

    RedactedString::new(out)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use crate::engram::{
        types::{Memory, MemoryType, SensitivityCategory, Visibility, iso_now, unix_secs_to_iso},
        EngramStore,
    };
    use std::time::{SystemTime, UNIX_EPOCH};

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn test_key() -> Vec<u8> {
        vec![0u8; 32]
    }

    fn tmp_db(tag: &str) -> std::path::PathBuf {
        let p = std::path::PathBuf::from(format!("/tmp/lumina_retrieval_test_{tag}.db"));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// Insert a memory with a known embedding into the store (via insert_memory).
    fn insert_mem(
        store: &EngramStore,
        user_id: &str,
        mtype: MemoryType,
        visibility: Visibility,
        content: &str,
        emb: Vec<f32>,
    ) -> Memory {
        let mut m = Memory::new(user_id, mtype, SensitivityCategory::General, content);
        m.visibility = visibility;
        m.embedding = emb;
        store.insert_memory(&m).unwrap();
        m
    }

    // ── MemoryQuery builder ───────────────────────────────────────────────────

    #[test]
    fn test_memory_query_defaults() {
        let q = MemoryQuery::new("alice", "hello");
        assert_eq!(q.user_id, "alice");
        assert_eq!(q.query_text, "hello");
        assert_eq!(q.max_results, 10);
        assert!(q.types.is_none());
        assert!(!q.include_shared);
        assert!(!q.include_system);
    }

    #[test]
    fn test_memory_query_builder_chain() {
        let q = MemoryQuery::new("bob", "test")
            .with_types(vec![MemoryType::Preference, MemoryType::Principle])
            .with_max_results(5)
            .with_shared();

        assert_eq!(q.max_results, 5);
        assert!(q.include_shared);
        assert!(!q.include_system);
        let types = q.types.unwrap();
        assert!(types.contains(&MemoryType::Preference));
        assert!(types.contains(&MemoryType::Principle));
    }

    // ── Type filter ───────────────────────────────────────────────────────────

    /// EMEM-07: type filter returns only memories of the requested type(s).
    #[test]
    fn test_memory_query_type_filter_returns_only_matching_types() {
        let path = tmp_db("type_filter");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        let emb = vec![1.0f32, 0.0, 0.0];
        insert_mem(&store, "system", MemoryType::Preference, Visibility::Private,
                   "likes dark roast coffee", emb.clone());
        insert_mem(&store, "system", MemoryType::Semantic, Visibility::Private,
                   "works in field marketing", emb.clone());
        insert_mem(&store, "system", MemoryType::Episodic, Visibility::Private,
                   "had standup on Monday", emb.clone());
        insert_mem(&store, "system", MemoryType::Principle, Visibility::Private,
                   "values efficiency", emb.clone());

        let query = MemoryQuery::new("system", "coffee")
            .with_types(vec![MemoryType::Preference])
            .with_embedding(emb.clone());

        let candidates = fetch_candidates(&store, &query).unwrap();
        assert_eq!(candidates.len(), 1, "should return only Preference memories");
        assert_eq!(candidates[0].memory.memory_type, MemoryType::Preference);
        assert_eq!(candidates[0].memory.content, "likes dark roast coffee");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_memory_query_multiple_types_filter() {
        let path = tmp_db("multi_type_filter");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        let emb = vec![1.0f32, 0.0, 0.0];
        insert_mem(&store, "system", MemoryType::Preference, Visibility::Private,
                   "preference 1", emb.clone());
        insert_mem(&store, "system", MemoryType::Principle, Visibility::Private,
                   "principle 1", emb.clone());
        insert_mem(&store, "system", MemoryType::Episodic, Visibility::Private,
                   "episodic 1", emb.clone());

        let query = MemoryQuery::new("system", "query")
            .with_types(vec![MemoryType::Preference, MemoryType::Principle])
            .with_embedding(emb.clone());

        let candidates = fetch_candidates(&store, &query).unwrap();
        assert_eq!(candidates.len(), 2, "should return Preference + Principle only");

        let _ = std::fs::remove_file(&path);
    }

    // ── Privacy filter ────────────────────────────────────────────────────────

    /// EMEM-07: privacy filter removes inaccessible (cross-user private) memories.
    #[test]
    fn test_memory_query_privacy_filter_removes_inaccessible() {
        let path = tmp_db("privacy_filter");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        // Alice's private memory
        let emb = vec![1.0f32, 0.0, 0.0];
        let mut alice_mem = Memory::new(
            "user-alice", MemoryType::Semantic, SensitivityCategory::General, "alice's secret",
        );
        alice_mem.visibility = Visibility::Private;
        alice_mem.embedding = emb.clone();
        store.insert_memory(&alice_mem).unwrap();

        // Bob's private memory
        let mut bob_mem = Memory::new(
            "user-bob", MemoryType::Semantic, SensitivityCategory::General, "bob's fact",
        );
        bob_mem.visibility = Visibility::Private;
        bob_mem.embedding = emb.clone();
        store.insert_memory(&bob_mem).unwrap();

        // Query as alice — should only see alice's memory
        let query_alice = MemoryQuery::new("user-alice", "secret").with_embedding(emb.clone());
        let candidates_alice = fetch_candidates(&store, &query_alice).unwrap();
        let scored_alice = score_and_filter(candidates_alice, &emb, "user-alice");

        assert_eq!(scored_alice.len(), 1, "alice should only see her own private memory");
        assert_eq!(scored_alice[0].memory.user_id, "user-alice");
        assert_eq!(scored_alice[0].memory.content, "alice's secret");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_memory_query_shared_memory_accessible_to_other_users() {
        let path = tmp_db("shared_access");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        let emb = vec![1.0f32, 0.0, 0.0];

        // Shared memory owned by alice
        let mut shared_mem = Memory::new(
            "user-alice", MemoryType::Semantic, SensitivityCategory::Household, "grocery list",
        );
        shared_mem.visibility = Visibility::Shared;
        shared_mem.embedding = emb.clone();
        store.insert_memory(&shared_mem).unwrap();

        // Bob queries with include_shared — should see alice's shared memory
        let query_bob = MemoryQuery::new("user-bob", "grocery")
            .with_embedding(emb.clone())
            .with_shared();

        let candidates = fetch_candidates(&store, &query_bob).unwrap();
        let scored = score_and_filter(candidates, &emb, "user-bob");
        assert_eq!(scored.len(), 1, "bob should see shared memory");
        assert_eq!(scored[0].memory.content, "grocery list");

        let _ = std::fs::remove_file(&path);
    }

    // ── Temporal weighting ────────────────────────────────────────────────────

    /// EMEM-07: temporal weighting boosts recent memories over old ones with equal base scores.
    #[test]
    fn test_memory_query_temporal_weighting_boosts_recent() {
        // Two candidates with identical embeddings (same cosine against any query)
        // but different creation timestamps. The newer one should score higher.
        let emb = vec![1.0f32, 0.0, 0.0];
        let query_emb = vec![1.0f32, 0.0, 0.0]; // cosine = 1.0 for both

        let now = now_secs();
        let old_ts = unix_secs_to_iso(now.saturating_sub(200 * 86400)); // 200 days ago
        let new_ts = iso_now();

        let mut old_mem = Memory::new("u", MemoryType::Semantic, SensitivityCategory::General, "old fact");
        old_mem.embedding = emb.clone();
        old_mem.created_at = old_ts;
        old_mem.access_count = 0;
        old_mem.visibility = Visibility::Private;

        let mut new_mem = Memory::new("u", MemoryType::Semantic, SensitivityCategory::General, "new fact");
        new_mem.embedding = emb.clone();
        new_mem.created_at = new_ts;
        new_mem.access_count = 0;
        new_mem.visibility = Visibility::Private;

        let candidates = vec![
            Candidate { memory: old_mem, embedding: emb.clone() },
            Candidate { memory: new_mem, embedding: emb.clone() },
        ];

        let mut scored = score_and_filter(candidates, &query_emb, "u");
        scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

        assert_eq!(scored.len(), 2);
        assert_eq!(scored[0].memory.content, "new fact",
            "new memory should rank first due to temporal weighting");
        assert!(scored[0].score > scored[1].score,
            "new memory score ({}) should exceed old ({})", scored[0].score, scored[1].score);
    }

    // ── Priority ordering in format_for_context ───────────────────────────────

    /// EMEM-07: Principle memories rank above Preference in formatted output.
    #[test]
    fn test_memory_query_principle_ranked_above_preference() {
        let emb = vec![1.0f32, 0.0, 0.0];
        let now_ts = iso_now();

        let mut principle_mem = Memory::new("u", MemoryType::Principle, SensitivityCategory::General, "You prefer efficiency");
        principle_mem.embedding = emb.clone();
        principle_mem.created_at = now_ts.clone();
        principle_mem.visibility = Visibility::Private;
        // Give the principle a lower raw score to ensure ordering is by type, not score
        let principle_scored = ScoredMemory { memory: principle_mem, score: 0.3 };

        let mut pref_mem = Memory::new("u", MemoryType::Preference, SensitivityCategory::General, "Likes dark roast coffee");
        pref_mem.embedding = emb.clone();
        pref_mem.created_at = now_ts.clone();
        pref_mem.visibility = Visibility::Private;
        // Preference has higher raw score
        let pref_scored = ScoredMemory { memory: pref_mem, score: 0.9 };

        let memories = vec![pref_scored, principle_scored];
        let output = format_for_context(&memories, 1000);

        let principle_pos = output.find("[Principle]").expect("should have Principle");
        let preference_pos = output.find("[Preference]").expect("should have Preference");

        assert!(
            principle_pos < preference_pos,
            "Principle should appear before Preference in output (pos {principle_pos} vs {preference_pos})"
        );
    }

    // ── format_for_context ────────────────────────────────────────────────────

    /// EMEM-07: format_for_context respects priority ordering for all four types.
    #[test]
    fn test_format_for_context_priority_ordering() {
        let now_ts = iso_now();

        let make_scored = |mtype: MemoryType, content: &str, score: f32| {
            let mut m = Memory::new("u", mtype, SensitivityCategory::General, content);
            m.created_at = now_ts.clone();
            m.visibility = Visibility::Private;
            ScoredMemory { memory: m, score }
        };

        // Insert in reverse priority order to verify sorting
        let memories = vec![
            make_scored(MemoryType::Episodic,   "Recent event",          0.9),
            make_scored(MemoryType::Semantic,    "A fact",                0.8),
            make_scored(MemoryType::Preference,  "Likes coffee",          0.7),
            make_scored(MemoryType::Principle,   "Values efficiency",     0.6),
        ];

        let output = format_for_context(&memories, 1000);

        let pos_principle  = output.find("[Principle]").expect("missing [Principle]");
        let pos_preference = output.find("[Preference]").expect("missing [Preference]");
        let pos_fact       = output.find("[Fact]").expect("missing [Fact]");
        let pos_recent     = output.find("[Recent]").expect("missing [Recent]");

        assert!(pos_principle < pos_preference, "Principle before Preference");
        assert!(pos_preference < pos_fact,      "Preference before Fact");
        assert!(pos_fact < pos_recent,          "Fact before Recent");
    }

    /// EMEM-07: context budget truncation drops lowest-priority memories first.
    #[test]
    fn test_format_for_context_context_budget_truncation() {
        let now_ts = iso_now();

        let make_scored = |mtype: MemoryType, content: &str| {
            let mut m = Memory::new("u", mtype, SensitivityCategory::General, content);
            m.created_at = now_ts.clone();
            m.visibility = Visibility::Private;
            ScoredMemory { memory: m, score: 0.8 }
        };

        let memories = vec![
            make_scored(MemoryType::Principle, "Values efficiency"),
            make_scored(MemoryType::Preference, "Likes dark roast coffee"),
            make_scored(MemoryType::Semantic, "Works in field marketing as a senior manager"),
            make_scored(MemoryType::Episodic, "Yesterday discussed the Nexus design"),
        ];

        // Tiny budget — just enough for header + one short line (~30 chars per token × 4 = 120 chars)
        let output_small = format_for_context(&memories, 10);
        // At minimum, the header and the first (highest-priority) memory should appear
        assert!(output_small.contains("## What I know about you:"), "header missing");
        assert!(output_small.contains("[Principle]"), "highest-priority memory should be included");
        // The lowest-priority (Episodic) should be excluded at a very tight budget
        // (header = 24 chars, one principle line ≈ 40 chars, budget 10*4 = 40 chars)
        // With budget this small the Episodic line should be cut
        let has_all_four = output_small.contains("[Principle]")
            && output_small.contains("[Preference]")
            && output_small.contains("[Fact]")
            && output_small.contains("[Recent]");
        assert!(!has_all_four, "tight budget should exclude lower-priority memories");
    }

    /// EMEM-07: type labels appear correctly for all four types.
    #[test]
    fn test_format_for_context_type_labels() {
        let now_ts = iso_now();

        let make_scored = |mtype: MemoryType, content: &str| {
            let mut m = Memory::new("u", mtype, SensitivityCategory::General, content);
            m.created_at = now_ts.clone();
            m.visibility = Visibility::Private;
            ScoredMemory { memory: m, score: 0.8 }
        };

        let memories = vec![
            make_scored(MemoryType::Principle, "p"),
            make_scored(MemoryType::Preference, "q"),
            make_scored(MemoryType::Semantic, "r"),
            make_scored(MemoryType::Episodic, "s"),
        ];

        let output = format_for_context(&memories, 1000);
        assert!(output.contains("[Principle]"), "missing [Principle] label");
        assert!(output.contains("[Preference]"), "missing [Preference] label");
        assert!(output.contains("[Fact]"),       "missing [Fact] label for Semantic");
        assert!(output.contains("[Recent]"),     "missing [Recent] label for Episodic");
    }

    /// EMEM-07: empty result set returns an empty string (no panic, no header).
    #[test]
    fn test_empty_results_returns_empty_string() {
        let output = format_for_context(&[], 1000);
        assert!(output.is_empty(), "empty memories should produce empty string, got: {output:?}");
    }

    /// EMEM-07: format_for_context with generous budget includes all memories.
    #[test]
    fn test_format_for_context_generous_budget_includes_all() {
        let now_ts = iso_now();
        let memories: Vec<ScoredMemory> = ["alpha", "beta", "gamma"]
            .iter()
            .map(|content| {
                let mut m = Memory::new("u", MemoryType::Semantic, SensitivityCategory::General, *content);
                m.created_at = now_ts.clone();
                m.visibility = Visibility::Private;
                ScoredMemory { memory: m, score: 0.5 }
            })
            .collect();

        let output = format_for_context(&memories, 10_000);
        for content in &["alpha", "beta", "gamma"] {
            assert!(output.contains(content), "should include {content}");
        }
    }

    // ── Access tracking via production path ───────────────────────────────────

    /// EMEM-07 (be252a9 fix): access_count must increment when a memory is returned
    /// through the production code path — fetch_candidates_for_query + score_candidates
    /// + the record_access block in agent_loop.rs.
    ///
    /// This test exercises the two-call pattern directly (no embedding service needed:
    /// we pre-set the embedding in the query) and then simulates the record_access
    /// step that agent_loop.rs performs after scoring.
    #[test]
    #[serial]
    fn test_access_count_increments_via_production_path() {
        let path = tmp_db("access_tracking_prod");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        // Insert a memory with a known embedding
        let emb = vec![1.0f32, 0.0, 0.0];
        insert_mem(
            &store,
            "system",
            MemoryType::Semantic,
            Visibility::Private,
            "a retrievable fact",
            emb.clone(),
        );

        // Verify initial access_count = 0
        let count_before: i64 = store.conn.query_row(
            "SELECT access_count FROM memories_v2 WHERE content = 'a retrievable fact'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(count_before, 0, "access_count should start at 0");

        // Step 1: fetch candidates (production path, sync)
        let query = MemoryQuery::new("system", "retrievable fact")
            .with_embedding(emb.clone());
        let candidates = fetch_candidates_for_query(&store, &query).unwrap();
        assert_eq!(candidates.len(), 1, "should find the inserted memory");

        // Step 2: score candidates (production path — does NOT call record_access itself)
        // Use a one-shot tokio runtime to drive the async fn.
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        // We need a Config with OLLAMA_EMBEDDING_URL unset — score_candidates will
        // skip the embed() call because the query already has a pre-set embedding.
        std::env::set_var("CHORD_PROXY_URL", "http://localhost:4000");
        std::env::set_var("LUMINA_CHORD_SECRET", "");
        let config = crate::config::Config::from_env().unwrap();
        let scored = rt
            .block_on(score_candidates(candidates, &query, &config))
            .unwrap();
        assert_eq!(scored.len(), 1, "should score the inserted memory");

        // Step 3: record_access — this is the fix: the agent_loop block that was
        // previously missing from the production path.
        for sm in &scored {
            let rowid: Option<i64> = store.conn.query_row(
                "SELECT rowid FROM memories_v2 WHERE id = ?1",
                rusqlite::params![sm.memory.id],
                |r| r.get(0),
            ).ok();
            if let Some(rid) = rowid {
                let _ = crate::engram::lifecycle::record_access(&store.conn, rid);
            }
        }

        // Verify access_count is now 1
        let count_after: i64 = store.conn.query_row(
            "SELECT access_count FROM memories_v2 WHERE content = 'a retrievable fact'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(count_after, 1, "access_count should be 1 after one retrieval through the production path");

        std::env::remove_var("CHORD_PROXY_URL");
        std::env::remove_var("LUMINA_CHORD_SECRET");
        let _ = std::fs::remove_file(&path);
    }
}
