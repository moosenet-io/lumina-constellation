//! DPROMPT-03: Knowledge Digest — nightly sleep-time reconstruction.
//!
//! Every night (and immediately after a session yields 5+ new facts) Lumina
//! rebuilds the `[knowledge]` layer from her *full* memory archive rather than
//! a top-N retrieval.  The deep model reads a large sample of semantic /
//! preference / principle memories plus recent session summaries and writes a
//! single coherent ~300-token "who is this person" summary.
//!
//! ## Design
//! * The builder is pure and synchronous: memories come from a [`MemorySource`]
//!   trait and generation from the shared [`LlmGenerator`] seam, so the whole
//!   thing is unit-testable with no live Engram DB or Chord proxy.  Wave 3 will
//!   supply an Engram-backed `MemorySource` and wire [`reconstruct`] into the
//!   nightly [`super::sleep_time`] orchestrator.
//! * Failure is non-destructive: if generation errors, an *existing*
//!   `knowledge-digest.txt` is left untouched and the error is propagated.
//! * No network, no chrono — weighted sampling uses a small deterministic LCG
//!   seeded from the memory set so tests are reproducible.
//!
//! [`LlmGenerator`]: super::llm::LlmGenerator

use std::path::Path;

use crate::error::{LuminaError, Result};
use crate::prompt::layers::{estimate_tokens, truncate_to_tokens};
use crate::prompt::llm::LlmGenerator;

/// Filename of the knowledge-digest layer within a user's layer directory.
/// Mirrors the `Knowledge` entry in [`crate::prompt::layers::LAYERS`].
pub const DIGEST_FILENAME: &str = "knowledge-digest.txt";

/// Model tier used for reconstruction — quality matters, so the deep model.
pub const DEEP_MODEL_HINT: &str = "lumina-deep";

/// Hard cap on the assembled digest, per the spec ("300-token output").
pub const DIGEST_TOKEN_LIMIT: usize = 300;

/// Maximum memories considered per reconstruction.  Beyond this we sample,
/// weighted by access_count.
pub const MAX_MEMORIES: usize = 500;

/// How many recent session summaries to fold in for recency context.
pub const RECENT_SESSIONS: usize = 20;

/// Below this many memories we emit a simple list instead of a narrative.
pub const FEW_MEMORIES_THRESHOLD: usize = 10;

/// New-fact count at/above which a session triggers immediate reconstruction.
pub const IMMEDIATE_TRIGGER_FACTS: usize = 5;

/// Message written/returned when a user has no memories yet.
pub const COLD_START_DIGEST: &str = "I'm still getting to know you. Tell me about yourself.";

/// One memory as seen by the digest builder.
///
/// A reduced projection of [`crate::engram::types::Memory`] — only the fields
/// reconstruction needs.  Wave 3's Engram-backed [`MemorySource`] maps real
/// `Memory` rows into this.
#[derive(Debug, Clone, PartialEq)]
pub struct DigestMemory {
    /// The remembered content.
    pub content: String,
    /// Retrieval count — used as the sampling weight (most-accessed = most
    /// important).
    pub access_count: u32,
    /// Database label of the memory type ("semantic" / "preference" /
    /// "principle").  Used only for grouping/labelling in the prompt.
    pub mem_type: String,
}

impl DigestMemory {
    /// Convenience constructor.
    pub fn new(content: impl Into<String>, access_count: u32, mem_type: impl Into<String>) -> Self {
        DigestMemory { content: content.into(), access_count, mem_type: mem_type.into() }
    }
}

/// Source of the raw material for a digest.
///
/// Abstracted so the builder can be exercised without a live Engram DB.  The
/// production impl (Wave 3) queries `memories_v2` for the user's semantic +
/// preference + principle memories and the conversation archive for recent
/// session summaries.
pub trait MemorySource {
    /// Fetch up to `limit` semantic + preference + principle memories for the
    /// user.  Implementations may return more than `limit`; the builder caps
    /// and samples.
    fn fetch_for_digest(&self, user_id: &str, limit: usize) -> Vec<DigestMemory>;

    /// Fetch up to `n` recent session summaries (most-recent first), as plain
    /// strings.
    fn recent_sessions(&self, user_id: &str, n: usize) -> Vec<String>;
}

/// Builds and persists the knowledge-digest layer.
#[derive(Debug, Clone, Default)]
pub struct KnowledgeDigestBuilder;

impl KnowledgeDigestBuilder {
    pub fn new() -> Self {
        KnowledgeDigestBuilder
    }

    /// Whether a session with `new_fact_count` extracted facts should trigger
    /// an immediate (out-of-band) reconstruction instead of waiting for the
    /// nightly run.
    pub fn should_trigger_immediate(new_fact_count: usize) -> bool {
        new_fact_count >= IMMEDIATE_TRIGGER_FACTS
    }

    /// Reconstruct the knowledge digest for `user_id` and write it to
    /// `out_dir/knowledge-digest.txt`.
    ///
    /// Returns the digest text on success.
    ///
    /// * Zero memories → cold-start message (written + returned).
    /// * `< FEW_MEMORIES_THRESHOLD` memories → simple list format (no LLM call).
    /// * Otherwise → deep-model narrative, capped to [`DIGEST_TOKEN_LIMIT`].
    ///
    /// On generator error any pre-existing digest file is **preserved**
    /// (never overwritten) and the error is returned.
    pub fn reconstruct(
        &self,
        user_id: &str,
        mem_source: &dyn MemorySource,
        gen: &dyn LlmGenerator,
        out_dir: &Path,
    ) -> Result<String> {
        let mut memories = mem_source.fetch_for_digest(user_id, MAX_MEMORIES);

        // Zero memories → cold start. Safe to write (nothing useful to lose).
        if memories.is_empty() {
            let digest = COLD_START_DIGEST.to_string();
            write_digest(out_dir, &digest)?;
            log::info!("knowledge digest cold-started for user (0 memories)");
            return Ok(digest);
        }

        // If over the cap, sample weighted by access_count.
        if memories.len() > MAX_MEMORIES {
            let original = memories.len();
            memories = sample_weighted(memories, MAX_MEMORIES);
            log::info!(
                "knowledge digest sampled {} of {original} memories (weighted by access_count)",
                memories.len()
            );
        }

        let sessions = mem_source.recent_sessions(user_id, RECENT_SESSIONS);

        // Few memories → deterministic list, no LLM spend.
        if memories.len() < FEW_MEMORIES_THRESHOLD {
            let digest = build_list_digest(&memories);
            write_digest(out_dir, &digest)?;
            log::info!(
                "knowledge digest (list form) reconstructed: {} words from {} memories",
                word_count(&digest),
                memories.len()
            );
            return Ok(digest);
        }

        // Full narrative reconstruction via the deep model.
        let user_prompt = build_reconstruction_prompt(&memories, &sessions);
        let raw = match gen.generate(DEEP_MODEL_HINT, RECONSTRUCTION_SYSTEM, &user_prompt) {
            Ok(text) => text,
            Err(e) => {
                // Preserve any existing digest — do NOT overwrite on failure.
                log::warn!(
                    "knowledge digest reconstruction failed ({e}); preserving previous digest"
                );
                return Err(e);
            }
        };

        let digest = truncate_to_tokens(raw.trim(), DIGEST_TOKEN_LIMIT);
        if digest.trim().is_empty() {
            // Generator returned nothing usable — treat as a failure, preserve.
            log::warn!("knowledge digest reconstruction produced empty output; preserving previous");
            return Err(LuminaError::Internal(
                "knowledge digest reconstruction produced empty output".into(),
            ));
        }

        write_digest(out_dir, &digest)?;
        log::info!(
            "knowledge digest reconstructed: {} words from {} memories",
            word_count(&digest),
            memories.len()
        );
        Ok(digest)
    }
}

/// System prompt for the reconstruction call.
const RECONSTRUCTION_SYSTEM: &str =
    "You are Lumina, rebuilding your private working knowledge of the person you assist.";

/// Build the user prompt exactly per the DPROMPT-03 spec.
fn build_reconstruction_prompt(memories: &[DigestMemory], sessions: &[String]) -> String {
    let n = memories.len();
    let mut p = String::new();
    p.push_str("You are rebuilding your knowledge of a person from their memory archive.\n");
    p.push_str(&format!(
        "Below are {n} facts, preferences, and principles you've learned about them,\n"
    ));
    p.push_str("plus excerpts from their recent conversations.\n\n");
    p.push_str("Write a single, coherent 300-word summary of who this person is:\n");
    p.push_str("their identity, their work, their preferences, their personality,\n");
    p.push_str("their current projects, and what matters to them. Write it as\n");
    p.push_str("context you'll read before every conversation — make it the most\n");
    p.push_str("useful 300 words possible for understanding and helping this person.\n\n");

    p.push_str("Memories:\n");
    for m in memories {
        p.push_str(&format!("[{}] {}\n", m.mem_type, m.content.trim()));
    }

    p.push_str("\nRecent conversation themes:\n");
    if sessions.is_empty() {
        p.push_str("(none recorded)\n");
    } else {
        for s in sessions {
            p.push_str(&format!("- {}\n", s.trim()));
        }
    }
    p
}

/// Simple list digest used when there are too few memories for a narrative.
fn build_list_digest(memories: &[DigestMemory]) -> String {
    let mut out = String::from("What I know so far:\n");
    for m in memories {
        out.push_str(&format!("- {}\n", m.content.trim()));
    }
    truncate_to_tokens(out.trim(), DIGEST_TOKEN_LIMIT)
}

/// Weighted sampling without replacement, keeping the `keep` highest
/// weighted-priority memories.  Weight is `access_count + 1` (so zero-access
/// memories can still be picked).  Deterministic: seeded from the input so
/// tests are reproducible and no RNG dependency is pulled in.
fn sample_weighted(memories: Vec<DigestMemory>, keep: usize) -> Vec<DigestMemory> {
    if memories.len() <= keep {
        return memories;
    }
    // Efraimidis–Spirakis weighted reservoir sampling with a deterministic LCG.
    // key = rand^(1/weight); larger key = more likely to be kept.  We avoid
    // floats-from-network by using a fixed-point pseudo-random in [1, u32::MAX].
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    for m in &memories {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(m.content.len() as u64 + m.access_count as u64 + 1);
    }
    let mut keyed: Vec<(f64, DigestMemory)> = memories
        .into_iter()
        .map(|m| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            // u in (0,1)
            let u = ((seed >> 11) as f64 + 1.0) / ((1u64 << 53) as f64 + 2.0);
            let weight = (m.access_count as f64) + 1.0;
            let key = u.powf(1.0 / weight);
            (key, m)
        })
        .collect();
    keyed.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    keyed.into_iter().take(keep).map(|(_, m)| m).collect()
}

/// Write the digest to `out_dir/knowledge-digest.txt`, creating the directory.
fn write_digest(out_dir: &Path, digest: &str) -> Result<()> {
    std::fs::create_dir_all(out_dir)?;
    std::fs::write(out_dir.join(DIGEST_FILENAME), digest)?;
    Ok(())
}

fn word_count(s: &str) -> usize {
    s.split_whitespace().count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::llm::MockGenerator;
    use std::path::PathBuf;
    use tempfile::tempdir;

    /// A failing generator for the preserve-on-failure test.
    struct FailingGenerator;
    impl LlmGenerator for FailingGenerator {
        fn generate(&self, _hint: &str, _sys: &str, _user: &str) -> Result<String> {
            Err(LuminaError::Chord("ollama down".into()))
        }
    }

    /// Fake memory source backed by in-memory vectors.
    struct FakeSource {
        mems: Vec<DigestMemory>,
        sessions: Vec<String>,
    }
    impl FakeSource {
        fn new(mems: Vec<DigestMemory>, sessions: Vec<String>) -> Self {
            FakeSource { mems, sessions }
        }
    }
    impl MemorySource for FakeSource {
        fn fetch_for_digest(&self, _user_id: &str, _limit: usize) -> Vec<DigestMemory> {
            self.mems.clone()
        }
        fn recent_sessions(&self, _user_id: &str, n: usize) -> Vec<String> {
            self.sessions.iter().take(n).cloned().collect()
        }
    }

    fn mems(n: usize) -> Vec<DigestMemory> {
        (0..n)
            .map(|i| DigestMemory::new(format!("fact number {i}"), (i % 7) as u32, "semantic"))
            .collect()
    }

    fn digest_path(dir: &Path) -> PathBuf {
        dir.join(DIGEST_FILENAME)
    }

    #[test]
    fn immediate_trigger_at_five() {
        assert!(!KnowledgeDigestBuilder::should_trigger_immediate(0));
        assert!(!KnowledgeDigestBuilder::should_trigger_immediate(4));
        assert!(KnowledgeDigestBuilder::should_trigger_immediate(5));
        assert!(KnowledgeDigestBuilder::should_trigger_immediate(50));
    }

    #[test]
    fn zero_memories_cold_start() {
        let dir = tempdir().unwrap();
        let src = FakeSource::new(vec![], vec![]);
        let gen = MockGenerator::returning("should not be used");
        let out = KnowledgeDigestBuilder::new()
            .reconstruct("operator", &src, &gen, dir.path())
            .unwrap();
        assert_eq!(out, COLD_START_DIGEST);
        assert_eq!(std::fs::read_to_string(digest_path(dir.path())).unwrap(), COLD_START_DIGEST);
    }

    #[test]
    fn few_memories_use_list_format() {
        let dir = tempdir().unwrap();
        let src = FakeSource::new(
            vec![
                DigestMemory::new("likes strong coffee", 3, "preference"),
                DigestMemory::new("works in field marketing", 5, "semantic"),
            ],
            vec!["talked about the homelab".into()],
        );
        // Canned text that must NOT appear — proves no narrative LLM path taken.
        let gen = MockGenerator::returning("NARRATIVE FROM LLM");
        let out = KnowledgeDigestBuilder::new()
            .reconstruct("operator", &src, &gen, dir.path())
            .unwrap();
        assert!(out.starts_with("What I know so far:"));
        assert!(out.contains("likes strong coffee"));
        assert!(out.contains("works in field marketing"));
        assert!(!out.contains("NARRATIVE FROM LLM"));
        assert_eq!(std::fs::read_to_string(digest_path(dir.path())).unwrap(), out);
    }

    #[test]
    fn narrative_prompt_includes_all_memories_and_sessions() {
        let dir = tempdir().unwrap();
        let memories: Vec<DigestMemory> = (0..12)
            .map(|i| DigestMemory::new(format!("UNIQUEFACT{i}"), 1, "semantic"))
            .collect();
        let sessions = vec!["SESSIONALPHA".to_string(), "SESSIONBETA".to_string()];
        let src = FakeSource::new(memories.clone(), sessions.clone());
        // Echo generator: returns the user prompt so we can inspect it.
        let gen = MockGenerator::default();
        let out = KnowledgeDigestBuilder::new()
            .reconstruct("operator", &src, &gen, dir.path())
            .unwrap();
        // The echoed prompt (capped) should contain the spec's framing, every
        // memory, and every session theme.
        assert!(out.contains("rebuilding your knowledge of a person"));
        for m in &memories {
            assert!(out.contains(&m.content), "missing memory {} in prompt", m.content);
        }
        for s in &sessions {
            assert!(out.contains(s), "missing session {s} in prompt");
        }
    }

    #[test]
    fn output_enforced_to_300_tokens() {
        let dir = tempdir().unwrap();
        let src = FakeSource::new(mems(12), vec![]);
        // A very long generation that must be truncated.
        let huge = "lorem ipsum ".repeat(2000);
        let gen = MockGenerator::returning(huge);
        let out = KnowledgeDigestBuilder::new()
            .reconstruct("operator", &src, &gen, dir.path())
            .unwrap();
        assert!(
            estimate_tokens(&out) <= DIGEST_TOKEN_LIMIT,
            "tokens {} exceed limit",
            estimate_tokens(&out)
        );
        assert!(out.ends_with('…'));
    }

    #[test]
    fn file_written_to_correct_path() {
        let dir = tempdir().unwrap();
        let src = FakeSource::new(mems(12), vec![]);
        let gen = MockGenerator::returning("A coherent summary of the person.");
        let out = KnowledgeDigestBuilder::new()
            .reconstruct("operator", &src, &gen, dir.path())
            .unwrap();
        let written = std::fs::read_to_string(digest_path(dir.path())).unwrap();
        assert_eq!(written, out);
        assert_eq!(written, "A coherent summary of the person.");
    }

    #[test]
    fn failed_generation_preserves_previous_digest() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        let previous = "PREVIOUS GOOD DIGEST";
        std::fs::write(digest_path(dir.path()), previous).unwrap();

        let src = FakeSource::new(mems(12), vec![]);
        let gen = FailingGenerator;
        let err = KnowledgeDigestBuilder::new()
            .reconstruct("operator", &src, &gen, dir.path())
            .unwrap_err();
        assert!(matches!(err, LuminaError::Chord(_)));
        // Existing digest must be untouched.
        assert_eq!(std::fs::read_to_string(digest_path(dir.path())).unwrap(), previous);
    }

    #[test]
    fn empty_generation_preserves_previous_digest() {
        let dir = tempdir().unwrap();
        let previous = "PREVIOUS GOOD DIGEST";
        std::fs::write(digest_path(dir.path()), previous).unwrap();
        let src = FakeSource::new(mems(12), vec![]);
        let gen = MockGenerator::returning("   \n  ");
        let err = KnowledgeDigestBuilder::new()
            .reconstruct("operator", &src, &gen, dir.path())
            .unwrap_err();
        assert!(matches!(err, LuminaError::Internal(_)));
        assert_eq!(std::fs::read_to_string(digest_path(dir.path())).unwrap(), previous);
    }

    #[test]
    fn over_cap_samples_500_weighted_by_access_count() {
        // 600 low-access + 50 very-high-access memories. Weighted sampling
        // should overwhelmingly keep the high-access ones.
        let mut all: Vec<DigestMemory> = (0..600)
            .map(|i| DigestMemory::new(format!("low{i}"), 0, "semantic"))
            .collect();
        for i in 0..50 {
            all.push(DigestMemory::new(format!("HIGH{i}"), 100_000, "principle"));
        }
        let kept = sample_weighted(all, MAX_MEMORIES);
        assert_eq!(kept.len(), MAX_MEMORIES);
        let high_kept = kept.iter().filter(|m| m.content.starts_with("HIGH")).count();
        // All 50 high-weight memories should survive sampling.
        assert_eq!(high_kept, 50, "expected all high-access memories retained");
    }

    #[test]
    fn sampling_applied_in_reconstruct_for_large_sets() {
        let dir = tempdir().unwrap();
        // 700 memories returned by source; reconstruct must cap to 500.
        let big = mems(700);
        let src = FakeSource::new(big, vec![]);
        let gen = MockGenerator::default(); // echo prompt
        let out = KnowledgeDigestBuilder::new()
            .reconstruct("operator", &src, &gen, dir.path())
            .unwrap();
        // The prompt header reports the sampled count, not 700.
        assert!(out.contains(&format!("Below are {MAX_MEMORIES} facts")));
    }

    #[test]
    fn sample_weighted_noop_when_under_cap() {
        let m = mems(10);
        let kept = sample_weighted(m.clone(), MAX_MEMORIES);
        assert_eq!(kept.len(), 10);
    }

    #[test]
    fn no_personal_or_infra_data_in_constants() {
        for c in [COLD_START_DIGEST, RECONSTRUCTION_SYSTEM, DEEP_MODEL_HINT] {
            assert!(!c.contains("the operator"));
            assert!(!c.contains("192.168"));
            assert!(!c.contains("Foster City"));
        }
    }
}
