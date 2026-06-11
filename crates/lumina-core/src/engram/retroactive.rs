//! DPROMPT-16 — Retroactive extraction recovery.
//!
//! The live ingestion pipeline (EMEM-04) rate-limits at 5 memories per turn
//! (`prompts::MAX_MEMORIES_PER_TURN`). Dense conversations therefore lose
//! facts: the 6th, 7th, … memory of a turn is silently dropped. The raw
//! conversation archive (DPROMPT-08) still holds the *full* transcript, but
//! those un-extracted facts are not individually searchable in Engram.
//!
//! `RetroactiveExtractor` runs during nightly sleep-time. It scans the last
//! 24h of archived turns for facts that were MENTIONED but NOT extracted, then
//! re-extracts them WITHOUT the 5/turn limit using the `lumina-deep` model
//! (these facts were important enough to mention and missed by the limit, so
//! they deserve the better model). Newly recovered facts are deduplicated
//! against existing memories and stored as Semantic memories tagged
//! `retroactive_extraction`, capped at [`MAX_RETROACTIVE_PER_RUN`] per night.
//!
//! ## Testability seams
//! The extractor depends on three injected seams so it can be unit-tested
//! without a live SQLCipher DB or an embedding backend:
//! - [`ArchiveSource`]   — supplies recent turns + what was already extracted.
//! - [`MemoryWriter`]    — dedup check + write of recovered facts.
//! - [`LlmGenerator`]    — the shared sleep-time LLM seam (`prompt::llm`).
//!
//! Production wires `ArchiveSource` over [`crate::engram::archive::ConversationArchive`]
//! + the per-user `EngramStore` (to read the turn's already-extracted memories),
//! and `MemoryWriter` over the per-user `EngramStore` (embedding-backed
//! `exists_similar` via cosine > 0.9, and `store_semantic` via `insert_memory`).
//!
//! No network, no chrono — `since_secs` is passed in by the caller.

use crate::error::Result;
use crate::prompt::llm::LlmGenerator;

/// Model tier used for retroactive extraction.
///
/// Per the spec, retroactive extraction uses `lumina-deep` (the larger model):
/// these facts were salient enough to be mentioned but were dropped by the live
/// 5/turn rate limit, so they warrant higher extraction quality than the
/// `lumina-fast` model used by the live ingest path.
pub const RETROACTIVE_MODEL: &str = "lumina-deep";

/// Source tag applied to every retroactively recovered memory. Lets the
/// Knowledge Digest / audits distinguish recovered facts from live extractions.
pub const RETROACTIVE_SOURCE_TAG: &str = "retroactive_extraction";

/// Hard cap on recovered memories stored per nightly run — prevents a very
/// dense day from flooding the archive.
pub const MAX_RETROACTIVE_PER_RUN: usize = 20;

/// A single live-rate-limited turn, hit at 5 extractions per turn.
const LIVE_RATE_LIMIT: usize = crate::engram::prompts::MAX_MEMORIES_PER_TURN;

// ── Seams ───────────────────────────────────────────────────────────────────

/// One archived conversation turn, paired with what live ingestion extracted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredTurn {
    /// Full text of the turn (user + assistant content), as archived.
    pub turn_text: String,
    /// Memory contents that live ingestion already stored from this turn.
    pub extracted_memories: Vec<String>,
    /// True if live ingestion hit the 5/turn rate limit on this turn (i.e. it
    /// extracted exactly the limit, so additional facts were likely dropped).
    pub hit_rate_limit: bool,
}

/// Supplies recent archived turns for retroactive scanning.
///
/// Production wraps [`crate::engram::archive::ConversationArchive`] (for turn
/// text) joined with the per-user `EngramStore` (for the memories created from
/// each turn, looked up via `source_conversation_id` / `source_turn_index`).
pub trait ArchiveSource {
    /// Return turns for `user_id` whose archived `ended_at` is at or after
    /// `since_secs` (an absolute Unix-second cutoff supplied by the caller —
    /// e.g. `now - 86_400` for "last 24h").
    fn recent_turns(&self, user_id: &str, since_secs: i64) -> Vec<RecoveredTurn>;
}

/// Dedup + write seam for recovered facts.
///
/// Production wraps the per-user `EngramStore`: `exists_similar` embeds `text`
/// and checks cosine > 0.9 against existing memories; `store_semantic` inserts a
/// `Semantic` memory with the retroactive source tag.
pub trait MemoryWriter {
    /// Whether a memory similar enough to `text` already exists (dedup guard —
    /// production uses cosine > 0.9 per the spec).
    fn exists_similar(&self, text: &str) -> bool;

    /// Store `text` as a Semantic memory for `user_id`, tagged `source_tag`.
    fn store_semantic(&self, user_id: &str, text: &str, source_tag: &str) -> Result<()>;
}

// ── RetroactiveExtractor ─────────────────────────────────────────────────────

/// Nightly retroactive extraction recovery. Stateless — all inputs flow through
/// method parameters, mirroring `MemoryIngestor`.
pub struct RetroactiveExtractor;

impl RetroactiveExtractor {
    /// Scan recent turns and return those that warrant re-extraction.
    ///
    /// A turn is flagged when EITHER:
    /// 1. it hit the live rate limit (`hit_rate_limit` — facts were almost
    ///    certainly dropped beyond the 5th), OR
    /// 2. its content is not "covered" by its extracted memories.
    ///
    /// ## "Covered" heuristic
    /// Without an embedder available in this seam, coverage is approximated by
    /// token overlap: tokenize the turn text and the concatenation of its
    /// extracted memories into lowercased alphanumeric tokens (length ≥ 3,
    /// stop-words removed). The turn is considered "covered" when at least
    /// [`COVERAGE_THRESHOLD`] of its content tokens also appear among the
    /// extracted-memory tokens. A turn that extracted nothing but contains
    /// content tokens is NOT covered. This is intentionally conservative — the
    /// LLM re-extraction step (and `MemoryWriter::exists_similar`) are the real
    /// quality/dedup gates; this heuristic only decides which turns to spend an
    /// LLM call on.
    pub fn scan_unextracted(
        &self,
        user_id: &str,
        since_secs: i64,
        source: &dyn ArchiveSource,
    ) -> Vec<RecoveredTurn> {
        source
            .recent_turns(user_id, since_secs)
            .into_iter()
            .filter(|turn| turn.hit_rate_limit || !is_covered(turn))
            .collect()
    }

    /// Re-extract missing facts from `turns`, dedup, store, and return the count
    /// of memories actually recovered.
    ///
    /// For each flagged turn:
    /// 1. build the retroactive extraction prompt (includes the turn text AND
    ///    the already-recorded memories so the LLM extracts only *additional*
    ///    facts),
    /// 2. call `gen.generate(RETROACTIVE_MODEL, ..)` (one fact per line),
    /// 3. parse, skip blanks / facts already named in the turn's own extracted
    ///    set, dedup via `writer.exists_similar`,
    /// 4. store via `writer.store_semantic(.., RETROACTIVE_SOURCE_TAG)`,
    ///    stopping once [`MAX_RETROACTIVE_PER_RUN`] have been stored this run.
    ///
    /// LLM / store failures on a single turn are non-fatal and skipped.
    pub fn extract_missing(
        &self,
        user_id: &str,
        turns: &[RecoveredTurn],
        gen: &dyn LlmGenerator,
        writer: &dyn MemoryWriter,
    ) -> Result<usize> {
        let mut recovered = 0usize;

        for turn in turns {
            if recovered >= MAX_RETROACTIVE_PER_RUN {
                break;
            }

            let prompt = retroactive_prompt(&turn.turn_text, &turn.extracted_memories);
            let raw = match gen.generate(RETROACTIVE_MODEL, RETROACTIVE_SYSTEM_PROMPT, &prompt) {
                Ok(text) => text,
                Err(e) => {
                    eprintln!("engram/retroactive: LLM generate failed (non-fatal): {e}");
                    continue;
                }
            };

            // Facts already recorded from this turn (case-insensitive) — never re-store.
            let already: Vec<String> = turn
                .extracted_memories
                .iter()
                .map(|m| m.trim().to_lowercase())
                .collect();

            for fact in parse_facts(&raw) {
                if recovered >= MAX_RETROACTIVE_PER_RUN {
                    break;
                }
                let key = fact.trim().to_lowercase();
                if already.iter().any(|a| a == &key) {
                    continue; // already recorded live from this turn
                }
                if writer.exists_similar(&fact) {
                    continue; // dedup against the wider memory store
                }
                match writer.store_semantic(user_id, &fact, RETROACTIVE_SOURCE_TAG) {
                    Ok(()) => recovered += 1,
                    Err(e) => {
                        eprintln!("engram/retroactive: store_semantic failed (non-fatal): {e}");
                    }
                }
            }
        }

        eprintln!(
            "engram/retroactive: recovered {recovered} facts from {} flagged turns",
            turns.len()
        );
        Ok(recovered)
    }
}

// ── Prompt ───────────────────────────────────────────────────────────────────

/// System prompt for retroactive extraction. Keeps the model focused on
/// surfacing *additional* durable facts, not re-stating what's already recorded.
pub const RETROACTIVE_SYSTEM_PROMPT: &str =
    "You recover facts that were mentioned in a conversation but not previously \
     saved to memory. Extract only ADDITIONAL durable facts, one per line, with \
     no numbering or preamble.";

/// Build the retroactive extraction prompt for one turn.
///
/// Mirrors the spec's template: the full turn text, the list of already-recorded
/// memories, and an instruction to extract only ADDITIONAL facts not already
/// covered. Output is one fact per line (parsed by [`parse_facts`]).
pub fn retroactive_prompt(turn_text: &str, existing_memories: &[String]) -> String {
    let already = if existing_memories.is_empty() {
        "(none)".to_string()
    } else {
        existing_memories
            .iter()
            .map(|m| format!("- {}", m.trim()))
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        r#"These facts were mentioned in a conversation but may not have been fully recorded in memory. Extract them as individual memories.

Conversation turn:
{turn_text}

Already recorded from this turn:
{already}

Extract any ADDITIONAL facts worth remembering that are not already covered above. Focus on specific details, dates, names, preferences, and decisions. Write one fact per line. If there is nothing additional worth remembering, write nothing."#,
        turn_text = turn_text,
        already = already,
    )
}

/// Parse the LLM completion into individual facts (one per line).
///
/// Strips blank lines, common list bullets/numbering, and a literal "(none)".
fn parse_facts(raw: &str) -> Vec<String> {
    raw.lines()
        .map(strip_bullet)
        .filter(|l| !l.is_empty())
        .filter(|l| !l.eq_ignore_ascii_case("(none)") && !l.eq_ignore_ascii_case("none"))
        .map(|l| l.to_string())
        .collect()
}

/// Strip a leading list marker (`-`, `*`, `•`, or `N.` / `N)`) and surrounding
/// whitespace from a single line.
fn strip_bullet(line: &str) -> &str {
    let t = line.trim();
    if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")).or_else(|| t.strip_prefix("• ")) {
        return rest.trim();
    }
    // Numbered list: "1. fact" / "1) fact"
    let bytes = t.as_bytes();
    let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits > 0 && digits < t.len() {
        let after = &t[digits..];
        if let Some(rest) = after.strip_prefix(". ").or_else(|| after.strip_prefix(") ")) {
            return rest.trim();
        }
    }
    t
}

// ── Coverage heuristic ───────────────────────────────────────────────────────

/// Fraction of a turn's content tokens that must appear among its extracted
/// memories for the turn to count as "covered" (and thus skipped, unless it
/// also hit the rate limit).
const COVERAGE_THRESHOLD: f64 = 0.6;

/// Tokens shorter than this are ignored (articles, fillers).
const MIN_TOKEN_LEN: usize = 3;

/// Decide whether a turn's content is covered by its extracted memories using
/// the token-overlap heuristic documented on [`RetroactiveExtractor::scan_unextracted`].
fn is_covered(turn: &RecoveredTurn) -> bool {
    let content_tokens = tokenize(&turn.turn_text);
    if content_tokens.is_empty() {
        return true; // no meaningful content → nothing to recover
    }
    let mut mem_tokens = std::collections::HashSet::new();
    for m in &turn.extracted_memories {
        for tok in tokenize(m) {
            mem_tokens.insert(tok);
        }
    }
    if mem_tokens.is_empty() {
        return false; // content present but nothing extracted → not covered
    }

    let unique: std::collections::HashSet<&String> = content_tokens.iter().collect();
    let covered = unique.iter().filter(|t| mem_tokens.contains(**t)).count();
    (covered as f64) / (unique.len() as f64) >= COVERAGE_THRESHOLD
}

/// Lowercased alphanumeric tokens of length ≥ [`MIN_TOKEN_LEN`], stop-words removed.
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .map(|w| w.to_lowercase())
        .filter(|w| w.len() >= MIN_TOKEN_LEN && !is_stop_word(w))
        .collect()
}

/// A small English stop-word set so common glue words don't inflate coverage.
fn is_stop_word(w: &str) -> bool {
    matches!(
        w,
        "the" | "and" | "for" | "are" | "was" | "you" | "your" | "with" | "this"
            | "that" | "have" | "has" | "had" | "but" | "not" | "all" | "any"
            | "can" | "will" | "from" | "they" | "them" | "about"
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::llm::MockGenerator;
    use std::cell::RefCell;

    // ── Fakes ────────────────────────────────────────────────────────────────

    /// Fake archive returning a canned set of turns, ignoring the cutoff except
    /// to record that it was forwarded.
    struct FakeArchive {
        turns: Vec<RecoveredTurn>,
        seen_since: RefCell<Option<i64>>,
    }
    impl FakeArchive {
        fn new(turns: Vec<RecoveredTurn>) -> Self {
            Self { turns, seen_since: RefCell::new(None) }
        }
    }
    impl ArchiveSource for FakeArchive {
        fn recent_turns(&self, _user_id: &str, since_secs: i64) -> Vec<RecoveredTurn> {
            *self.seen_since.borrow_mut() = Some(since_secs);
            self.turns.clone()
        }
    }

    /// Fake writer: records every stored (text, tag) and can pre-seed
    /// "already exists" facts for dedup tests.
    struct FakeWriter {
        existing: Vec<String>,
        stored: RefCell<Vec<(String, String)>>, // (text, tag)
    }
    impl FakeWriter {
        fn new() -> Self {
            Self { existing: Vec::new(), stored: RefCell::new(Vec::new()) }
        }
        fn with_existing(existing: Vec<&str>) -> Self {
            Self {
                existing: existing.into_iter().map(String::from).collect(),
                stored: RefCell::new(Vec::new()),
            }
        }
        fn stored(&self) -> Vec<(String, String)> {
            self.stored.borrow().clone()
        }
    }
    impl MemoryWriter for FakeWriter {
        fn exists_similar(&self, text: &str) -> bool {
            let key = text.trim().to_lowercase();
            self.existing.iter().any(|e| e.to_lowercase() == key)
                || self.stored.borrow().iter().any(|(t, _)| t.to_lowercase() == key)
        }
        fn store_semantic(&self, _user_id: &str, text: &str, source_tag: &str) -> Result<()> {
            self.stored.borrow_mut().push((text.to_string(), source_tag.to_string()));
            Ok(())
        }
    }

    fn turn(text: &str, extracted: &[&str], hit: bool) -> RecoveredTurn {
        RecoveredTurn {
            turn_text: text.to_string(),
            extracted_memories: extracted.iter().map(|s| s.to_string()).collect(),
            hit_rate_limit: hit,
        }
    }

    // ── scan_unextracted ──────────────────────────────────────────────────────

    #[test]
    fn test_rate_limited_turn_is_flagged() {
        // A turn that hit the limit is flagged even if it looks "covered".
        let t = turn("I like dark roast coffee", &["likes dark roast coffee"], true);
        let arc = FakeArchive::new(vec![t]);
        let out = RetroactiveExtractor.scan_unextracted("alice", 1000, &arc);
        assert_eq!(out.len(), 1, "rate-limited turn must be flagged");
        assert_eq!(*arc.seen_since.borrow(), Some(1000), "cutoff forwarded to source");
    }

    #[test]
    fn test_unextracted_content_flagged() {
        // Turn mentions a lot but extracted nothing → not covered → flagged.
        let t = turn(
            "My dentist appointment is Tuesday and my sister Maria visits Friday",
            &[],
            false,
        );
        let arc = FakeArchive::new(vec![t]);
        let out = RetroactiveExtractor.scan_unextracted("alice", 0, &arc);
        assert_eq!(out.len(), 1, "turn with content but no extractions is unextracted");
    }

    #[test]
    fn test_well_covered_turn_not_flagged() {
        // Extracted memories cover the salient tokens → skipped.
        let t = turn(
            "dentist appointment Tuesday",
            &["dentist appointment is Tuesday"],
            false,
        );
        let arc = FakeArchive::new(vec![t]);
        let out = RetroactiveExtractor.scan_unextracted("alice", 0, &arc);
        assert!(out.is_empty(), "fully covered, non-rate-limited turn is skipped");
    }

    #[test]
    fn test_empty_content_turn_not_flagged() {
        let t = turn("   ", &[], false);
        let arc = FakeArchive::new(vec![t]);
        let out = RetroactiveExtractor.scan_unextracted("alice", 0, &arc);
        assert!(out.is_empty(), "empty content has nothing to recover");
    }

    // ── extract_missing ───────────────────────────────────────────────────────

    #[test]
    fn test_recovers_new_facts_with_source_tag() {
        let t = turn("dense turn", &["already known"], true);
        let gen = MockGenerator::returning("Maria's birthday is March 3\nHe prefers window seats");
        let writer = FakeWriter::new();
        let n = RetroactiveExtractor
            .extract_missing("alice", &[t], &gen, &writer)
            .unwrap();
        assert_eq!(n, 2, "both new facts recovered");
        let stored = writer.stored();
        assert_eq!(stored.len(), 2);
        for (_, tag) in &stored {
            assert_eq!(tag, RETROACTIVE_SOURCE_TAG, "every recovered fact tagged source");
        }
        assert!(stored.iter().any(|(t, _)| t.contains("Maria")));
    }

    #[test]
    fn test_dedup_prevents_restore() {
        let t = turn("dense turn", &[], true);
        // The store already knows this fact → must not be re-stored.
        let gen = MockGenerator::returning("likes dark roast coffee\na genuinely new fact");
        let writer = FakeWriter::with_existing(vec!["likes dark roast coffee"]);
        let n = RetroactiveExtractor
            .extract_missing("alice", &[t], &gen, &writer)
            .unwrap();
        assert_eq!(n, 1, "duplicate skipped, only the new fact stored");
        assert_eq!(writer.stored().len(), 1);
        assert!(writer.stored()[0].0.contains("genuinely new"));
    }

    #[test]
    fn test_already_recorded_from_turn_not_restored() {
        // A fact identical to one already extracted live from THIS turn is skipped.
        let t = turn("dense turn", &["window seat preference"], true);
        let gen = MockGenerator::returning("window seat preference\nnew detail here");
        let writer = FakeWriter::new();
        let n = RetroactiveExtractor
            .extract_missing("alice", &[t], &gen, &writer)
            .unwrap();
        assert_eq!(n, 1, "fact already recorded from the turn is not re-stored");
        assert!(writer.stored()[0].0.contains("new detail"));
    }

    #[test]
    fn test_max_20_cap_enforced() {
        // 30 facts offered across turns → only 20 stored.
        let many: String = (0..30)
            .map(|i| format!("unique fact number {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let gen = MockGenerator::returning(many);
        let turns = vec![turn("dense", &[], true)];
        let writer = FakeWriter::new();
        let n = RetroactiveExtractor
            .extract_missing("alice", &turns, &gen, &writer)
            .unwrap();
        assert_eq!(n, MAX_RETROACTIVE_PER_RUN, "must cap at 20 per run");
        assert_eq!(writer.stored().len(), MAX_RETROACTIVE_PER_RUN);
    }

    #[test]
    fn test_cap_spans_multiple_turns() {
        use crate::prompt::llm::LlmGenerator;
        use std::sync::atomic::{AtomicUsize, Ordering};

        // A generator returning 15 *distinct* facts per call, so two turns
        // produce 30 unique candidates and the 20-cap actually bites
        // (identical facts would be collapsed by dedup, never reaching the cap).
        struct SeqGenerator { call: AtomicUsize }
        impl LlmGenerator for SeqGenerator {
            fn generate(&self, _h: &str, _s: &str, _u: &str) -> Result<String> {
                let n = self.call.fetch_add(1, Ordering::SeqCst);
                Ok((0..15).map(|i| format!("fact {n}-{i}")).collect::<Vec<_>>().join("\n"))
            }
        }

        let gen = SeqGenerator { call: AtomicUsize::new(0) };
        let turns = vec![turn("t1", &[], true), turn("t2", &[], true)];
        let writer = FakeWriter::new();
        let n = RetroactiveExtractor
            .extract_missing("alice", &turns, &gen, &writer)
            .unwrap();
        assert_eq!(n, MAX_RETROACTIVE_PER_RUN, "cap holds across turns");
    }

    #[test]
    fn test_empty_completion_recovers_nothing() {
        let gen = MockGenerator::returning("(none)");
        let turns = vec![turn("t", &["x"], true)];
        let writer = FakeWriter::new();
        let n = RetroactiveExtractor
            .extract_missing("alice", &turns, &gen, &writer)
            .unwrap();
        assert_eq!(n, 0);
        assert!(writer.stored().is_empty());
    }

    // ── prompt ────────────────────────────────────────────────────────────────

    #[test]
    fn test_prompt_includes_existing_memories() {
        let p = retroactive_prompt(
            "User: I moved to a new place",
            &["lives in Foster City".to_string()],
        );
        assert!(p.contains("Already recorded from this turn"));
        assert!(p.contains("lives in Foster City"), "existing memories must be in prompt");
        assert!(p.contains("I moved to a new place"), "turn text must be in prompt");
        assert!(p.to_lowercase().contains("additional"), "asks for additional facts only");
    }

    #[test]
    fn test_prompt_handles_no_existing_memories() {
        let p = retroactive_prompt("some turn", &[]);
        assert!(p.contains("(none)"), "empty existing list renders as (none)");
    }

    #[test]
    fn test_uses_deep_model() {
        assert_eq!(RETROACTIVE_MODEL, "lumina-deep", "retroactive uses the deep model");
    }

    // ── parsing / helpers ─────────────────────────────────────────────────────

    #[test]
    fn test_parse_facts_strips_bullets_and_numbering() {
        let raw = "- first fact\n* second fact\n1. third fact\n2) fourth fact\n\n  \nplain fifth";
        let facts = parse_facts(raw);
        assert_eq!(facts, vec!["first fact", "second fact", "third fact", "fourth fact", "plain fifth"]);
    }

    #[test]
    fn test_live_rate_limit_matches_ingest() {
        assert_eq!(LIVE_RATE_LIMIT, 5, "must track the live MAX_MEMORIES_PER_TURN");
    }

    #[test]
    fn test_no_hardcoded_ips() {
        assert!(!RETROACTIVE_MODEL.contains("192.168"));
        assert!(!RETROACTIVE_MODEL.starts_with("http"));
        assert!(!RETROACTIVE_SYSTEM_PROMPT.contains("192.168"));
    }
}
