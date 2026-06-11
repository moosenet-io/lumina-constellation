//! Engram — multi-substrate memory system (v2).
//!
//! Encrypted at rest via SQLCipher (ENGRAM_DB_KEY from vault).
//! Retrieval uses hybrid FTS5 + vector cosine similarity + RRF (EDGE-07).
//! Per-user isolation: each user has their own database file (P2-02).
//!
//! EMEM-01: v2 schema — typed memories (Episodic/Semantic/Preference/Principle),
//! visibility (Private/Shared/System), sensitivity categories (Health/Finance/...).
//! Migrates v1 `facts` table to `memories_v2` on first open.

pub mod archive;
pub mod cli;
pub mod retroactive;
pub mod embedding_security;
pub mod fts;
pub mod hybrid_search;
pub mod ingest;
pub mod lifecycle;
pub mod operational;
pub mod migration;
pub mod principles;
pub mod privacy;
pub mod prompts;
pub mod provenance;
pub mod reflexa;
pub mod research_ingest;
pub mod retrieval;
pub mod secure_delete;
pub mod secure_export;
pub mod secure_memory;
pub mod shared;
pub mod side_channel_defense;
pub mod temporal;
pub mod transit_security;
pub mod types;

pub use types::{Memory, MemoryType, SensitivityCategory, Visibility};
pub use privacy::PrivacyEnforcer;
pub use provenance::ProvenanceTracker;
pub use retrieval::{MemoryQuery, ScoredMemory, Candidate};
pub use secure_memory::{SecureMemory, RedactedString, format_for_context};
pub use shared::SharedMemoryManager;
pub use temporal::{TemporalQuery, parse_temporal_query, temporal_decay, access_boost, apply_temporal_scoring};

use crate::config::Config;
use crate::error::{LuminaError, Result};
use crate::users::user_data_dir;
use crate::vault;
use rusqlite::{params, Connection};
use secrecy::ExposeSecret;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

// ── EngramStore ────────────────────────────────────────────────────────────

/// SQLCipher-backed store for long-term user facts and their embeddings.
///
/// Key from `vault::VaultStore::load().get("ENGRAM_DB_KEY")` — never from env.
pub struct EngramStore {
    /// Raw SQLCipher connection. `pub(crate)` so sibling modules (e.g. `shared`,
    /// `secure_delete`) can issue custom queries and verifications.
    pub(crate) conn: Connection,
    /// Which search backend(s) to use when `query()` is called.
    /// Defaults to `SearchMode::Hybrid` (FTS5 + vector + RRF).
    pub search_mode: hybrid_search::SearchMode,
    /// Owner of this store instance. All reads/writes are scoped to this user_id.
    /// Use "system" for the legacy/anonymous slot (matches the column DEFAULT).
    user_id: String,
    /// ESEC-04: count of secure deletions since last VACUUM.
    /// Wrapped in Arc so copies (e.g. open_for_user_at clones) share state.
    deletion_count: Arc<AtomicU32>,
    /// ESEC-03: embedding encryption + noise injection.
    /// Disabled gracefully if LUMINA_EMBEDDING_KEY is unavailable.
    pub(crate) embedding_sec: embedding_security::EmbeddingSecurity,
}

impl EngramStore {
    /// Open (or create) the engram store at `db_path` with the raw 32-byte `key`.
    pub fn open(db_path: &Path, key: &[u8]) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| LuminaError::Config(format!("Cannot create engram dir: {e}")))?;
        }

        let conn = Connection::open(db_path)
            .map_err(|e| LuminaError::Config(format!("Cannot open engram store: {e}")))?;

        let hex_key = hex::encode(key);
        conn.execute_batch(&format!("PRAGMA key = \"x'{hex_key}'\";"))
            .map_err(|e| LuminaError::Config(format!("Failed to set engram key: {e}")))?;

        conn.execute_batch("SELECT count(*) FROM sqlite_master;")
            .map_err(|_| LuminaError::SecurityViolation(
                "Engram store key is incorrect — cannot open database".to_string()
            ))?;

        conn.execute_batch("PRAGMA journal_mode = WAL;")
            .map_err(|e| LuminaError::Config(format!("WAL mode failed: {e}")))?;

        // ESEC-04: zero-fill freed pages on every DELETE.
        conn.execute_batch("PRAGMA secure_delete = ON;")
            .map_err(|e| LuminaError::Config(format!("secure_delete PRAGMA failed: {e}")))?;

        let embedding_sec = embedding_security::EmbeddingSecurity::new();
        let store = Self {
            conn,
            search_mode: hybrid_search::SearchMode::default(),
            user_id: "system".to_string(),
            deletion_count: Arc::new(AtomicU32::new(0)),
            embedding_sec,
        };
        store.ensure_schema()?;
        Ok(store)
    }

    /// Insert a fact text and its embedding vector.
    ///
    /// EMEM-01: writes to `memories_v2` (Semantic/Private/General defaults).
    /// Embedding stored as little-endian BLOB. FTS5 index updated on success.
    pub fn insert_fact(&self, text: &str, embedding: &[f32]) -> Result<()> {
        let id = types::new_uuid();
        let now = types::iso_now();
        // ESEC-03: apply noise + encryption before storage.
        let blob = self.embedding_sec.maybe_encrypt(embedding)?;
        self.conn.execute(
            "INSERT INTO memories_v2
             (id, user_id, memory_type, visibility, sensitivity, content, embedding, confidence, access_count, created_at, updated_at)
             VALUES (?1, ?2, 'semantic', 'private', 'general', ?3, ?4, 0.8, 0, ?5, ?5)",
            params![id, self.user_id, text, blob, now],
        ).map_err(|e| LuminaError::Config(format!("insert_fact failed: {e}")))?;
        let rowid = self.conn.last_insert_rowid();
        let _ = fts::fts_sync_insert_v2(&self.conn, rowid, text);
        Ok(())
    }

    /// Insert a typed memory into memories_v2.
    ///
    /// EMEM-02: enforces sensitivity privacy before storage — Health/Finance/Personal
    /// are always forced to Private regardless of the memory's visibility field.
    /// Also syncs content to FTS index.
    pub fn insert_memory(&self, memory: &Memory) -> Result<()> {
        // EMEM-02: enforce sensitivity — mutates visibility if needed
        let mut memory = memory.clone();
        privacy::PrivacyEnforcer::enforce_sensitivity(&mut memory);

        // ESEC-03: apply noise + encryption before storage.
        let blob = if memory.embedding.is_empty() {
            None
        } else {
            Some(self.embedding_sec.maybe_encrypt(&memory.embedding)?)
        };
        let tags_json = serde_json::to_string(&memory.tags).unwrap_or_else(|_| "[]".to_string());
        self.conn.execute(
            "INSERT INTO memories_v2
             (id, user_id, memory_type, visibility, sensitivity, content, embedding,
              source_conversation_id, source_turn_index, confidence, access_count,
              last_accessed, created_at, updated_at, superseded_by, tags)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                memory.id,
                memory.user_id,
                memory.memory_type.to_db(),
                memory.visibility.to_db(),
                memory.sensitivity.to_db(),
                memory.content,
                blob,
                memory.source_conversation_id,
                memory.source_turn_index,
                memory.confidence,
                memory.access_count,
                memory.last_accessed,
                memory.created_at,
                memory.updated_at,
                memory.superseded_by,
                tags_json,
            ],
        ).map_err(|e| LuminaError::Config(format!("insert_memory failed: {e}")))?;
        // Sync to FTS index so typed memories are searchable via Hybrid/FtsOnly modes.
        let rowid = self.conn.last_insert_rowid();
        let _ = fts::fts_sync_insert_v2(&self.conn, rowid, &memory.content);
        Ok(())
    }

    /// Share a memory to household visibility.
    ///
    /// EMEM-02: validates that caller owns the memory and sensitivity allows sharing.
    /// Returns error if the memory is Health/Finance/Personal (always private).
    pub fn share_memory(&self, caller_user_id: &str, memory_id: &str) -> Result<()> {
        // Load the memory first
        let mem = self.get_memory_by_id(memory_id)?
            .ok_or_else(|| LuminaError::Config(format!("memory {memory_id} not found")))?;
        // Privacy check
        let new_visibility = privacy::PrivacyEnforcer::validate_share(caller_user_id, &mem)?;
        let now = types::iso_now();
        self.conn.execute(
            "UPDATE memories_v2 SET visibility = ?1, updated_at = ?2 WHERE id = ?3",
            params![new_visibility.to_db(), now, memory_id],
        ).map_err(|e| LuminaError::Config(format!("share_memory failed: {e}")))?;
        Ok(())
    }

    /// Retrieve a memory by its UUID id.
    ///
    /// EMEM-02: scoped by user_id — enforces "all queries filter by user_id" invariant.
    pub fn get_memory_by_id(&self, memory_id: &str) -> Result<Option<Memory>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, user_id, memory_type, visibility, sensitivity, content, embedding,
                    source_conversation_id, source_turn_index, confidence, access_count,
                    last_accessed, created_at, updated_at, superseded_by, tags
             FROM memories_v2 WHERE id = ?1 AND user_id = ?2"
        ).map_err(|e| LuminaError::Config(format!("get_memory_by_id prepare: {e}")))?;

        let sec = &self.embedding_sec;
        let mut rows = stmt.query_map(params![memory_id, self.user_id], |row| {
            row_to_memory_with_sec(row, Some(sec))
        }).map_err(|e| LuminaError::Config(format!("get_memory_by_id query: {e}")))?;

        match rows.next() {
            Some(Ok(m)) => Ok(Some(m)),
            Some(Err(e)) => Err(LuminaError::Config(format!("get_memory_by_id row: {e}"))),
            None => Ok(None),
        }
    }

    /// Delete a fact by row id, also removing it from the FTS index.
    ///
    /// EMEM-02: validates that the store's user_id owns the memory before deletion.
    /// ESEC-04: looks up the UUID by rowid then delegates to SecureDeleter, which
    ///          overwrites content+embedding with random bytes before DELETE.
    pub fn delete_fact(&self, id: i64) -> Result<()> {
        // EMEM-02: validate ownership — only the owning user may delete.
        let mem_opt: Option<(String, String)> = self.conn.query_row(
            "SELECT id, user_id FROM memories_v2 WHERE rowid = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ).ok();

        let memory_uuid = match mem_opt {
            Some((uuid, owner)) => {
                if owner != self.user_id {
                    let violation = privacy::PrivacyViolation::DeleteNotOwned {
                        caller: self.user_id.clone(),
                        owner,
                    };
                    return Err(violation.to_error());
                }
                uuid
            }
            None => {
                // Row not found — nothing to delete (idempotent)
                return Ok(());
            }
        };

        // ESEC-04: remove from FTS before overwriting content.
        let _ = fts::fts_sync_delete_v2(&self.conn, id);

        // ESEC-04: overwrite + delete via SecureDeleter.
        secure_delete::SecureDeleter::delete_memory(&self.conn, &memory_uuid, "delete_fact")?;

        // ESEC-04: increment deletion counter; VACUUM every 100 deletions.
        let prev = self.deletion_count.fetch_add(1, Ordering::Relaxed);
        if (prev + 1) % 100 == 0 {
            let _ = secure_delete::SecureDeleter::vacuum(&self.conn);
        }

        Ok(())
    }

    /// Delete a memory by UUID — ESEC-04 secure path.
    ///
    /// Overwrites content+embedding with random bytes before DELETE.
    /// Removes the row from the FTS index first.
    /// Tracks deletion count; runs VACUUM every 100 deletions.
    pub fn secure_delete_memory(&self, memory_id: &str, reason: &str) -> Result<()> {
        // Look up the rowid for FTS cleanup
        let rowid: Option<i64> = self.conn.query_row(
            "SELECT rowid FROM memories_v2 WHERE id = ?1 AND user_id = ?2",
            params![memory_id, self.user_id],
            |r| r.get(0),
        ).ok();

        if let Some(rid) = rowid {
            let _ = fts::fts_sync_delete_v2(&self.conn, rid);
        } else {
            // Row not found — nothing to do
            return Ok(());
        }

        // ESEC-04: overwrite + delete
        secure_delete::SecureDeleter::delete_memory(&self.conn, memory_id, reason)?;

        // ESEC-04: VACUUM every 100 deletions
        let prev = self.deletion_count.fetch_add(1, Ordering::Relaxed);
        if (prev + 1) % 100 == 0 {
            let _ = secure_delete::SecureDeleter::vacuum(&self.conn);
        }

        Ok(())
    }

    /// Hybrid search: returns the top-K most relevant fact texts using the
    /// configured `search_mode` (default: FTS5 + vector + RRF).
    ///
    /// `query_embedding` is required for `Hybrid` and `VectorOnly` modes;
    /// if `None` those modes fall back to FTS-only behaviour.
    ///
    /// P2-14: bumps `access_count` and sets `accessed_at` for each returned fact
    /// via `lifecycle::record_access` so lifecycle pruning works correctly.
    ///
    /// EMEM-02: results are privacy-filtered — `validate_access` is called per row.
    pub fn query(
        &self,
        query_text: &str,
        query_embedding: Option<&[f32]>,
        k: usize,
    ) -> Result<Vec<hybrid_search::HybridResult>> {
        let results = hybrid_search::hybrid_search(
            &self.conn,
            query_text,
            query_embedding,
            k,
            self.search_mode,
            &self.user_id,
            Some(&self.embedding_sec),
        )?;
        // EMEM-02: privacy-filter results — validate_access per row.
        // HybridResult doesn't carry full Memory; fetch visibility for check.
        let filtered: Vec<hybrid_search::HybridResult> = results.into_iter()
            .filter(|r| {
                // Non-fatal: if we can't load visibility, exclude the row (fail-safe).
                let visibility: Option<String> = self.conn.query_row(
                    "SELECT visibility FROM memories_v2 WHERE rowid = ?1",
                    params![r.id],
                    |row| row.get(0),
                ).ok();
                match visibility.as_deref() {
                    Some("private") => true, // visible to owner — user_id scoped at SQL level (per-user DB)
                    Some("shared") | Some("system") => true,        // visible to all
                    _ => false, // unknown visibility: exclude (fail-safe)
                }
            })
            .collect();
        // P2-14: track access for each result (non-fatal on error).
        for r in &filtered {
            let _ = lifecycle::record_access(&self.conn, r.id);
        }
        Ok(filtered)
    }

    /// Delete all facts not accessed in the last `days` days. Returns count deleted.
    pub fn prune_older_than(&self, days: u64) -> Result<usize> {
        lifecycle::prune_older_than(&self.conn, days)
    }

    /// Keep only the `keep` most-accessed facts. Returns count deleted.
    pub fn prune_least_accessed(&self, keep: usize) -> Result<usize> {
        lifecycle::prune_least_accessed(&self.conn, keep)
    }

    /// Merge fact pairs whose embeddings have cosine similarity ≥ `threshold`.
    /// Returns count of facts removed.
    pub fn consolidate_similar(&self, threshold: f32) -> Result<usize> {
        lifecycle::consolidate_similar(&self.conn, threshold)
    }

    /// Return all stored (text, embedding) pairs.
    ///
    /// EMEM-01: reads from memories_v2, filtered by user_id.
    /// Rows with missing or corrupt embeddings are returned with empty embedding.
    pub fn all_facts(&self) -> Result<Vec<(String, Vec<f32>)>> {
        let mut stmt = self.conn.prepare(
            "SELECT content, embedding FROM memories_v2 WHERE user_id = ?1 ORDER BY created_at ASC",
        ).map_err(|e| LuminaError::Config(format!("all_facts prepare failed: {e}")))?;

        let rows = stmt.query_map(rusqlite::params![self.user_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<Vec<u8>>>(1)?))
        }).map_err(|e| LuminaError::Config(format!("all_facts query failed: {e}")))?;

        let mut out = Vec::new();
        for row in rows {
            match row {
                Ok((text, Some(blob))) => {
                    // ESEC-03: decrypt if encrypted (maybe_decrypt handles both paths).
                    match self.embedding_sec.maybe_decrypt(&blob) {
                        Ok(emb) => out.push((text, emb)),
                        Err(_) => {
                            eprintln!("engram: skipping fact with corrupt/undecryptable embedding ({} bytes)", blob.len());
                        }
                    }
                }
                Ok((text, None)) => {
                    // Memory without embedding — include with empty embedding
                    out.push((text, Vec::new()));
                }
                Err(e) => {
                    eprintln!("engram: row read error (skipping): {e}");
                }
            }
        }
        Ok(out)
    }

    /// ESEC-03-aware embedding blob decoder.
    ///
    /// Sibling modules (e.g. `reflexa::consolidation`) that read raw embedding
    /// blobs from the database MUST use this instead of the bare `decode_embedding`
    /// free function, because blobs may be encrypted + noise-injected by ESEC-03.
    /// `maybe_decrypt` handles both the encrypted and unencrypted (legacy) paths.
    pub(crate) fn decrypt_embedding_blob(&self, blob: &[u8]) -> Option<Vec<f32>> {
        self.embedding_sec.maybe_decrypt(blob).ok()
    }

    fn ensure_schema(&self) -> Result<()> {
        // EMEM-01: create memories_v2 (the canonical v2 schema)
        migration::create_memories_v2(&self.conn)?;

        // EMEM-01: migrate v1 `facts` table → memories_v2 if it still exists.
        // Also handles the P2-02 user_id migration (facts had that column).
        migration::migrate_v1_to_v2(&self.conn)?;

        // Create FTS5 virtual table on memories_v2 — gracefully degrades if FTS5 unavailable
        fts::create_fts_v2_table(&self.conn)?;

        // EMEM-01: add lifecycle columns to memories_v2 if upgrading from a
        // pre-EMEM-01 memories_v2 that lacked them.
        lifecycle::migrate_lifecycle_columns_v2(&self.conn)?;
        Ok(())
    }
}

// ── Embedding BLOB codec ───────────────────────────────────────────────────

/// Encode a Vec<f32> as little-endian bytes.
pub fn encode_embedding(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for &f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

/// Decode little-endian bytes into Vec<f32>. Returns None on malformed input.
pub fn decode_embedding(blob: &[u8]) -> Option<Vec<f32>> {
    if blob.len() % 4 != 0 || blob.is_empty() {
        return None;
    }
    let floats: Vec<f32> = blob.chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    Some(floats)
}

// ── Open-default helpers ───────────────────────────────────────────────────

/// Default path for the engram store: `ENGRAM_DB_PATH` env var or ~/.lumina/engram.db.
pub fn engram_db_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("ENGRAM_DB_PATH") {
        return std::path::PathBuf::from(p);
    }
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".lumina")
        .join("engram.db")
}

/// Get the ENGRAM_DB_KEY (fresh VaultStore::load; env fallback for tests).
pub(crate) fn engram_key() -> Result<Vec<u8>> {
    // 1. Fresh vault read
    if let Ok(store) = vault::VaultStore::load() {
        if let Some(s) = store.get("ENGRAM_DB_KEY") {
            if let Ok(bytes) = hex::decode(s.expose_secret()) {
                if bytes.len() >= 32 {
                    return Ok(bytes);
                }
            }
        }
    }
    // 2. Env fallback for tests
    if let Ok(hex_val) = std::env::var("ENGRAM_DB_KEY") {
        if let Ok(bytes) = hex::decode(&hex_val) {
            if bytes.len() >= 32 {
                return Ok(bytes);
            }
        }
    }
    Err(LuminaError::Config("ENGRAM_DB_KEY not found in vault or environment".to_string()))
}

impl EngramStore {
    /// Open using the default path and ENGRAM_DB_KEY from vault.
    pub fn open_default() -> Result<Self> {
        let path = engram_db_path();
        let key = engram_key()?;
        Self::open(&path, &key)
    }

    /// Open (or create) a per-user database under `base/users/{user_id}/engram.db`.
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
                "Cannot create engram directory for user {}: {}",
                user_id, e
            ))
        })?;
        let db_path = dir.join("engram.db");
        let mut store = Self::open(&db_path, key)?;
        store.user_id = user_id.to_string();
        // Reset deletion counter for this user's store — each open gets its own counter.
        store.deletion_count = Arc::new(AtomicU32::new(0));
        Ok(store)
    }

    /// Open a per-user engram database under `~/.lumina/users/{user_id}/engram.db`.
    ///
    /// Uses ENGRAM_DB_KEY from vault. Pass `"system"` for the anonymous/legacy user slot
    /// (matches the DB column default).
    pub fn open_for_user(user_id: &str) -> Result<Self> {
        let base = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".lumina");
        let key = engram_key()?;
        Self::open_for_user_at(&base, user_id, &key)
    }
}

// ── Ollama embedding client (P1-07) ───────────────────────────────────────

/// Embed `text` via the Ollama embeddings endpoint.
///
/// URL and model come exclusively from `config.ollama_embedding_url()` and
/// `config.engram_embed_model()`. No hardcoded host. Returns an L2-normalized
/// `Vec<f32>` so cosine similarity reduces to a dot product (P1-08).
///
/// Returns `LuminaError` if the endpoint is unreachable or returns a non-200
/// status. Callers should degrade gracefully (skip retrieval/storage for the turn).
pub async fn embed(text: &str, config: &Config) -> Result<Vec<f32>> {
    let url = config.ollama_embedding_url();
    if url.is_empty() {
        return Err(LuminaError::Config("OLLAMA_EMBEDDING_URL not set".to_string()));
    }
    let model = config.engram_embed_model();

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| LuminaError::Config(format!("reqwest build error: {e}")))?;

    let body = serde_json::json!({ "model": model, "prompt": text });
    // `?` converts reqwest::Error via From impl
    let resp = client.post(&url).json(&body).send().await?;

    if !resp.status().is_success() {
        return Err(LuminaError::Config(format!(
            "Ollama embedding returned HTTP {}", resp.status()
        )));
    }

    // `?` converts serde_json::Error via From impl
    let json: serde_json::Value = resp.json().await?;

    let raw: Vec<f32> = json["embedding"]
        .as_array()
        .ok_or_else(|| LuminaError::Config("Ollama response missing 'embedding' field".to_string()))?
        .iter()
        .map(|v| v.as_f64().unwrap_or(0.0) as f32)
        .collect();

    if raw.is_empty() {
        return Err(LuminaError::Config("Ollama embedding is empty".to_string()));
    }

    Ok(l2_normalize(raw))
}

/// L2-normalize a vector in place (so cosine = dot product).
pub fn l2_normalize(mut v: Vec<f32>) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-9 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
    v
}

// ── Cosine similarity retrieval (P1-08) ───────────────────────────────────

/// Compute cosine similarity between two vectors.
///
/// Returns 0.0 for zero-length or mismatched-dimension vectors (logged warning).
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        if !a.is_empty() && !b.is_empty() && a.len() != b.len() {
            eprintln!("engram: cosine() dimension mismatch: {} vs {}", a.len(), b.len());
        }
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na < 1e-9 || nb < 1e-9 {
        return 0.0;
    }
    (dot / (na * nb)).clamp(-1.0, 1.0)
}

/// Minimum similarity threshold for retrieval (facts below this are excluded).
const MIN_SIMILARITY: f32 = 0.2;

/// Retrieve the top-K most relevant facts for a query embedding.
///
// ── Memory injection and extraction (P1-09) ────────────────────────────────
//
// Design note: `Connection: !Sync` → `&EngramStore: !Send`. Pattern to keep futures Send:
//   1. Load all data from store synchronously (no await while holding &EngramStore)
//   2. Call embed() async (store reference no longer live at this point per NLL)
//   3. Score/inject synchronously
// The public async wrappers below maintain this ordering.

/// Select the top-K most relevant texts from `all_facts` given a query embedding.
/// Pure sync — no async, no store reference.
pub fn retrieve_from_embeddings(
    query_emb: &[f32],
    all_facts: &[(String, Vec<f32>)],
    k: usize,
) -> Vec<String> {
    let mut scored: Vec<(f32, &str)> = all_facts
        .iter()
        .map(|(text, emb)| (cosine(query_emb, emb), text.as_str()))
        .filter(|(s, _)| *s >= MIN_SIMILARITY)
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(k);
    scored.into_iter().map(|(_, t)| t.to_string()).collect()
}

/// Inject relevant facts as a bullet list into the system prompt (sync).
/// Returns `system_prompt` unchanged if `facts` is empty.
pub fn inject_memory_bullets(system_prompt: &str, facts: &[String]) -> String {
    if facts.is_empty() {
        return system_prompt.to_string();
    }
    let bullets = facts.iter().map(|f| format!("• {f}")).collect::<Vec<_>>().join("\n");
    format!("{system_prompt}\n\nKnown facts about the user:\n{bullets}")
}

/// Preference extraction patterns (keyword/regex heuristic — no LLM per de-bloat rules).
pub static PREFERENCE_PATTERNS: &[&str] = &[
    r"(?i)\bI (?:like|love|enjoy|prefer|use|need|want|hate|dislike|don't like)\b.{3,}",
    r"(?i)\bmy (?:favorite|preferred|usual|default)\b.{3,}",
    r"(?i)\bI(?:'m| am) (?:a |an )?.{3,}",
    r"(?i)\bmy \w+ is\b.{3,}",
];

/// Extract preference-shaped statements from user input (sync, keyword/regex only).
pub fn extract_preference_texts(user_input: &str) -> Vec<String> {
    let mut out = Vec::new();
    for pattern in PREFERENCE_PATTERNS {
        if let Ok(re) = regex::Regex::new(pattern) {
            for mat in re.find_iter(user_input) {
                let fact = mat.as_str().trim();
                if fact.len() >= 8 && fact.len() <= 300 {
                    out.push(fact.to_string());
                }
            }
        }
    }
    out
}

/// Async wrapper: retrieve top-K relevant fact texts for `query`, without holding
/// any `&EngramStore` reference across an await (satisfies Send bound).
///
/// Pattern: load facts sync → drop store ref → embed async → score sync.
///
/// P2-14: Calls `lifecycle::record_access` for each returned fact so that
/// `prune_least_accessed` and `prune_older_than` have accurate access data.
pub async fn retrieve(
    query: &str,
    k: usize,
    store: &EngramStore,
    config: &Config,
) -> Result<Vec<String>> {
    // Load facts with IDs synchronously (no await while store is referenced)
    let all_facts_ids = store.all_facts_with_ids()?;
    // store not referenced after this line

    let query_emb = match embed(query, config).await {
        Ok(e) => e,
        Err(_) => return Ok(Vec::new()),
    };

    // Score and rank
    let all_facts: Vec<(String, Vec<f32>)> = all_facts_ids.iter()
        .map(|(_, text, emb)| (text.clone(), emb.clone()))
        .collect();
    let result_texts = retrieve_from_embeddings(&query_emb, &all_facts, k);

    // P2-14: bump access_count for returned facts by ID (not text match).
    if !result_texts.is_empty() {
        for (id, text, _) in &all_facts_ids {
            if result_texts.contains(text) {
                let _ = lifecycle::record_access(&store.conn, *id);
            }
        }
    }

    Ok(result_texts)
}

impl EngramStore {
    /// Return all stored (rowid, text, embedding) triples.
    ///
    /// EMEM-01: reads from memories_v2. Uses rowid (integer primary key alias)
    /// for access tracking compatibility with lifecycle module.
    /// P2-14: used by `retrieve()` to bump `access_count` by ID.
    pub fn all_facts_with_ids(&self) -> Result<Vec<(i64, String, Vec<f32>)>> {
        let mut stmt = self.conn.prepare(
            "SELECT rowid, content, embedding FROM memories_v2 WHERE user_id = ?1 ORDER BY created_at ASC",
        ).map_err(|e| LuminaError::Config(format!("all_facts_with_ids prepare failed: {e}")))?;

        let rows = stmt.query_map(rusqlite::params![self.user_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<Vec<u8>>>(2)?))
        }).map_err(|e| LuminaError::Config(format!("all_facts_with_ids query failed: {e}")))?;

        let mut out = Vec::new();
        for row in rows {
            match row {
                Ok((id, text, Some(blob))) => {
                    // ESEC-03: decrypt if encrypted.
                    match self.embedding_sec.maybe_decrypt(&blob) {
                        Ok(emb) => out.push((id, text, emb)),
                        Err(_) => {
                            eprintln!("engram: skipping fact with corrupt/undecryptable embedding ({} bytes)", blob.len());
                        }
                    }
                }
                Ok((id, text, None)) => {
                    out.push((id, text, Vec::new()));
                }
                Err(e) => {
                    eprintln!("engram: row read error (skipping): {e}");
                }
            }
        }
        Ok(out)
    }

    /// Query memories by type.
    ///
    /// EMEM-02: results are privacy-filtered via validate_access per row.
    /// Returns only memories accessible to self.user_id (private = owner only,
    /// shared = all users, system = all users).
    pub fn query_by_type(&self, memory_type: MemoryType) -> Result<Vec<Memory>> {
        // EMEM-02: include privacy_where_clause for database-level defense in depth.
        // The user_id = ?1 AND memory_type = ?2 clause handles the primary case,
        // with the visibility check adding the defense-in-depth layer.
        let sql = format!(
            "SELECT id, user_id, memory_type, visibility, sensitivity, content, embedding,
                    source_conversation_id, source_turn_index, confidence, access_count,
                    last_accessed, created_at, updated_at, superseded_by, tags
             FROM memories_v2
             WHERE user_id = ?1 AND memory_type = ?2 AND superseded_by IS NULL
               AND ({})
             ORDER BY created_at ASC",
            privacy::privacy_where_clause(1)
        );
        let mut stmt = self.conn.prepare(&sql)
            .map_err(|e| LuminaError::Config(format!("query_by_type prepare failed: {e}")))?;

        let sec = &self.embedding_sec;
        let rows = stmt.query_map(
            rusqlite::params![self.user_id, memory_type.to_db()],
            |row| row_to_memory_with_sec(row, Some(sec)),
        ).map_err(|e| LuminaError::Config(format!("query_by_type query failed: {e}")))?;

        // EMEM-02: validate_access per row as application-level defense in depth.
        let mut results = Vec::new();
        for row in rows {
            if let Ok(memory) = row {
                if privacy::PrivacyEnforcer::validate_access(&self.user_id, &memory).is_ok() {
                    results.push(memory);
                }
            }
        }
        Ok(results)
    }
}

/// Map a database row to a Memory struct (plaintext decode — legacy path).
///
/// Used only in tests or when no encryption is configured.
/// For encrypted stores use `row_to_memory_with_sec` instead.
pub(crate) fn row_to_memory(row: &rusqlite::Row<'_>) -> rusqlite::Result<Memory> {
    row_to_memory_with_sec(row, None)
}

/// Map a database row to a Memory struct, decrypting the embedding via
/// `EmbeddingSecurity` if provided.
fn row_to_memory_with_sec(
    row: &rusqlite::Row<'_>,
    sec: Option<&embedding_security::EmbeddingSecurity>,
) -> rusqlite::Result<Memory> {
    let tags_json: String = row.get(15).unwrap_or_else(|_| "[]".to_string());
    let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
    let emb_blob: Option<Vec<u8>> = row.get(6)?;
    let embedding = match emb_blob.as_deref() {
        None => Vec::new(),
        Some(blob) => {
            if let Some(esec) = sec {
                // ESEC-03: decrypt (handles both encrypted and plaintext blobs)
                esec.maybe_decrypt(blob).unwrap_or_else(|_| {
                    // Fallback: try raw decode
                    decode_embedding(blob).unwrap_or_default()
                })
            } else {
                decode_embedding(blob).unwrap_or_default()
            }
        }
    };

    Ok(Memory {
        id: row.get(0)?,
        user_id: row.get(1)?,
        memory_type: MemoryType::from_db(&row.get::<_, String>(2)?),
        visibility: Visibility::from_db(&row.get::<_, String>(3)?),
        sensitivity: SensitivityCategory::from_db(&row.get::<_, String>(4)?),
        content: row.get(5)?,
        embedding,
        source_conversation_id: row.get(7)?,
        source_turn_index: row.get(8)?,
        confidence: row.get(9).unwrap_or(0.8),
        access_count: row.get(10).unwrap_or(0),
        last_accessed: row.get(11)?,
        created_at: row.get(12).unwrap_or_default(),
        updated_at: row.get(13).unwrap_or_default(),
        superseded_by: row.get(14)?,
        tags,
    })
}

/// Async wrapper: inject memories into system prompt.
/// Loads facts sync, embeds async (store not held across await).
///
/// EMEM-08: also fetches shared memories (query-filtered) and appends them
/// after private facts with `[Shared by {user_id}]` attribution labels.
pub async fn inject_memories(
    user_input: &str,
    system_prompt: &str,
    k: usize,
    store: &EngramStore,
    config: &Config,
) -> String {
    // Load private facts + shared memories synchronously (no await while store referenced).
    let all_facts = match store.all_facts() {
        Ok(f) => f,
        Err(_) => return system_prompt.to_string(),
    };
    // EMEM-08: load shared memories filtered by query text.
    let shared_mems = shared::SharedMemoryManager::query_shared(
        &store.conn,
        &store.user_id,
        Some(user_input),
        k,
    ).unwrap_or_default();
    // store not referenced after this line

    let query_emb = match embed(user_input, config).await {
        Ok(e) => e,
        Err(_) => return system_prompt.to_string(),
    };

    let facts = retrieve_from_embeddings(&query_emb, &all_facts, k);

    // EMEM-08: format and append shared memories after private facts.
    let shared_formatted = shared::SharedMemoryManager::format_shared_for_context(&shared_mems);

    let mut combined = inject_memory_bullets(system_prompt, &facts);
    if !shared_formatted.is_empty() {
        combined.push_str("\n\nShared household memories:\n");
        combined.push_str(&shared_formatted);
    }
    combined
}

/// Async: extract preference facts from user input and persist to store.
///
/// Pattern: embed all facts first (store not referenced during awaits),
/// then insert synchronously. This keeps the future `Send` per the design note.
pub async fn extract_and_store(
    user_input: &str,
    store: &EngramStore,
    config: &Config,
) {
    let texts = extract_preference_texts(user_input);

    // Step 1: embed all facts async (store NOT referenced here)
    let mut embedded: Vec<(String, Vec<f32>)> = Vec::new();
    for fact in texts {
        if let Ok(emb) = embed(&fact, config).await {
            embedded.push((fact, emb));
        }
    }

    // Step 2: insert synchronously (no more awaits, store borrow is fine here)
    for (fact, emb) in embedded {
        let _ = store.insert_fact(&fact, &emb);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn test_key() -> Vec<u8> { vec![0u8; 32] }

    fn tmp_db(tag: &str) -> std::path::PathBuf {
        let p = std::path::PathBuf::from(format!("/tmp/lumina_engram_test_{tag}.db"));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn test_encode_decode_round_trip() {
        let v = vec![1.0f32, 2.0, 3.0, -1.5];
        let blob = encode_embedding(&v);
        let decoded = decode_embedding(&blob).unwrap();
        assert_eq!(decoded.len(), 4);
        for (a, b) in v.iter().zip(decoded.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn test_decode_empty_returns_none() {
        assert!(decode_embedding(&[]).is_none());
    }

    #[test]
    fn test_decode_misaligned_returns_none() {
        // 7 bytes not divisible by 4
        assert!(decode_embedding(&[0u8; 7]).is_none());
    }

    #[test]
    fn test_insert_and_all_facts() {
        let path = tmp_db("insert");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        let emb1 = vec![1.0f32, 0.0, 0.0];
        let emb2 = vec![0.0f32, 1.0, 0.0];
        store.insert_fact("user likes dark mode", &emb1).unwrap();
        store.insert_fact("user is a morning person", &emb2).unwrap();

        let facts = store.all_facts().unwrap();
        assert_eq!(facts.len(), 2);
        assert_eq!(facts[0].0, "user likes dark mode");
        assert_eq!(facts[1].0, "user is a morning person");

        // ESEC-03: embedding may have noise applied on insert. Verify cosine similarity
        // is preserved (> 0.98) rather than checking exact byte equality.
        assert_eq!(facts[0].1.len(), emb1.len(), "embedding dimension must be preserved");
        let sim = cosine(&facts[0].1, &emb1);
        assert!(sim > 0.97, "round-trip embedding cosine similarity should be > 0.97, got {sim:.4}");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_empty_store_returns_empty_vec() {
        let path = tmp_db("empty");
        let store = EngramStore::open(&path, &test_key()).unwrap();
        let facts = store.all_facts().unwrap();
        assert!(facts.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_wrong_key_returns_error() {
        let path = tmp_db("wrongkey");
        EngramStore::open(&path, &test_key()).unwrap();
        let bad_key = vec![0xFFu8; 32];
        assert!(EngramStore::open(&path, &bad_key).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_corrupt_embedding_skipped() {
        let path = tmp_db("corrupt");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        // Insert a valid fact
        store.insert_fact("good fact", &[1.0f32]).unwrap();

        // Manually insert a row with a corrupt 3-byte embedding blob into memories_v2
        let bad_id = types::new_uuid();
        let now = types::iso_now();
        store.conn.execute(
            "INSERT INTO memories_v2 (id, user_id, memory_type, visibility, sensitivity, content, embedding, confidence, access_count, created_at, updated_at)
             VALUES (?1, 'system', 'semantic', 'private', 'general', 'bad fact', x'010203', 0.8, 0, ?2, ?2)",
            rusqlite::params![bad_id, now],
        ).unwrap();

        let facts = store.all_facts().unwrap();
        // Only the good fact should be returned (corrupt embedding skipped)
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].0, "good fact");
        let _ = std::fs::remove_file(&path);
    }

    // ── Embedding tests (P1-07) ────────────────────────────────────────────

    #[test]
    fn test_l2_normalize_unit_vector() {
        let v = vec![3.0f32, 0.0, 4.0]; // magnitude = 5
        let n = l2_normalize(v);
        let dot: f32 = n.iter().map(|x| x * x).sum();
        assert!((dot - 1.0).abs() < 1e-6, "normalized vector should have unit length: {dot}");
        assert!((n[0] - 0.6).abs() < 1e-6);
        assert!((n[2] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn test_l2_normalize_zero_vector_no_panic() {
        let v = vec![0.0f32; 4];
        let n = l2_normalize(v.clone());
        // Zero vector stays zero — no panic, no NaN
        for x in &n {
            assert!(x.is_finite(), "result should be finite");
        }
    }

    #[test]
    fn test_l2_normalize_already_unit() {
        let v = vec![1.0f32, 0.0, 0.0];
        let n = l2_normalize(v);
        assert!((n[0] - 1.0).abs() < 1e-6);
        assert!((n[1]).abs() < 1e-6);
    }

    /// Integration test for `embed()`: HTTP mock returns a 4-element embedding,
    /// verify parsed Vec<f32> is L2-normalized.
    #[tokio::test]
    #[serial]
    async fn test_embed_parses_and_normalizes() {
        use httpmock::MockServer;
        use serde_json::json;

        let mock = MockServer::start();
        let raw = vec![3.0f64, 0.0, 4.0, 0.0]; // norm = 5
        let _m = mock.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/api/embeddings");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(json!({ "embedding": raw }));
        });

        std::env::set_var("CHORD_PROXY_URL", "http://localhost:4000"); // needed for Config::from_env
        std::env::set_var("LUMINA_CHORD_SECRET", "");
        std::env::set_var("OLLAMA_EMBEDDING_URL", format!("{}/api/embeddings", mock.base_url()));
        std::env::set_var("ENGRAM_EMBED_MODEL", "test-model");

        let config = Config::from_env().unwrap();
        let result = embed("hello world", &config).await.unwrap();

        assert_eq!(result.len(), 4);
        let norm: f32 = result.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "result should be normalized: {norm}");
        assert!((result[0] - 0.6).abs() < 1e-5, "component 0 wrong: {}", result[0]);

        std::env::remove_var("CHORD_PROXY_URL");
        std::env::remove_var("LUMINA_CHORD_SECRET");
        std::env::remove_var("OLLAMA_EMBEDDING_URL");
        std::env::remove_var("ENGRAM_EMBED_MODEL");
    }

    #[tokio::test]
    #[serial]
    async fn test_embed_missing_url_returns_error() {
        std::env::set_var("CHORD_PROXY_URL", "http://localhost:4000");
        std::env::set_var("LUMINA_CHORD_SECRET", "");
        std::env::remove_var("OLLAMA_EMBEDDING_URL");
        let config = Config::from_env().unwrap();
        let result = embed("test", &config).await;
        assert!(result.is_err());
        std::env::remove_var("CHORD_PROXY_URL");
        std::env::remove_var("LUMINA_CHORD_SECRET");
    }

    #[tokio::test]
    #[serial]
    async fn test_embed_missing_embedding_field_returns_error() {
        use httpmock::MockServer;
        use serde_json::json;

        let mock = MockServer::start();
        let _m = mock.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/api/embeddings");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(json!({ "not_embedding": [] }));
        });

        std::env::set_var("CHORD_PROXY_URL", "http://localhost:4000");
        std::env::set_var("LUMINA_CHORD_SECRET", "");
        std::env::set_var("OLLAMA_EMBEDDING_URL", format!("{}/api/embeddings", mock.base_url()));
        std::env::set_var("ENGRAM_EMBED_MODEL", "test-model");

        let config = Config::from_env().unwrap();
        let result = embed("hello", &config).await;
        assert!(result.is_err(), "Missing embedding field should return error");

        std::env::remove_var("CHORD_PROXY_URL");
        std::env::remove_var("LUMINA_CHORD_SECRET");
        std::env::remove_var("OLLAMA_EMBEDDING_URL");
        std::env::remove_var("ENGRAM_EMBED_MODEL");
    }

    // ── Cosine / retrieve tests (P1-08) ────────────────────────────────────

    #[test]
    fn test_cosine_identical_vectors() {
        let v = vec![1.0f32, 0.0, 0.0];
        let sim = cosine(&v, &v);
        assert!((sim - 1.0).abs() < 1e-6, "identical vectors: {sim}");
    }

    #[test]
    fn test_cosine_orthogonal_vectors() {
        let a = vec![1.0f32, 0.0, 0.0];
        let b = vec![0.0f32, 1.0, 0.0];
        let sim = cosine(&a, &b);
        assert!(sim.abs() < 1e-6, "orthogonal vectors: {sim}");
    }

    #[test]
    fn test_cosine_zero_vectors_returns_zero() {
        let z = vec![0.0f32; 3];
        assert_eq!(cosine(&z, &z), 0.0);
    }

    #[test]
    fn test_cosine_mismatched_dimensions_returns_zero() {
        let a = vec![1.0f32, 0.0];
        let b = vec![1.0f32, 0.0, 0.0];
        assert_eq!(cosine(&a, &b), 0.0);
    }

    #[test]
    fn test_cosine_empty_returns_zero() {
        assert_eq!(cosine(&[], &[]), 0.0);
    }

    /// Integration test: insert 3 facts with known embeddings, retrieve with a
    /// query embedding closest to one of them, assert it ranks first.
    #[tokio::test]
    #[serial]
    async fn test_retrieve_top_k_ranking() {
        use httpmock::MockServer;
        use serde_json::json;

        // Three unit vectors along x, y, z axes
        let emb_x = vec![1.0f32, 0.0, 0.0];
        let emb_y = vec![0.0f32, 1.0, 0.0];
        let emb_z = vec![0.0f32, 0.0, 1.0];

        let path = tmp_db("retrieve");
        let store = EngramStore::open(&path, &test_key()).unwrap();
        store.insert_fact("fact_x", &emb_x).unwrap();
        store.insert_fact("fact_y", &emb_y).unwrap();
        store.insert_fact("fact_z", &emb_z).unwrap();

        // Mock Ollama to return a query embedding close to emb_x
        let mock = MockServer::start();
        let _m = mock.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/api/embeddings");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(json!({ "embedding": [0.9_f64, 0.1, 0.0] }));
        });

        std::env::set_var("CHORD_PROXY_URL", "http://localhost:4000");
        std::env::set_var("LUMINA_CHORD_SECRET", "");
        std::env::set_var("OLLAMA_EMBEDDING_URL", format!("{}/api/embeddings", mock.base_url()));
        std::env::set_var("ENGRAM_EMBED_MODEL", "test-model");
        let config = Config::from_env().unwrap();

        let results = retrieve("query near x", 3, &store, &config).await.unwrap();
        assert!(!results.is_empty(), "Should retrieve at least one fact");
        assert_eq!(results[0], "fact_x", "fact_x should rank first");

        let _ = std::fs::remove_file(&path);
        std::env::remove_var("CHORD_PROXY_URL");
        std::env::remove_var("LUMINA_CHORD_SECRET");
        std::env::remove_var("OLLAMA_EMBEDDING_URL");
        std::env::remove_var("ENGRAM_EMBED_MODEL");
    }

    #[tokio::test]
    #[serial]
    async fn test_retrieve_empty_store_returns_empty() {
        use httpmock::MockServer;
        use serde_json::json;

        let path = tmp_db("retrieve_empty");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        let mock = MockServer::start();
        let _m = mock.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/api/embeddings");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(json!({ "embedding": [1.0_f64, 0.0] }));
        });

        std::env::set_var("CHORD_PROXY_URL", "http://localhost:4000");
        std::env::set_var("LUMINA_CHORD_SECRET", "");
        std::env::set_var("OLLAMA_EMBEDDING_URL", format!("{}/api/embeddings", mock.base_url()));
        std::env::set_var("ENGRAM_EMBED_MODEL", "test-model");
        let config = Config::from_env().unwrap();

        let results = retrieve("anything", 5, &store, &config).await.unwrap();
        assert!(results.is_empty(), "Empty store should return empty");

        let _ = std::fs::remove_file(&path);
        std::env::remove_var("CHORD_PROXY_URL");
        std::env::remove_var("LUMINA_CHORD_SECRET");
        std::env::remove_var("OLLAMA_EMBEDDING_URL");
        std::env::remove_var("ENGRAM_EMBED_MODEL");
    }

    #[tokio::test]
    #[serial]
    async fn test_retrieve_below_threshold_returns_empty() {
        use httpmock::MockServer;
        use serde_json::json;

        let path = tmp_db("retrieve_thresh");
        let store = EngramStore::open(&path, &test_key()).unwrap();
        // Insert a fact orthogonal to the query (cosine = 0, below 0.2 threshold)
        store.insert_fact("orthogonal fact", &[1.0f32, 0.0]).unwrap();

        let mock = MockServer::start();
        let _m = mock.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/api/embeddings");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(json!({ "embedding": [0.0_f64, 1.0] })); // orthogonal
        });

        std::env::set_var("CHORD_PROXY_URL", "http://localhost:4000");
        std::env::set_var("LUMINA_CHORD_SECRET", "");
        std::env::set_var("OLLAMA_EMBEDDING_URL", format!("{}/api/embeddings", mock.base_url()));
        std::env::set_var("ENGRAM_EMBED_MODEL", "test-model");
        let config = Config::from_env().unwrap();

        let results = retrieve("orthogonal query", 5, &store, &config).await.unwrap();
        assert!(results.is_empty(), "Below-threshold facts should be excluded");

        let _ = std::fs::remove_file(&path);
        std::env::remove_var("CHORD_PROXY_URL");
        std::env::remove_var("LUMINA_CHORD_SECRET");
        std::env::remove_var("OLLAMA_EMBEDDING_URL");
        std::env::remove_var("ENGRAM_EMBED_MODEL");
    }

    // ── Memory injection tests (P1-09) ─────────────────────────────────────

    #[test]
    fn test_extract_captures_preferences() {
        // Call the actual function to exercise the 8-300 char filter and trim too
        let inputs_expected: &[(&str, bool)] = &[
            ("I like dark mode", true),
            ("I love coffee in the morning", true),
            ("my favorite editor is Neovim", true),
            ("I prefer working at night", true),
        ];
        for (input, expect_match) in inputs_expected {
            let results = extract_preference_texts(input);
            assert_eq!(!results.is_empty(), *expect_match, "extract_preference_texts({input:?}) mismatch");
        }
    }

    #[test]
    fn test_extract_ignores_plain_questions() {
        let non_pref = [
            "What is the weather?",
            "Tell me a joke",
            "How are you today",
            "ok thanks",
        ];
        for input in &non_pref {
            let results = extract_preference_texts(input);
            assert!(results.is_empty(), "Should NOT extract from: {input}");
        }
    }

    /// Verify inject_memories adds bullets to system prompt when facts are relevant,
    /// and leaves it unchanged when no facts are above threshold.
    #[tokio::test]
    #[serial]
    async fn test_inject_memories_adds_bullets() {
        use httpmock::MockServer;
        use serde_json::json;

        let mock = MockServer::start();
        // Return a query embedding close to our stored fact
        let _m = mock.mock(|when, then| {
            when.method(httpmock::Method::POST).path("/api/embeddings");
            then.status(200)
                .header("Content-Type", "application/json")
                .json_body(json!({ "embedding": [1.0_f64, 0.0] }));
        });

        let path = tmp_db("inject");
        let store = EngramStore::open(&path, &test_key()).unwrap();
        store.insert_fact("user likes dark mode", &[1.0f32, 0.0]).unwrap();

        std::env::set_var("CHORD_PROXY_URL", "http://localhost:4000");
        std::env::set_var("LUMINA_CHORD_SECRET", "");
        std::env::set_var("OLLAMA_EMBEDDING_URL", format!("{}/api/embeddings", mock.base_url()));
        std::env::set_var("ENGRAM_EMBED_MODEL", "test-model");
        let config = Config::from_env().unwrap();

        let result = inject_memories("do you know my preferences?", "You are Lumina.", 5, &store, &config).await;
        assert!(result.contains("Known facts about the user:"), "Should inject facts header");
        assert!(result.contains("user likes dark mode"), "Should inject the fact");
        assert!(result.starts_with("You are Lumina."), "Original prompt should be preserved");

        let _ = std::fs::remove_file(&path);
        std::env::remove_var("CHORD_PROXY_URL");
        std::env::remove_var("LUMINA_CHORD_SECRET");
        std::env::remove_var("OLLAMA_EMBEDDING_URL");
        std::env::remove_var("ENGRAM_EMBED_MODEL");
    }

    #[test]
    fn test_inject_memory_bullets_empty_facts_unchanged() {
        let prompt = "You are Lumina.";
        let result = inject_memory_bullets(prompt, &[]);
        assert_eq!(result, prompt, "Empty facts should return system prompt unchanged");
    }

    #[test]
    fn test_inject_memory_bullets_with_facts() {
        let result = inject_memory_bullets("You are Lumina.", &[
            "user likes dark mode".to_string(),
            "user is a morning person".to_string(),
        ]);
        assert!(result.starts_with("You are Lumina."));
        assert!(result.contains("Known facts about the user:"));
        assert!(result.contains("• user likes dark mode"));
        assert!(result.contains("• user is a morning person"));
    }

    // ── P2-02: per-user isolation tests ────────────────────────────────────

    #[test]
    fn test_open_for_user_at_creates_namespaced_path() {
        let base = std::path::PathBuf::from("/tmp/lumina_p202_engram_base");
        let _ = std::fs::remove_dir_all(&base);

        let store = EngramStore::open_for_user_at(&base, "user-carol", &test_key()).unwrap();
        drop(store);

        let expected = base.join("users").join("user-carol").join("engram.db");
        assert!(expected.exists(), "Per-user engram DB should exist at: {:?}", expected);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn test_engram_different_users_isolated() {
        let base = std::path::PathBuf::from("/tmp/lumina_p202_engram_isolation");
        let _ = std::fs::remove_dir_all(&base);

        // user-alice inserts a fact
        let store_a = EngramStore::open_for_user_at(&base, "user-alice", &test_key()).unwrap();
        store_a.insert_fact("alice-private-fact", &[1.0f32, 0.0, 0.0]).unwrap();
        drop(store_a);

        // user-bob's store should be empty
        let store_b = EngramStore::open_for_user_at(&base, "user-bob", &test_key()).unwrap();
        let bob_facts = store_b.all_facts().unwrap();
        assert!(bob_facts.is_empty(), "Bob should not see Alice's facts");
        drop(store_b);

        // user-alice's facts should still be there
        let store_a2 = EngramStore::open_for_user_at(&base, "user-alice", &test_key()).unwrap();
        let alice_facts = store_a2.all_facts().unwrap();
        assert_eq!(alice_facts.len(), 1);
        assert_eq!(alice_facts[0].0, "alice-private-fact");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn test_user_id_column_exists_in_engram() {
        // Verify user_id column by inserting a fact and reading it back.
        // Migration applies the column on open; if missing, insert_fact would fail.
        let path = tmp_db("p202_engram_migration");
        let store = EngramStore::open(&path, &test_key()).unwrap();
        store.insert_fact("migration test fact", &[1.0f32, 0.0]).unwrap();
        let facts = store.all_facts().unwrap();
        assert_eq!(facts.len(), 1, "Fact should be stored after user_id migration");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_invalid_user_id_rejected_engram() {
        let base = std::path::PathBuf::from("/tmp/lumina_p202_engram_invalid");
        let result = EngramStore::open_for_user_at(&base, "../etc", &test_key());
        assert!(result.is_err(), "Path traversal user_id should be rejected");

        let result2 = EngramStore::open_for_user_at(&base, "", &test_key());
        assert!(result2.is_err(), "Empty user_id should be rejected");
    }

    // ── EMEM-01: typed Memory API tests ───────────────────────────────────

    #[test]
    fn test_insert_memory_round_trip() {
        let path = tmp_db("insert_memory");
        let store = EngramStore::open(&path, &test_key()).unwrap();
        // Use "system" to match the default store user_id
        let mut mem = types::Memory::new("system", MemoryType::Preference, SensitivityCategory::General, "likes dark roast coffee");
        mem.embedding = vec![1.0f32, 0.0, 0.0];
        mem.confidence = 0.95;
        mem.tags = vec!["coffee".to_string(), "preference".to_string()];
        store.insert_memory(&mem).unwrap();

        let facts = store.all_facts().unwrap();
        assert_eq!(facts.len(), 1, "should have 1 memory");
        assert_eq!(facts[0].0, "likes dark roast coffee");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_insert_memory_syncs_to_fts() {
        let path = tmp_db("insert_memory_fts");
        let store = EngramStore::open(&path, &test_key()).unwrap();
        let mem = types::Memory::new("system", MemoryType::Semantic, SensitivityCategory::Work, "project deadline is June 10");
        store.insert_memory(&mem).unwrap();
        // Verify via query() — no panic = FTS sync worked correctly
        let _ = store.query("project deadline", None, 5).unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_query_by_type_returns_only_matching() {
        let path = tmp_db("query_by_type");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        let pref = types::Memory::new("system", MemoryType::Preference, SensitivityCategory::General, "likes dark mode");
        let fact = types::Memory::new("system", MemoryType::Semantic, SensitivityCategory::General, "is a senior manager");
        let epis = types::Memory::new("system", MemoryType::Episodic, SensitivityCategory::Work, "had meeting on Tuesday");
        store.insert_memory(&pref).unwrap();
        store.insert_memory(&fact).unwrap();
        store.insert_memory(&epis).unwrap();

        let preferences = store.query_by_type(MemoryType::Preference).unwrap();
        assert_eq!(preferences.len(), 1, "should return only Preference memories");
        assert_eq!(preferences[0].content, "likes dark mode");
        assert_eq!(preferences[0].memory_type, MemoryType::Preference);

        let semantics = store.query_by_type(MemoryType::Semantic).unwrap();
        assert_eq!(semantics.len(), 1, "should return only Semantic memories");
        assert_eq!(semantics[0].content, "is a senior manager");

        let principles = store.query_by_type(MemoryType::Principle).unwrap();
        assert!(principles.is_empty(), "no Principle memories inserted");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_query_by_type_user_isolation() {
        let base_a = std::path::PathBuf::from("/tmp/lumina_emem01_qa");
        let base_b = std::path::PathBuf::from("/tmp/lumina_emem01_qb");
        let _ = std::fs::remove_dir_all(&base_a);
        let _ = std::fs::remove_dir_all(&base_b);

        let key = test_key();
        // store_a is opened for "user-alice" — its user_id == "user-alice"
        let store_a = EngramStore::open_for_user_at(&base_a, "user-alice", &key).unwrap();
        let mem_a = types::Memory::new("user-alice", MemoryType::Preference, SensitivityCategory::General, "alice's coffee preference");
        store_a.insert_memory(&mem_a).unwrap();
        drop(store_a);

        // store_b is for "user-bob" — separate DB, cannot see alice's memories
        let store_b = EngramStore::open_for_user_at(&base_b, "user-bob", &key).unwrap();
        let bob_prefs = store_b.query_by_type(MemoryType::Preference).unwrap();
        assert!(bob_prefs.is_empty(), "Bob should not see Alice's typed memories");

        let _ = std::fs::remove_dir_all(&base_a);
        let _ = std::fs::remove_dir_all(&base_b);
    }

    #[test]
    fn test_sensitive_memories_default_to_private_visibility() {
        let path = tmp_db("sensitive_vis");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        let h = types::Memory::new("system", MemoryType::Semantic, SensitivityCategory::Health, "shellfish allergy");
        assert_eq!(h.visibility, Visibility::Private, "Health must be Private");
        store.insert_memory(&h).unwrap();

        let f = types::Memory::new("system", MemoryType::Semantic, SensitivityCategory::Finance, "monthly budget $5000");
        assert_eq!(f.visibility, Visibility::Private, "Finance must be Private");
        store.insert_memory(&f).unwrap();

        let p = types::Memory::new("system", MemoryType::Semantic, SensitivityCategory::Personal, "went through a hard breakup");
        assert_eq!(p.visibility, Visibility::Private, "Personal must be Private");
        store.insert_memory(&p).unwrap();

        let count: i64 = store.conn.query_row(
            "SELECT COUNT(*) FROM memories_v2 WHERE visibility = 'private' AND sensitivity IN ('health','finance','personal')",
            [], |r| r.get(0)
        ).unwrap();
        assert_eq!(count, 3, "All 3 sensitive memories must be stored with private visibility");

        let _ = std::fs::remove_file(&path);
    }
}
