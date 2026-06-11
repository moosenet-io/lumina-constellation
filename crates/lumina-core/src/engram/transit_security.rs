//! ESEC-05: Memory transit minimization and sanitization.
//!
//! The practical defense for memory content in transit to LLM and embedding
//! endpoints is NOT transport-layer encryption (Chord/Ollama don't support it)
//! but a three-pronged approach:
//!
//! 1. **Minimization** — hard caps on how many memories are included per request:
//!    - LLM requests: max 10 memories (`MAX_MEMORIES_PER_LLM_REQUEST`)
//!    - Embedding requests: one memory at a time (`enforce_embed_one_at_a_time`)
//!    - Reflexa consolidation batches: max 5 (`MAX_REFLEXA_BATCH_SIZE`)
//!
//! 2. **Sanitization** — provenance and identity fields stripped before LLM transit:
//!    - `source_conversation_id` — the LLM doesn't need it for extraction/abstraction
//!    - `source_turn_index` — ditto
//!    - `user_id` — the LLM doesn't need to know whose memory this is
//!    - `sensitivity` — prevents the LLM from learning classification patterns
//!
//! 3. **Egress audit** — request sizes logged (not content) so anomalous payloads
//!    (potential exfiltration) can be detected.

use std::time::{SystemTime, UNIX_EPOCH};
use crate::engram::types::{Memory, MemoryType};
use crate::error::{LuminaError, Result};

// ── Constants ──────────────────────────────────────────────────────────────────

/// Maximum number of memories that may be included in a single LLM request.
///
/// Enforced by `TransitSanitizer::sanitize_for_llm`. If the input slice
/// exceeds this limit, the highest-scoring memories are kept (caller is
/// responsible for pre-ranking) and a warning is logged.
pub const MAX_MEMORIES_PER_LLM_REQUEST: usize = 10;

/// Maximum number of memories per Reflexa consolidation batch.
///
/// Reflexa processes batches of memories for consolidation/supersession.
/// Limiting batch size bounds the exposure per HTTP call.
pub const MAX_REFLEXA_BATCH_SIZE: usize = 5;

// ── SanitizedMemory ───────────────────────────────────────────────────────────

/// A memory with all provenance and identity fields removed, safe for LLM transit.
///
/// Contains only the semantic content the LLM needs for extraction or abstraction.
/// Deliberately does NOT implement `From<Memory>` or `Into<Memory>` to prevent
/// accidental round-tripping of sensitive fields.
#[derive(Debug, Clone)]
pub struct SanitizedMemory {
    /// The actual remembered content. The only field the LLM needs.
    pub content: String,
    /// Cognitive category (type label for context injection).
    pub memory_type: MemoryType,
    /// Confidence 0.0–1.0 — useful for weighting LLM context, but not sensitive.
    pub confidence: f32,
    /// Free-form tags — no user identity, safe to include.
    pub tags: Vec<String>,
    /// ISO 8601 creation timestamp — useful for temporal context, not sensitive.
    pub created_at: String,
}

impl SanitizedMemory {
    /// A type label suitable for context injection into an LLM prompt.
    pub fn type_label(&self) -> &'static str {
        match self.memory_type {
            MemoryType::Principle => "[Principle]",
            MemoryType::Preference => "[Preference]",
            MemoryType::Semantic => "[Fact]",
            MemoryType::Episodic => "[Recent]",
        }
    }
}

// ── TransitAuditEntry ─────────────────────────────────────────────────────────

/// An immutable audit record for a single memory transit event.
///
/// Records the size and endpoint type of every memory-containing request.
/// Never records content — only metadata needed for anomaly detection.
#[derive(Debug, Clone)]
pub struct TransitAuditEntry {
    /// Unix timestamp (seconds) of the request.
    pub timestamp: u64,
    /// Category of the downstream endpoint receiving the memories.
    pub target_endpoint_type: TransitTarget,
    /// How many memories were included in the request.
    pub memory_count: usize,
    /// Approximate payload size in bytes (serialized content lengths).
    pub total_bytes: usize,
}

/// The kind of endpoint receiving a memory-containing request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitTarget {
    /// LLM inference endpoint (Chord / llama-server).
    Llm,
    /// Embedding generation endpoint (Ollama).
    Embedding,
    /// Reflexa consolidation pipeline.
    Reflexa,
}

impl TransitTarget {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Llm => "llm",
            Self::Embedding => "embedding",
            Self::Reflexa => "reflexa",
        }
    }
}

// ── TransitSanitizer ──────────────────────────────────────────────────────────

/// Core ESEC-05 control: sanitizes, caps, and audits memory transit.
///
/// All methods are pure (no I/O); callers integrate audit entries into their
/// own logging/audit subsystem via the returned `TransitAuditEntry`.
pub struct TransitSanitizer;

impl TransitSanitizer {
    /// Strip provenance and identity fields from memories before sending to an LLM.
    ///
    /// Enforces `MAX_MEMORIES_PER_LLM_REQUEST` — truncates the input slice to that
    /// limit with a warning log if exceeded. Callers should pre-rank memories by
    /// relevance score so the most important ones survive truncation.
    ///
    /// **Stripped fields:** `source_conversation_id`, `source_turn_index`, `user_id`,
    /// `sensitivity`.
    ///
    /// **Kept fields:** `content`, `memory_type`, `confidence`, `tags`, `created_at`.
    pub fn sanitize_for_llm(memories: &[Memory]) -> Vec<SanitizedMemory> {
        let input_count = memories.len();
        let capped = if input_count > MAX_MEMORIES_PER_LLM_REQUEST {
            eprintln!(
                "transit_security: LLM request capped at {} memories (caller supplied {}); \
                 pre-rank by score so top {} are kept",
                MAX_MEMORIES_PER_LLM_REQUEST, input_count, MAX_MEMORIES_PER_LLM_REQUEST
            );
            &memories[..MAX_MEMORIES_PER_LLM_REQUEST]
        } else {
            memories
        };

        capped
            .iter()
            .map(|m| SanitizedMemory {
                content: m.content.clone(),
                memory_type: m.memory_type.clone(),
                confidence: m.confidence,
                tags: m.tags.clone(),
                created_at: m.created_at.clone(),
                // user_id, source_conversation_id, source_turn_index, sensitivity — STRIPPED
            })
            .collect()
    }

    /// Validate that an embedding request contains exactly one memory.
    ///
    /// Returns `Ok(())` if `count == 1`, otherwise an error describing the violation.
    /// Callers MUST call this before dispatching to the embedding endpoint and split
    /// larger slices into individual calls.
    pub fn enforce_embed_one_at_a_time(count: usize) -> Result<()> {
        if count == 1 {
            Ok(())
        } else {
            Err(LuminaError::SecurityViolation(format!(
                "transit_security: embedding requests must be sent one at a time \
                 (attempted to send {count}); split into {count} individual calls"
            )))
        }
    }

    /// Validate and return a capped slice of memories for a Reflexa batch.
    ///
    /// If `memories.len() > MAX_REFLEXA_BATCH_SIZE`, returns the first
    /// `MAX_REFLEXA_BATCH_SIZE` items and logs a warning. Callers should
    /// call this repeatedly on successive chunks of the full memory set.
    pub fn cap_reflexa_batch(memories: &[Memory]) -> &[Memory] {
        if memories.len() > MAX_REFLEXA_BATCH_SIZE {
            eprintln!(
                "transit_security: Reflexa batch capped at {} memories (supplied {}); \
                 process remaining memories in subsequent batches",
                MAX_REFLEXA_BATCH_SIZE,
                memories.len()
            );
            &memories[..MAX_REFLEXA_BATCH_SIZE]
        } else {
            memories
        }
    }

    /// Log the size (not content) of a memory-containing transit request.
    ///
    /// Computes `total_bytes` as the sum of UTF-8 byte lengths of the content
    /// fields — a proxy for actual serialized payload size. Logs at `info` level
    /// (via `eprintln!` since we have no logger dependency here) and returns a
    /// `TransitAuditEntry` for the caller to persist.
    pub fn log_transit_size(
        target: TransitTarget,
        memories: &[SanitizedMemory],
    ) -> TransitAuditEntry {
        let memory_count = memories.len();
        let total_bytes: usize = memories.iter().map(|m| m.content.len()).sum();
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        eprintln!(
            "transit_security: {} request — {} memories, ~{} bytes",
            target.as_str(),
            memory_count,
            total_bytes
        );

        TransitAuditEntry {
            timestamp,
            target_endpoint_type: target,
            memory_count,
            total_bytes,
        }
    }

    /// Convenience: log a raw (pre-sanitization) transit event for embedding calls.
    ///
    /// Embedding calls send a single raw text string, so there is no `SanitizedMemory`
    /// slice — just the byte length of the content string.
    pub fn log_embed_transit(content_bytes: usize) -> TransitAuditEntry {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        eprintln!(
            "transit_security: embedding request — 1 memory, ~{content_bytes} bytes"
        );

        TransitAuditEntry {
            timestamp,
            target_endpoint_type: TransitTarget::Embedding,
            memory_count: 1,
            total_bytes: content_bytes,
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engram::types::{Memory, MemoryType, SensitivityCategory};

    fn make_memory(user_id: &str, content: &str, sensitivity: SensitivityCategory) -> Memory {
        let mut m = Memory::new(user_id, MemoryType::Semantic, sensitivity, content);
        m.source_conversation_id = Some("conv-abc-123".to_string());
        m.source_turn_index = Some(7);
        m
    }

    fn make_memories(n: usize) -> Vec<Memory> {
        (0..n)
            .map(|i| make_memory("user-alice", &format!("memory content {i}"), SensitivityCategory::General))
            .collect()
    }

    // ── sanitize_for_llm ──────────────────────────────────────────────────────

    #[test]
    fn test_max_10_memories_enforced() {
        let memories = make_memories(15);
        let sanitized = TransitSanitizer::sanitize_for_llm(&memories);
        assert_eq!(
            sanitized.len(),
            MAX_MEMORIES_PER_LLM_REQUEST,
            "sanitize_for_llm must cap at MAX_MEMORIES_PER_LLM_REQUEST"
        );
    }

    #[test]
    fn test_fewer_than_max_not_truncated() {
        let memories = make_memories(5);
        let sanitized = TransitSanitizer::sanitize_for_llm(&memories);
        assert_eq!(sanitized.len(), 5, "must not truncate when under the cap");
    }

    #[test]
    fn test_exactly_max_not_truncated() {
        let memories = make_memories(MAX_MEMORIES_PER_LLM_REQUEST);
        let sanitized = TransitSanitizer::sanitize_for_llm(&memories);
        assert_eq!(sanitized.len(), MAX_MEMORIES_PER_LLM_REQUEST);
    }

    #[test]
    fn test_provenance_stripped() {
        let m = make_memory("user-alice", "test content", SensitivityCategory::General);
        assert!(m.source_conversation_id.is_some(), "precondition: source_conversation_id set");
        assert!(m.source_turn_index.is_some(), "precondition: source_turn_index set");

        let sanitized = TransitSanitizer::sanitize_for_llm(&[m]);
        assert_eq!(sanitized.len(), 1);
        // SanitizedMemory has no source_conversation_id or source_turn_index fields — compile-time proof
        // Verify content is preserved
        assert_eq!(sanitized[0].content, "test content");
    }

    #[test]
    fn test_user_id_stripped() {
        let m = make_memory("user-alice", "personal memory", SensitivityCategory::Personal);
        assert_eq!(m.user_id, "user-alice", "precondition: user_id is set");

        let sanitized = TransitSanitizer::sanitize_for_llm(&[m]);
        assert_eq!(sanitized.len(), 1);
        // SanitizedMemory has no user_id field — compile-time proof that it cannot be transmitted.
        // Verify content is intact.
        assert_eq!(sanitized[0].content, "personal memory");
    }

    #[test]
    fn test_sensitivity_stripped() {
        let health_mem = make_memory("user-alice", "shellfish allergy", SensitivityCategory::Health);
        assert_eq!(health_mem.sensitivity, SensitivityCategory::Health, "precondition");

        let sanitized = TransitSanitizer::sanitize_for_llm(&[health_mem]);
        assert_eq!(sanitized.len(), 1);
        // SanitizedMemory has no sensitivity field — stripped by construction.
        assert_eq!(sanitized[0].content, "shellfish allergy");
    }

    #[test]
    fn test_content_preserved_after_sanitization() {
        let memories = vec![
            make_memory("user-a", "memory one", SensitivityCategory::General),
            make_memory("user-a", "memory two", SensitivityCategory::Work),
        ];
        let sanitized = TransitSanitizer::sanitize_for_llm(&memories);
        assert_eq!(sanitized[0].content, "memory one");
        assert_eq!(sanitized[1].content, "memory two");
    }

    #[test]
    fn test_memory_type_preserved_after_sanitization() {
        let mut m = Memory::new("user-b", MemoryType::Preference, SensitivityCategory::General, "likes dark mode");
        m.source_conversation_id = None;
        m.source_turn_index = None;

        let sanitized = TransitSanitizer::sanitize_for_llm(&[m]);
        assert_eq!(sanitized[0].memory_type, MemoryType::Preference);
        assert_eq!(sanitized[0].type_label(), "[Preference]");
    }

    #[test]
    fn test_tags_preserved_after_sanitization() {
        let mut m = make_memory("user-a", "tagged content", SensitivityCategory::General);
        m.tags = vec!["coffee".to_string(), "preference".to_string()];

        let sanitized = TransitSanitizer::sanitize_for_llm(&[m]);
        assert_eq!(sanitized[0].tags, vec!["coffee", "preference"]);
    }

    #[test]
    fn test_sanitize_empty_slice() {
        let sanitized = TransitSanitizer::sanitize_for_llm(&[]);
        assert!(sanitized.is_empty(), "empty input → empty output");
    }

    // ── enforce_embed_one_at_a_time ───────────────────────────────────────────

    #[test]
    fn test_embed_one_at_a_time_accepts_one() {
        assert!(TransitSanitizer::enforce_embed_one_at_a_time(1).is_ok());
    }

    #[test]
    fn test_embed_one_at_a_time_rejects_zero() {
        let result = TransitSanitizer::enforce_embed_one_at_a_time(0);
        assert!(result.is_err(), "0 memories should be rejected");
    }

    #[test]
    fn test_embed_one_at_a_time_rejects_two() {
        let result = TransitSanitizer::enforce_embed_one_at_a_time(2);
        assert!(result.is_err(), "batches of 2+ should be rejected");
    }

    #[test]
    fn test_embed_one_at_a_time_rejects_large_batch() {
        let result = TransitSanitizer::enforce_embed_one_at_a_time(100);
        assert!(result.is_err(), "large batches must be rejected");
    }

    // ── cap_reflexa_batch ─────────────────────────────────────────────────────

    #[test]
    fn test_reflexa_batch_max_5() {
        let memories = make_memories(8);
        let batch = TransitSanitizer::cap_reflexa_batch(&memories);
        assert_eq!(
            batch.len(),
            MAX_REFLEXA_BATCH_SIZE,
            "Reflexa batch must be capped at MAX_REFLEXA_BATCH_SIZE"
        );
    }

    #[test]
    fn test_reflexa_batch_under_limit_unchanged() {
        let memories = make_memories(3);
        let batch = TransitSanitizer::cap_reflexa_batch(&memories);
        assert_eq!(batch.len(), 3, "under-limit batch must not be truncated");
    }

    #[test]
    fn test_reflexa_batch_exactly_limit_unchanged() {
        let memories = make_memories(MAX_REFLEXA_BATCH_SIZE);
        let batch = TransitSanitizer::cap_reflexa_batch(&memories);
        assert_eq!(batch.len(), MAX_REFLEXA_BATCH_SIZE);
    }

    #[test]
    fn test_reflexa_batch_empty_ok() {
        let memories: Vec<Memory> = Vec::new();
        let batch = TransitSanitizer::cap_reflexa_batch(&memories);
        assert!(batch.is_empty());
    }

    // ── log_transit_size ──────────────────────────────────────────────────────

    #[test]
    fn test_transit_size_logged_llm() {
        let memories = make_memories(3);
        let sanitized = TransitSanitizer::sanitize_for_llm(&memories);
        let entry = TransitSanitizer::log_transit_size(TransitTarget::Llm, &sanitized);

        assert_eq!(entry.memory_count, 3);
        assert_eq!(entry.target_endpoint_type, TransitTarget::Llm);
        assert!(entry.total_bytes > 0, "total_bytes should reflect content length");
        assert!(entry.timestamp > 0, "timestamp should be non-zero");
    }

    #[test]
    fn test_transit_size_logged_reflexa() {
        let memories = make_memories(2);
        let sanitized = TransitSanitizer::sanitize_for_llm(&memories);
        let entry = TransitSanitizer::log_transit_size(TransitTarget::Reflexa, &sanitized);

        assert_eq!(entry.memory_count, 2);
        assert_eq!(entry.target_endpoint_type, TransitTarget::Reflexa);
    }

    #[test]
    fn test_transit_size_logged_embedding() {
        let content = "user has shellfish allergy";
        let entry = TransitSanitizer::log_embed_transit(content.len());

        assert_eq!(entry.memory_count, 1);
        assert_eq!(entry.target_endpoint_type, TransitTarget::Embedding);
        assert_eq!(entry.total_bytes, content.len());
    }

    #[test]
    fn test_transit_audit_entry_has_timestamp() {
        let memories = make_memories(1);
        let sanitized = TransitSanitizer::sanitize_for_llm(&memories);
        let entry = TransitSanitizer::log_transit_size(TransitTarget::Llm, &sanitized);
        // Timestamp should be a plausible Unix time (post 2020-01-01 = 1577836800)
        assert!(entry.timestamp > 1_577_836_800, "timestamp looks like a real Unix time");
    }

    #[test]
    fn test_transit_total_bytes_is_sum_of_content() {
        let mut memories = Vec::new();
        for content in &["abc", "defgh", "ij"] {
            memories.push(make_memory("user-a", content, SensitivityCategory::General));
        }
        let sanitized = TransitSanitizer::sanitize_for_llm(&memories);
        let entry = TransitSanitizer::log_transit_size(TransitTarget::Llm, &sanitized);
        // "abc"(3) + "defgh"(5) + "ij"(2) = 10
        assert_eq!(entry.total_bytes, 10);
    }

    // ── TransitTarget ─────────────────────────────────────────────────────────

    #[test]
    fn test_transit_target_as_str() {
        assert_eq!(TransitTarget::Llm.as_str(), "llm");
        assert_eq!(TransitTarget::Embedding.as_str(), "embedding");
        assert_eq!(TransitTarget::Reflexa.as_str(), "reflexa");
    }
}
