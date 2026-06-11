//! HRNS-07: Curated research results → Engram knowledge ingestion.
//!
//! When Harness-1 curates high-quality documents during a deep-research turn,
//! the key findings are ingested into Engram as **Semantic** memories. Lumina
//! doesn't just answer the question — she LEARNS from the research for future
//! reference.
//!
//! Input: the agentic execution log. HRNS-05 records one `research_source` step
//! per curated document (title + url + importance + BM25-compressed summary —
//! metadata only, never full document text). This module reads those steps.
//!
//! Pipeline (`ResearchIngestor::ingest_curated_set`):
//!  1. Filter to `very_high` and `high` importance only.
//!  2. Cap at 5 per session — if more qualify, take the top 5 by importance,
//!     then recency (later harness turn wins).
//!  3. For each: build a 1–2 sentence summary, embed it, dedup against existing
//!     memories (cosine > 0.9 → skip), and store as a Semantic memory with
//!     tags `["research", "{topic}", "source:{url}"]`, provenance =
//!     `source_conversation_id`, and confidence mapped from importance
//!     (very_high = 0.95, high = 0.80).
//!  4. Per-user isolation: memories belong to the asking user's store.
//!
//! Non-blocking: designed to be `tokio::spawn`-ed AFTER the agentic response is
//! delivered. All failures are logged and NEVER propagate.
//!
//! No hardcoded infrastructure values anywhere in this file.

use crate::chord::AgenticExecutionStep;
use crate::config::Config;
use crate::engram::types::{Memory, MemoryType, SensitivityCategory};
use crate::engram::{cosine, embed, EngramStore};

/// Maximum number of new research memories stored per research session.
pub const MAX_RESEARCH_MEMORIES_PER_SESSION: usize = 5;

/// Cosine-similarity threshold above which a candidate is treated as a
/// near-duplicate of an existing memory and skipped (HRNS-07 dedup).
pub const RESEARCH_DEDUP_THRESHOLD: f32 = 0.9;

/// Confidence assigned to a `very_high` importance source.
pub const CONFIDENCE_VERY_HIGH: f32 = 0.95;
/// Confidence assigned to a `high` importance source.
pub const CONFIDENCE_HIGH: f32 = 0.80;

/// A curated document distilled from a `research_source` execution-log step,
/// after importance filtering. Internal to the ingestor.
#[derive(Debug, Clone)]
struct Candidate {
    title: String,
    url: String,
    /// Importance rank: 3 = very_high, 2 = high. (fair/low never reach here.)
    rank: u8,
    confidence: f32,
    summary: String,
    added_at_turn: usize,
}

/// Ingests curated research documents from an agentic execution log into Engram.
pub struct ResearchIngestor;

impl ResearchIngestor {
    /// Returns `true` if any step in the execution log is a curated research
    /// source — i.e. this turn ran the harness and produced documents worth
    /// considering for ingestion. Cheap pre-check for the call site.
    pub fn has_research_sources(exec_log: &[AgenticExecutionStep]) -> bool {
        exec_log
            .iter()
            .any(|s| s.step_type == "research_source" && s.research_source.is_some())
    }

    /// Map an importance label to (rank, confidence). Returns `None` for any
    /// importance below `high` (fair/low/unknown are excluded from ingestion).
    fn importance_to_rank_confidence(importance: &str) -> Option<(u8, f32)> {
        match importance.trim().to_ascii_lowercase().as_str() {
            "very_high" | "veryhigh" => Some((3, CONFIDENCE_VERY_HIGH)),
            "high" => Some((2, CONFIDENCE_HIGH)),
            _ => None, // fair, low, unknown → excluded
        }
    }

    /// Extract a 1–2 sentence summary from a curated document's compressed text.
    ///
    /// Per the de-bloat rules this is pure Python-equivalent string work — no
    /// LLM call. Takes the first 1–2 sentences of the BM25-compressed summary
    /// (the model already saw these as the most relevant sentences). Falls back
    /// to the title when no summary text is present.
    fn summarize(title: &str, compressed: &str) -> String {
        let text = compressed.trim();
        if text.is_empty() {
            return title.trim().to_string();
        }

        // Split into sentences on '.', '!', '?' boundaries, keeping the
        // terminator. Take the first 1–2 non-trivial sentences.
        let mut sentences: Vec<String> = Vec::new();
        let mut current = String::new();
        for ch in text.chars() {
            current.push(ch);
            if matches!(ch, '.' | '!' | '?') {
                let trimmed = current.trim();
                if trimmed.len() > 1 {
                    sentences.push(trimmed.to_string());
                }
                current.clear();
                if sentences.len() == 2 {
                    break;
                }
            }
        }
        // Capture a trailing fragment with no terminator (only if we have <2).
        if sentences.len() < 2 {
            let trimmed = current.trim();
            if trimmed.len() > 1 {
                sentences.push(trimmed.to_string());
            }
        }

        if sentences.is_empty() {
            return title.trim().to_string();
        }
        sentences.join(" ")
    }

    /// Filter + rank the execution log's `research_source` steps into the
    /// top-N candidates eligible for ingestion (very_high/high only, max 5,
    /// ordered by importance then recency).
    ///
    /// Pure / synchronous — no I/O. Exposed `pub(crate)` for unit testing.
    pub(crate) fn select_candidates(exec_log: &[AgenticExecutionStep]) -> Vec<Candidate> {
        let mut candidates: Vec<Candidate> = exec_log
            .iter()
            .filter(|s| s.step_type == "research_source")
            .filter_map(|s| s.research_source.as_ref())
            .filter_map(|rs| {
                let (rank, confidence) =
                    Self::importance_to_rank_confidence(&rs.importance)?;
                Some(Candidate {
                    title: rs.title.clone(),
                    url: rs.url.clone(),
                    rank,
                    confidence,
                    summary: Self::summarize(&rs.title, &rs.summary),
                    added_at_turn: rs.added_at_turn,
                })
            })
            .collect();

        // Order: importance (rank desc) then recency (later turn first).
        candidates.sort_by(|a, b| {
            b.rank
                .cmp(&a.rank)
                .then_with(|| b.added_at_turn.cmp(&a.added_at_turn))
        });

        candidates.truncate(MAX_RESEARCH_MEMORIES_PER_SESSION);
        candidates
    }

    /// Derive a short topic slug for the research tag from the user's query.
    ///
    /// Lower-cased, alphanumerics + spaces collapsed to hyphens, truncated.
    /// Pure string work — no LLM.
    fn topic_slug(topic: &str) -> String {
        let cleaned: String = topic
            .trim()
            .to_ascii_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { ' ' })
            .collect();
        let slug = cleaned
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("-");
        if slug.is_empty() {
            "general".to_string()
        } else {
            slug.chars().take(60).collect()
        }
    }

    /// Build the typed Semantic memory for one candidate (no embedding yet).
    fn build_memory(
        cand: &Candidate,
        user_id: &str,
        topic_slug: &str,
        source_conversation_id: Option<String>,
    ) -> Memory {
        let mut mem = Memory::new(
            user_id,
            MemoryType::Semantic,
            SensitivityCategory::General,
            cand.summary.clone(),
        );
        mem.confidence = cand.confidence;
        mem.source_conversation_id = source_conversation_id;
        mem.tags = vec![
            "research".to_string(),
            topic_slug.to_string(),
            format!("source:{}", cand.url),
        ];
        mem
    }

    /// Full ingestion pipeline. Designed to be `tokio::spawn`-ed AFTER the
    /// agentic response has already been delivered to the user.
    ///
    /// `topic` is the user's research query (used for the `{topic}` tag).
    /// `source_conversation_id` is the provenance for stored memories.
    /// All failures are logged and never propagate.
    pub async fn ingest_curated_set(
        user_id: String,
        topic: String,
        source_conversation_id: Option<String>,
        exec_log: Vec<AgenticExecutionStep>,
        config: Config,
    ) {
        let candidates = Self::select_candidates(&exec_log);
        if candidates.is_empty() {
            return; // 0 very_high/high documents → nothing ingested.
        }

        // Per-user isolation: open the asking user's store.
        let store = match EngramStore::open_for_user(&user_id) {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "engram/research_ingest: cannot open store for {user_id} (non-fatal): {e}"
                );
                return;
            }
        };

        let topic_slug = Self::topic_slug(&topic);

        // Step 1: embed all candidate summaries async (store NOT referenced here,
        // keeping the future Send per the engram design note).
        let mut embedded: Vec<(Candidate, Vec<f32>)> = Vec::new();
        for cand in candidates {
            match embed(&cand.summary, &config).await {
                Ok(emb) => embedded.push((cand, emb)),
                Err(e) => {
                    // Without an embedding we cannot dedup safely — skip rather
                    // than risk flooding Engram with near-duplicates.
                    eprintln!(
                        "engram/research_ingest: embedding failed for '{}': {e} (skipping)",
                        cand.title
                    );
                }
            }
        }
        if embedded.is_empty() {
            return;
        }

        // Step 2: load existing embeddings once for dedup (sync).
        let existing = match store.all_facts_with_ids() {
            Ok(f) => f,
            Err(e) => {
                eprintln!(
                    "engram/research_ingest: cannot load existing memories for dedup (non-fatal): {e}"
                );
                Vec::new()
            }
        };

        // Step 3: dedup + insert (sync — no awaits, store borrow is fine).
        // Track already-stored embeddings so two near-identical candidates in the
        // same session don't both land.
        let mut stored_embeddings: Vec<Vec<f32>> = Vec::new();
        let mut stored_count = 0usize;

        for (cand, emb) in embedded {
            // Dedup against existing memories.
            let dup_existing = existing.iter().any(|(_, _, e)| {
                !e.is_empty() && cosine(&emb, e) > RESEARCH_DEDUP_THRESHOLD
            });
            // Dedup against memories stored earlier in this same session.
            let dup_session = stored_embeddings
                .iter()
                .any(|e| cosine(&emb, e) > RESEARCH_DEDUP_THRESHOLD);

            if dup_existing || dup_session {
                continue;
            }

            let mut mem = Self::build_memory(
                &cand,
                &user_id,
                &topic_slug,
                source_conversation_id.clone(),
            );
            mem.embedding = emb.clone();

            // insert_memory enforces sensitivity privacy (EMEM-02) internally.
            match store.insert_memory(&mem) {
                Ok(()) => {
                    stored_embeddings.push(emb);
                    stored_count += 1;
                }
                Err(e) => {
                    eprintln!(
                        "engram/research_ingest: insert_memory failed (non-fatal): {e}"
                    );
                }
            }
        }

        if stored_count > 0 {
            eprintln!(
                "engram/research_ingest: stored {stored_count} research memory(ies) for user {user_id} (topic '{topic_slug}')"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chord::ResearchSource;

    fn step(title: &str, url: &str, importance: &str, summary: &str, turn: usize) -> AgenticExecutionStep {
        AgenticExecutionStep {
            step_type: "research_source".into(),
            tool_name: None,
            duration_ms: 0,
            status: "ok".into(),
            error_message: None,
            research_source: Some(ResearchSource {
                title: title.into(),
                url: url.into(),
                importance: importance.into(),
                summary: summary.into(),
                added_at_turn: turn,
            }),
        }
    }

    fn tool_step(name: &str) -> AgenticExecutionStep {
        AgenticExecutionStep {
            step_type: "tool_call".into(),
            tool_name: Some(name.into()),
            duration_ms: 10,
            status: "ok".into(),
            error_message: None,
            research_source: None,
        }
    }

    // ── Filtering: very_high/high in, fair/low out ─────────────────────────

    #[test]
    fn test_very_high_and_high_ingested_fair_low_excluded() {
        let log = vec![
            step("A", "http://a", "very_high", "Alpha finding.", 1),
            step("B", "http://b", "high", "Beta finding.", 2),
            step("C", "http://c", "fair", "Gamma finding.", 3),
            step("D", "http://d", "low", "Delta finding.", 4),
            tool_step("searxng_search"),
        ];
        let cands = ResearchIngestor::select_candidates(&log);
        assert_eq!(cands.len(), 2, "only very_high + high qualify");
        let urls: Vec<&str> = cands.iter().map(|c| c.url.as_str()).collect();
        assert!(urls.contains(&"http://a"));
        assert!(urls.contains(&"http://b"));
        assert!(!urls.contains(&"http://c"));
        assert!(!urls.contains(&"http://d"));
    }

    #[test]
    fn test_zero_high_docs_nothing_selected() {
        let log = vec![
            step("C", "http://c", "fair", "x.", 1),
            step("D", "http://d", "low", "y.", 2),
            tool_step("searxng_search"),
        ];
        assert!(ResearchIngestor::select_candidates(&log).is_empty());
    }

    #[test]
    fn test_confidence_mapping() {
        let log = vec![
            step("A", "http://a", "very_high", "x.", 1),
            step("B", "http://b", "high", "y.", 2),
        ];
        let cands = ResearchIngestor::select_candidates(&log);
        let a = cands.iter().find(|c| c.url == "http://a").unwrap();
        let b = cands.iter().find(|c| c.url == "http://b").unwrap();
        assert!((a.confidence - CONFIDENCE_VERY_HIGH).abs() < 1e-6);
        assert!((b.confidence - CONFIDENCE_HIGH).abs() < 1e-6);
    }

    // ── Max 5 + top-5 by importance then recency ───────────────────────────

    #[test]
    fn test_max_5_per_session() {
        let log: Vec<_> = (0..20)
            .map(|i| step(&format!("T{i}"), &format!("http://{i}"), "very_high", "s.", i))
            .collect();
        let cands = ResearchIngestor::select_candidates(&log);
        assert_eq!(cands.len(), MAX_RESEARCH_MEMORIES_PER_SESSION);
    }

    #[test]
    fn test_top_5_importance_then_recency() {
        // 6 high docs at turns 0..6 and 1 very_high at turn 0.
        let mut log: Vec<_> = (0..6)
            .map(|i| step(&format!("H{i}"), &format!("http://h{i}"), "high", "s.", i))
            .collect();
        log.push(step("V", "http://v", "very_high", "s.", 0));

        let cands = ResearchIngestor::select_candidates(&log);
        assert_eq!(cands.len(), 5);
        // very_high must be first (highest importance).
        assert_eq!(cands[0].url, "http://v");
        // Among the high docs, the most recent (highest turn) win: h5, h4, h3, h2.
        let high_urls: Vec<&str> = cands[1..].iter().map(|c| c.url.as_str()).collect();
        assert_eq!(high_urls, vec!["http://h5", "http://h4", "http://h3", "http://h2"]);
    }

    // ── Summary: 1–2 sentences ─────────────────────────────────────────────

    #[test]
    fn test_summary_one_to_two_sentences() {
        let s = ResearchIngestor::summarize(
            "Title",
            "First sentence here. Second sentence too. Third should be dropped. Fourth also.",
        );
        // Exactly the first two sentences.
        assert_eq!(s, "First sentence here. Second sentence too.");
        assert!(!s.contains("Third"));
    }

    #[test]
    fn test_summary_falls_back_to_title_when_empty() {
        let s = ResearchIngestor::summarize("Just A Title", "   ");
        assert_eq!(s, "Just A Title");
    }

    #[test]
    fn test_summary_single_fragment_no_terminator() {
        let s = ResearchIngestor::summarize("T", "a finding with no period");
        assert_eq!(s, "a finding with no period");
    }

    // ── Topic slug ─────────────────────────────────────────────────────────

    #[test]
    fn test_topic_slug() {
        assert_eq!(
            ResearchIngestor::topic_slug("Quantum Computing & You!"),
            "quantum-computing-you"
        );
        assert_eq!(ResearchIngestor::topic_slug("   "), "general");
    }

    // ── Memory shape: type, tags, provenance ───────────────────────────────

    #[test]
    fn test_build_memory_type_and_tags() {
        let cand = Candidate {
            title: "T".into(),
            url: "http://example.test/doc".into(),
            rank: 3,
            confidence: CONFIDENCE_VERY_HIGH,
            summary: "A useful finding.".into(),
            added_at_turn: 1,
        };
        let mem = ResearchIngestor::build_memory(
            &cand,
            "alice",
            "quantum-computing",
            Some("conv-123".into()),
        );
        assert_eq!(mem.memory_type, MemoryType::Semantic);
        assert_eq!(mem.user_id, "alice");
        assert_eq!(mem.content, "A useful finding.");
        assert!((mem.confidence - CONFIDENCE_VERY_HIGH).abs() < 1e-6);
        assert_eq!(mem.source_conversation_id, Some("conv-123".to_string()));
        assert_eq!(
            mem.tags,
            vec![
                "research".to_string(),
                "quantum-computing".to_string(),
                "source:http://example.test/doc".to_string(),
            ]
        );
    }

    #[test]
    fn test_has_research_sources() {
        assert!(ResearchIngestor::has_research_sources(&[step(
            "A", "http://a", "high", "s.", 0
        )]));
        assert!(!ResearchIngestor::has_research_sources(&[tool_step("x")]));
        assert!(!ResearchIngestor::has_research_sources(&[]));
    }

    // ── End-to-end ingestion: dedup + per-user isolation + storage ─────────

    fn test_key() -> Vec<u8> {
        vec![0u8; 32]
    }

    /// Build a store at an explicit per-user base so tests are isolated.
    fn user_store(base: &std::path::Path, user: &str) -> EngramStore {
        EngramStore::open_for_user_at(base, user, &test_key()).unwrap()
    }

    /// Insert a fact directly with a known embedding (test helper).
    /// `insert_fact` stores under the store's own `user_id`, giving us
    /// per-user isolation for free.
    fn insert_with_emb(store: &EngramStore, content: &str, emb: &[f32]) {
        store.insert_fact(content, emb).unwrap();
    }

    #[test]
    fn test_dedup_skips_near_duplicate_against_existing() {
        // Pure-sync slice of the pipeline: verify the cosine dedup decision.
        let existing_emb = vec![1.0f32, 0.0, 0.0];
        let new_emb = vec![0.999f32, 0.001, 0.0]; // cosine ~ 1.0 > 0.9
        assert!(cosine(&new_emb, &existing_emb) > RESEARCH_DEDUP_THRESHOLD);

        let distinct = vec![0.0f32, 1.0, 0.0]; // orthogonal → not a dup
        assert!(cosine(&distinct, &existing_emb) < RESEARCH_DEDUP_THRESHOLD);
    }

    #[test]
    fn test_per_user_isolation_separate_stores() {
        let dir = std::env::temp_dir().join(format!(
            "lumina_research_ingest_{}",
            crate::engram::types::new_uuid()
        ));
        let alice = user_store(&dir, "alice");
        let bob = user_store(&dir, "bob");

        insert_with_emb(&alice, "alice research finding", &[1.0, 0.0, 0.0]);

        // Bob's store must not see Alice's memory.
        let bob_facts = bob.all_facts().unwrap();
        assert!(bob_facts.is_empty(), "bob must not see alice's research memory");

        let alice_facts = alice.all_facts().unwrap();
        assert_eq!(alice_facts.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_stored_memory_is_semantic_with_research_tags() {
        let dir = std::env::temp_dir().join(format!(
            "lumina_research_store_{}",
            crate::engram::types::new_uuid()
        ));
        let store = user_store(&dir, "carol");

        let cand = Candidate {
            title: "Doc".into(),
            url: "http://example.test/x".into(),
            rank: 2,
            confidence: CONFIDENCE_HIGH,
            summary: "Carol learned a fact.".into(),
            added_at_turn: 3,
        };
        let mut mem = ResearchIngestor::build_memory(&cand, "carol", "topic-x", Some("c-9".into()));
        mem.embedding = vec![0.0, 0.0, 1.0];
        store.insert_memory(&mem).unwrap();

        let semantics = store.query_by_type(MemoryType::Semantic).unwrap();
        let found = semantics.iter().find(|m| m.content == "Carol learned a fact.").unwrap();
        assert!(found.tags.contains(&"research".to_string()));
        assert!(found.tags.contains(&"topic-x".to_string()));
        assert!(found.tags.contains(&"source:http://example.test/x".to_string()));
        assert!((found.confidence - CONFIDENCE_HIGH).abs() < 1e-6);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
