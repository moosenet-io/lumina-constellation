//! EMEM-08: Shared household memory management.
//!
//! Provides SharedMemoryManager — the surface for sharing, unsharing, querying,
//! creating, and formatting shared memories within a household.
//!
//! Privacy invariants (enforced by the existing EMEM-02 layer):
//! - Health/Finance/Personal memories CANNOT be shared — PrivacyEnforcer blocks them.
//! - Only the creator (user_id) may share or unshare a memory.
//! - Any household member can READ shared memories (visibility = 'shared').

use crate::error::{LuminaError, Result};
use rusqlite::{params, Connection};
use super::{EngramStore, row_to_memory};
use super::types::{Memory, MemoryType, SensitivityCategory, Visibility, new_uuid, iso_now};

// ── SharedMemoryManager ───────────────────────────────────────────────────────

/// Household shared memory operations.
///
/// All methods are stateless — they receive either an `&EngramStore` or a raw
/// `&Connection` and delegate privacy checks to `PrivacyEnforcer` / `EngramStore`.
pub struct SharedMemoryManager;

impl SharedMemoryManager {
    /// Change a memory's visibility from Private → Shared.
    ///
    /// Delegates to `store.share_memory()` which calls `PrivacyEnforcer::validate_share()`
    /// internally. Health/Finance/Personal memories are blocked automatically.
    /// Only the creator of the memory (matching user_id) may share it.
    pub fn share(store: &EngramStore, caller_user_id: &str, memory_id: &str) -> Result<()> {
        store.share_memory(caller_user_id, memory_id)
    }

    /// Change a memory's visibility back to Private (unshare).
    ///
    /// Only the creator (matching user_id) may unshare. The memory must exist
    /// and must be owned by `caller_user_id`.
    pub fn unshare(conn: &Connection, caller_user_id: &str, memory_id: &str) -> Result<()> {
        // Load the memory to verify ownership.
        let mem: Option<Memory> = {
            let mut stmt = conn.prepare(
                "SELECT id, user_id, memory_type, visibility, sensitivity, content, embedding,
                        source_conversation_id, source_turn_index, confidence, access_count,
                        last_accessed, created_at, updated_at, superseded_by, tags
                 FROM memories_v2 WHERE id = ?1",
            ).map_err(|e| LuminaError::Config(format!("unshare prepare: {e}")))?;

            let mut rows = stmt.query_map(params![memory_id], row_to_memory)
                .map_err(|e| LuminaError::Config(format!("unshare query: {e}")))?;

            match rows.next() {
                Some(Ok(m)) => Some(m),
                Some(Err(e)) => return Err(LuminaError::Config(format!("unshare row: {e}"))),
                None => None,
            }
        };

        let mem = mem.ok_or_else(|| LuminaError::Config(format!("memory {memory_id} not found")))?;

        // Only the creator may unshare.
        // TODO: add admin role check when household roles are implemented (future spec).
        if mem.user_id != caller_user_id {
            // Use the same opaque error pattern as EMEM-02.
            return Err(LuminaError::SecurityViolation(
                "This memory is not accessible.".to_string(),
            ));
        }

        let now = iso_now();
        conn.execute(
            "UPDATE memories_v2 SET visibility = 'private', updated_at = ?1 WHERE id = ?2",
            params![now, memory_id],
        ).map_err(|e| LuminaError::Config(format!("unshare update: {e}")))?;

        Ok(())
    }

    /// Return shared memories accessible to a household member, optionally filtered by query text.
    ///
    /// Fetches rows where `visibility = 'shared'` — no user_id filter because
    /// shared memories are visible to ALL household members by design.
    ///
    /// **Isolation model**: Shared memories span all users in the same SQLCipher database
    /// file (the household-scoped DB). Household membership is enforced at the store-open
    /// level via `EngramStore::open_for_user_at()` — only users granted the DB key can open
    /// the store. The `_user_id` parameter is accepted for API symmetry and future
    /// household-role filtering; it is intentionally unused today.
    ///
    /// When `query` is `Some(text)`, a case-insensitive substring match on `content`
    /// is applied (LOWER LIKE with wildcard escaping). Pass `None` (or `Some("")`) to
    /// retrieve all shared memories up to `limit`.
    ///
    /// Results are ordered newest-first, capped at `limit`.
    pub fn query_shared(conn: &Connection, _user_id: &str, query: Option<&str>, limit: usize) -> Result<Vec<Memory>> {
        // Build query: optionally filter by content substring.
        let (sql, use_filter): (&str, bool) = match query {
            Some(q) if !q.is_empty() => (
                "SELECT id, user_id, memory_type, visibility, sensitivity, content, embedding,
                        source_conversation_id, source_turn_index, confidence, access_count,
                        last_accessed, created_at, updated_at, superseded_by, tags
                 FROM memories_v2
                 WHERE visibility = 'shared' AND superseded_by IS NULL
                   AND LOWER(content) LIKE LOWER(?1) ESCAPE '\\'
                 ORDER BY created_at DESC
                 LIMIT ?2",
                true,
            ),
            _ => (
                "SELECT id, user_id, memory_type, visibility, sensitivity, content, embedding,
                        source_conversation_id, source_turn_index, confidence, access_count,
                        last_accessed, created_at, updated_at, superseded_by, tags
                 FROM memories_v2
                 WHERE visibility = 'shared' AND superseded_by IS NULL
                 ORDER BY created_at DESC
                 LIMIT ?1",
                false,
            ),
        };

        let mut stmt = conn.prepare(sql)
            .map_err(|e| LuminaError::Config(format!("query_shared prepare: {e}")))?;

        let rows = if use_filter {
            // Escape SQL LIKE wildcards (%, _) in the query to prevent inadvertent
            // wildcard expansion on user-supplied text.
            let raw = query.unwrap_or("");
            let escaped = raw.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
            let pattern = format!("%{escaped}%");
            stmt.query_map(params![pattern, limit as i64], row_to_memory)
                .map_err(|e| LuminaError::Config(format!("query_shared query: {e}")))?
        } else {
            stmt.query_map(params![limit as i64], row_to_memory)
                .map_err(|e| LuminaError::Config(format!("query_shared query: {e}")))?
        };

        let mut results = Vec::new();
        for row in rows {
            match row {
                Ok(m) => results.push(m),
                Err(e) => eprintln!("engram/shared: row error (skipping): {e}"),
            }
        }
        Ok(results)
    }

    /// Create a new memory with `visibility = Shared` directly.
    ///
    /// Intended for the `/share "text"` Matrix command. Sets sensitivity to
    /// `Household` (shared by nature) and confidence to 1.0 (explicit user intent).
    /// Privacy enforcement via `store.insert_memory()` still runs — `Household`
    /// sensitivity is not in the always-private set, so this succeeds.
    pub fn create_shared(
        store: &EngramStore,
        user_id: &str,
        content: &str,
        tags: Vec<String>,
    ) -> Result<String> {
        let mut mem = Memory::new(
            user_id,
            MemoryType::Semantic,
            SensitivityCategory::Household,
            content,
        );
        mem.id = new_uuid();
        // Household defaults to Shared visibility — but be explicit.
        mem.visibility = Visibility::Shared;
        mem.confidence = 1.0; // Explicit user action → full confidence.
        mem.tags = tags;

        let id = mem.id.clone();
        store.insert_memory(&mem)?;
        Ok(id)
    }

    /// Format a slice of shared memories for LLM context injection.
    ///
    /// Each memory is prefixed with `[Shared by {user_id}]` so the model knows
    /// who contributed it and that it is household-visible, not private.
    pub fn format_shared_for_context(memories: &[Memory]) -> String {
        if memories.is_empty() {
            return String::new();
        }
        memories
            .iter()
            .map(|m| format!("[Shared by {}] {}", m.user_id, m.content))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engram::{EngramStore, types::*};
    use std::path::PathBuf;

    fn test_key() -> Vec<u8> {
        vec![0u8; 32]
    }

    fn tmp_db(tag: &str) -> PathBuf {
        let p = PathBuf::from(format!("/tmp/lumina_shared_test_{tag}.db"));
        let _ = std::fs::remove_file(&p);
        p
    }

    // Helper: open a store scoped to a specific user_id.
    fn open_store_for(base_tag: &str, user_id: &str) -> (PathBuf, EngramStore) {
        let base = PathBuf::from(format!("/tmp/lumina_shared_base_{base_tag}"));
        let _ = std::fs::remove_dir_all(&base);
        let store = EngramStore::open_for_user_at(&base, user_id, &test_key()).unwrap();
        (base, store)
    }

    // ── share / unshare ────────────────────────────────────────────────────────

    /// Sharing a General memory should change its visibility to Shared.
    #[test]
    fn test_share_changes_visibility_to_shared() {
        let (base, store) = open_store_for("share_vis", "alice");

        let mem = Memory::new("alice", MemoryType::Semantic, SensitivityCategory::General, "prefers oat milk");
        let id = mem.id.clone();
        store.insert_memory(&mem).unwrap();

        // Before share: visibility = private
        let loaded = store.get_memory_by_id(&id).unwrap().unwrap();
        assert_eq!(loaded.visibility, Visibility::Private);

        SharedMemoryManager::share(&store, "alice", &id).unwrap();

        let updated = store.get_memory_by_id(&id).unwrap().unwrap();
        assert_eq!(updated.visibility, Visibility::Shared, "Visibility should change to Shared");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Attempting to share a Health memory must be blocked by PrivacyEnforcer.
    #[test]
    fn test_share_blocked_for_health_sensitivity() {
        let (base, store) = open_store_for("share_health", "alice");

        let mem = Memory::new("alice", MemoryType::Semantic, SensitivityCategory::Health, "has peanut allergy");
        let id = mem.id.clone();
        store.insert_memory(&mem).unwrap();

        let result = SharedMemoryManager::share(&store, "alice", &id);
        assert!(result.is_err(), "Health memory must be blocked from sharing");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Attempting to share a Finance memory must be blocked.
    #[test]
    fn test_share_blocked_for_finance_sensitivity() {
        let (base, store) = open_store_for("share_finance", "alice");

        let mem = Memory::new("alice", MemoryType::Semantic, SensitivityCategory::Finance, "monthly budget $3000");
        let id = mem.id.clone();
        store.insert_memory(&mem).unwrap();

        let result = SharedMemoryManager::share(&store, "alice", &id);
        assert!(result.is_err(), "Finance memory must be blocked from sharing");

        let _ = std::fs::remove_dir_all(&base);
    }

    /// Unsharing should change visibility back to Private.
    #[test]
    fn test_unshare_changes_visibility_back_to_private() {
        let path = tmp_db("unshare_vis");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        let mut mem = Memory::new("system", MemoryType::Semantic, SensitivityCategory::General, "grocery run on Friday");
        mem.visibility = Visibility::Shared;
        let id = mem.id.clone();
        store.insert_memory(&mem).unwrap();

        // Verify it was stored as shared.
        let loaded = store.get_memory_by_id(&id).unwrap().unwrap();
        assert_eq!(loaded.visibility, Visibility::Shared);

        // Unshare using the raw connection path.
        SharedMemoryManager::unshare(&store.conn, "system", &id).unwrap();

        let updated = store.get_memory_by_id(&id).unwrap().unwrap();
        assert_eq!(updated.visibility, Visibility::Private, "Should revert to Private after unshare");

        let _ = std::fs::remove_file(&path);
    }

    /// A non-owner trying to unshare should get a security error.
    #[test]
    fn test_unshare_blocked_for_non_owner() {
        let path = tmp_db("unshare_nonowner");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        let mut mem = Memory::new("system", MemoryType::Semantic, SensitivityCategory::General, "alice owns this");
        mem.visibility = Visibility::Shared;
        // Set user_id directly to simulate alice's memory in a shared DB.
        mem.user_id = "alice".to_string();
        let id = mem.id.clone();
        // Insert directly via raw connection (bypassing user_id scope of store).
        let blob: Option<Vec<u8>> = None;
        let tags_json = "[]";
        store.conn.execute(
            "INSERT INTO memories_v2
             (id, user_id, memory_type, visibility, sensitivity, content, embedding,
              confidence, access_count, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                mem.id, "alice", "semantic", "shared", "general",
                "alice owns this", blob,
                0.8_f64, 0_i32, iso_now(), iso_now()
            ],
        ).unwrap();

        let result = SharedMemoryManager::unshare(&store.conn, "bob", &id);
        assert!(result.is_err(), "Bob should not be able to unshare Alice's memory");

        let _ = std::fs::remove_file(&path);
    }

    // ── query_shared ───────────────────────────────────────────────────────────

    /// query_shared should return memories with visibility = 'shared'.
    #[test]
    fn test_query_shared_returns_shared_memories() {
        let path = tmp_db("query_shared_ret");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        // Insert a shared memory directly.
        let now = iso_now();
        let id1 = new_uuid();
        store.conn.execute(
            "INSERT INTO memories_v2
             (id, user_id, memory_type, visibility, sensitivity, content, confidence, access_count, created_at, updated_at)
             VALUES (?1, 'alice', 'semantic', 'shared', 'household', 'grocery list: eggs, milk', 0.9, 0, ?2, ?2)",
            params![id1, now],
        ).unwrap();

        let results = SharedMemoryManager::query_shared(&store.conn, "bob", None, 10).unwrap();
        assert_eq!(results.len(), 1, "Should return 1 shared memory");
        assert_eq!(results[0].content, "grocery list: eggs, milk");
        assert_eq!(results[0].visibility, Visibility::Shared);

        let _ = std::fs::remove_file(&path);
    }

    /// query_shared must exclude private memories — they should be invisible.
    #[test]
    fn test_query_shared_excludes_private_memories() {
        let path = tmp_db("query_shared_excl");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        let now = iso_now();
        // Shared memory.
        store.conn.execute(
            "INSERT INTO memories_v2
             (id, user_id, memory_type, visibility, sensitivity, content, confidence, access_count, created_at, updated_at)
             VALUES (?1, 'alice', 'semantic', 'shared', 'general', 'shared content', 0.8, 0, ?2, ?2)",
            params![new_uuid(), now],
        ).unwrap();

        // Private memory — must NOT appear in query_shared results.
        store.conn.execute(
            "INSERT INTO memories_v2
             (id, user_id, memory_type, visibility, sensitivity, content, confidence, access_count, created_at, updated_at)
             VALUES (?1, 'alice', 'semantic', 'private', 'general', 'private content', 0.8, 0, ?2, ?2)",
            params![new_uuid(), now],
        ).unwrap();

        let results = SharedMemoryManager::query_shared(&store.conn, "bob", None, 10).unwrap();
        assert_eq!(results.len(), 1, "Only shared memories should appear");
        assert_eq!(results[0].content, "shared content");
        assert!(!results.iter().any(|m| m.content.contains("private")),
            "Private content should never appear in query_shared");

        let _ = std::fs::remove_file(&path);
    }

    /// Creator attribution (user_id) must be preserved after sharing.
    #[test]
    fn test_creator_attribution_preserved() {
        let path = tmp_db("creator_attr");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        let now = iso_now();
        let id = new_uuid();
        store.conn.execute(
            "INSERT INTO memories_v2
             (id, user_id, memory_type, visibility, sensitivity, content, confidence, access_count, created_at, updated_at)
             VALUES (?1, 'partner', 'semantic', 'shared', 'household', 'we eat dinner at 7pm', 1.0, 0, ?2, ?2)",
            params![id, now],
        ).unwrap();

        let results = SharedMemoryManager::query_shared(&store.conn, "bob", None, 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].user_id, "partner", "Creator attribution must be preserved");

        let _ = std::fs::remove_file(&path);
    }

    // ── create_shared ──────────────────────────────────────────────────────────

    /// create_shared should produce a memory with visibility = Shared.
    #[test]
    fn test_create_shared_produces_shared_memory() {
        let (base, store) = open_store_for("create_shared", "alice");

        let id = SharedMemoryManager::create_shared(
            &store,
            "alice",
            "family dinner is Sunday at 6pm",
            vec!["schedule".to_string()],
        ).unwrap();

        assert!(!id.is_empty(), "Should return a non-empty memory ID");

        // Verify directly via query_shared using the store's connection.
        let results = SharedMemoryManager::query_shared(&store.conn, "bob", None, 10).unwrap();
        let found = results.iter().find(|m| m.id == id);
        assert!(found.is_some(), "Created memory should appear in query_shared");
        let m = found.unwrap();
        assert_eq!(m.visibility, Visibility::Shared);
        assert_eq!(m.sensitivity, SensitivityCategory::Household);
        assert_eq!(m.confidence, 1.0);
        assert_eq!(m.user_id, "alice");

        let _ = std::fs::remove_dir_all(&base);
    }

    // ── format_shared_for_context ──────────────────────────────────────────────

    /// format_shared_for_context should prefix each memory with [Shared by user_id].
    #[test]
    fn test_format_shared_for_context_labels() {
        let mut m1 = Memory::new("alice", MemoryType::Semantic, SensitivityCategory::Household, "we have a cat named Biscuit");
        m1.visibility = Visibility::Shared;
        let mut m2 = Memory::new("partner", MemoryType::Semantic, SensitivityCategory::Household, "grocery run on Saturdays");
        m2.visibility = Visibility::Shared;

        let formatted = SharedMemoryManager::format_shared_for_context(&[m1, m2]);

        assert!(formatted.contains("[Shared by alice]"), "Should include alice attribution");
        assert!(formatted.contains("[Shared by partner]"), "Should include partner attribution");
        assert!(formatted.contains("we have a cat named Biscuit"), "Should include memory content");
        assert!(formatted.contains("grocery run on Saturdays"), "Should include second memory content");
    }

    /// Empty slice should produce an empty string (no panics).
    #[test]
    fn test_format_shared_for_context_empty_returns_empty_string() {
        let formatted = SharedMemoryManager::format_shared_for_context(&[]);
        assert!(formatted.is_empty(), "Empty memories should produce empty string");
    }

    /// Single memory — verify exact format.
    #[test]
    fn test_format_shared_for_context_exact_format() {
        let mut m = Memory::new("bob", MemoryType::Semantic, SensitivityCategory::Household, "we prefer decaf after noon");
        m.visibility = Visibility::Shared;

        let formatted = SharedMemoryManager::format_shared_for_context(&[m]);
        assert_eq!(formatted, "[Shared by bob] we prefer decaf after noon");
    }

    /// query_shared with SQL wildcard characters in the query should not match-all.
    #[test]
    fn test_query_shared_text_filter_escapes_wildcards() {
        let path = tmp_db("query_shared_wildcard");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        let now = iso_now();
        store.conn.execute(
            "INSERT INTO memories_v2
             (id, user_id, memory_type, visibility, sensitivity, content, confidence, access_count, created_at, updated_at)
             VALUES (?1, 'alice', 'semantic', 'shared', 'household', 'sale: 100% off today', 0.9, 0, ?2, ?2)",
            params![new_uuid(), now],
        ).unwrap();
        store.conn.execute(
            "INSERT INTO memories_v2
             (id, user_id, memory_type, visibility, sensitivity, content, confidence, access_count, created_at, updated_at)
             VALUES (?1, 'alice', 'semantic', 'shared', 'household', 'other unrelated item', 0.9, 0, ?2, ?2)",
            params![new_uuid(), now],
        ).unwrap();

        // Searching for "100%" should NOT match "other unrelated item" (wildcard not expanded).
        let results = SharedMemoryManager::query_shared(&store.conn, "bob", Some("100%"), 10).unwrap();
        assert_eq!(results.len(), 1, "% should be escaped, not act as SQL wildcard");
        assert!(results[0].content.contains("100%"));

        let _ = std::fs::remove_file(&path);
    }

    /// query_shared with a text filter should return only matching memories.
    #[test]
    fn test_query_shared_with_text_filter() {
        let path = tmp_db("query_shared_filter");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        let now = iso_now();
        store.conn.execute(
            "INSERT INTO memories_v2
             (id, user_id, memory_type, visibility, sensitivity, content, confidence, access_count, created_at, updated_at)
             VALUES (?1, 'alice', 'semantic', 'shared', 'household', 'grocery list: eggs milk bread', 0.9, 0, ?2, ?2)",
            params![new_uuid(), now],
        ).unwrap();
        store.conn.execute(
            "INSERT INTO memories_v2
             (id, user_id, memory_type, visibility, sensitivity, content, confidence, access_count, created_at, updated_at)
             VALUES (?1, 'alice', 'semantic', 'shared', 'household', 'family dinner every Sunday', 0.9, 0, ?2, ?2)",
            params![new_uuid(), now],
        ).unwrap();

        // Filter for "grocery" — should return only the first entry.
        let results = SharedMemoryManager::query_shared(&store.conn, "bob", Some("grocery"), 10).unwrap();
        assert_eq!(results.len(), 1, "Filter should narrow to matching memories");
        assert!(results[0].content.contains("grocery"));

        // No filter — should return both.
        let all = SharedMemoryManager::query_shared(&store.conn, "bob", None, 10).unwrap();
        assert_eq!(all.len(), 2, "No filter should return all shared memories");

        let _ = std::fs::remove_file(&path);
    }

    /// Attempting to share a Personal memory must be blocked.
    #[test]
    fn test_share_blocked_for_personal_sensitivity() {
        let (base, store) = open_store_for("share_personal", "alice");

        let mem = Memory::new("alice", MemoryType::Semantic, SensitivityCategory::Personal, "going through a hard time");
        let id = mem.id.clone();
        store.insert_memory(&mem).unwrap();

        let result = SharedMemoryManager::share(&store, "alice", &id);
        assert!(result.is_err(), "Personal memory must be blocked from sharing");

        let _ = std::fs::remove_dir_all(&base);
    }
}
