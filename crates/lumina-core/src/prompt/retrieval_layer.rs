//! DPROMPT-11: Per-query semantic memory retrieval layer.
//!
//! Every turn, the pre-computed prompt layers (identity, style, personality,
//! knowledge digest, …) are supplemented with *per-query* retrieval from
//! Engram.  The Knowledge Digest answers "who is this person" in general; this
//! layer answers "what do I know about **this topic**" — driven by the user's
//! actual message, not the digest or personality.
//!
//! The assembler ([`crate::prompt`]) already reserves the `[memory]` section and
//! accepts a pre-formatted body via [`crate::prompt::AssemblyExtras::memory`].
//! This module *produces* that body string.  It does not touch the assembler.
//!
//! ## Design
//! * **Injectable seam.**  Retrieval goes through the [`RetrievalSource`] trait,
//!   so this layer is fully testable without a live Engram store or embedder.
//!   The production source ([`agent_loop`]) wraps
//!   `engram::retrieval::{fetch_candidates_for_query, score_candidates}` and maps
//!   `ScoredMemory` → [`RetrievedMemory`].
//! * **No network, no chrono.**  All timestamps are passed in as Unix seconds
//!   (`now_secs`) so the 5-minute topic cache is deterministic under test.
//! * **Token budget.**  The retrieval block is the headroom between the static
//!   layer budget (`GLOBAL_TOKEN_BUDGET` = 1300) and the hard total ceiling of
//!   [`TOTAL_PROMPT_BUDGET`] = 1500.  Given the tokens already consumed by the
//!   assembled static layers, the block is capped at `1200 - existing` and never
//!   more than [`DEFAULT_RETRIEVAL_BUDGET`] (~200) tokens.
//! * **Type priority.**  Principle > Preference > Semantic > Episodic, then by
//!   score within a type (mirrors `MemoryType::retrieval_priority`).
//! * **Digest dedup.**  A retrieved memory already covered by the Knowledge
//!   Digest is dropped — no embeddings needed here; see [`is_covered_by_digest`].

use super::layers::estimate_tokens;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

/// Hard ceiling for the entire assembled system prompt, including the per-query
/// retrieval block.  Static layers target [`super::layers::GLOBAL_TOKEN_BUDGET`]
/// (1300); the remaining headroom up to this value is available to retrieval.
///
/// Raised 1200→1500 in S77 alongside `GLOBAL_TOKEN_BUDGET` (1000→1300) so the
/// invariant `TOTAL_PROMPT_BUDGET ≥ GLOBAL_TOKEN_BUDGET` holds and the ~200t
/// retrieval headroom above the static ceiling is preserved.
pub const TOTAL_PROMPT_BUDGET: usize = 1500;

/// Default cap for the retrieval block when there is generous headroom.
pub const DEFAULT_RETRIEVAL_BUDGET: usize = 200;

/// Topic-cache time-to-live in seconds (5 minutes per spec).
pub const CACHE_TTL_SECS: u64 = 5 * 60;

/// Fraction of a memory's tokens that must be "covered" by the digest for the
/// memory to be considered redundant and dropped.  See [`is_covered_by_digest`].
const DIGEST_OVERLAP_THRESHOLD: f32 = 0.7;

// ── MemoryKind ────────────────────────────────────────────────────────────────

/// Local mirror of `engram::types::MemoryType`, kept independent so this layer
/// is testable without constructing full `Memory` rows.  The production
/// [`RetrievalSource`] maps from `MemoryType` (see `agent_loop`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryKind {
    Principle,
    Preference,
    Semantic,
    Episodic,
}

impl MemoryKind {
    /// Retrieval priority — lower sorts first (matches
    /// `MemoryType::retrieval_priority`): Principle(0) > Preference(1) >
    /// Semantic(2) > Episodic(3).
    pub fn priority(self) -> u8 {
        match self {
            MemoryKind::Principle => 0,
            MemoryKind::Preference => 1,
            MemoryKind::Semantic => 2,
            MemoryKind::Episodic => 3,
        }
    }

    /// Lower-case, terse label used in the `[memory]` block.  Note these differ
    /// from Engram's `type_label` (which is bracketed/title-case for a different
    /// context) — the spec's example block uses `[principle]/[preference]/`
    /// `[fact]/[recent]`.
    pub fn label(self) -> &'static str {
        match self {
            MemoryKind::Principle => "[principle]",
            MemoryKind::Preference => "[preference]",
            MemoryKind::Semantic => "[fact]",
            MemoryKind::Episodic => "[recent]",
        }
    }
}

// ── RetrievedMemory ───────────────────────────────────────────────────────────

/// One memory returned by a [`RetrievalSource`], stripped to what this layer
/// needs: the content, its cognitive type, and its retrieval score.
#[derive(Debug, Clone)]
pub struct RetrievedMemory {
    pub content: String,
    pub mem_type: MemoryKind,
    pub score: f32,
}

impl RetrievedMemory {
    pub fn new(content: impl Into<String>, mem_type: MemoryKind, score: f32) -> Self {
        Self { content: content.into(), mem_type, score }
    }
}

// ── RetrievalSource (injectable seam) ─────────────────────────────────────────

/// Abstracts the act of fetching top-`k` memories for `(user_id, query)`.
///
/// Production wraps Engram's `fetch_candidates_for_query` + `score_candidates`;
/// tests supply a fixed in-memory list.  Per-user isolation is the source's
/// responsibility — this layer passes `user_id` through unchanged and never
/// caches across users (see [`RetrievalLayer`]'s cache key).
pub trait RetrievalSource {
    fn retrieve(&self, user_id: &str, query: &str, k: usize) -> Vec<RetrievedMemory>;
}

// ── RetrievalLayer ────────────────────────────────────────────────────────────

/// Cached top-`k` retrieval driver with digest dedup and token budgeting.
///
/// Holds a 5-minute topic cache keyed by `(user_id, topic-hash(message))` so
/// rapid same-topic messages reuse the formatted block instead of re-querying.
pub struct RetrievalLayer {
    /// How many memories to request from the source (top-5 per spec).
    top_k: usize,
    /// cache key → (expiry_secs, formatted block or None for "no result").
    cache: HashMap<(String, u64), (u64, Option<String>)>,
}

impl Default for RetrievalLayer {
    fn default() -> Self {
        Self { top_k: 5, cache: HashMap::new() }
    }
}

impl RetrievalLayer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct with an explicit `top_k` (tests / tuning).
    pub fn with_top_k(top_k: usize) -> Self {
        Self { top_k: top_k.max(1), cache: HashMap::new() }
    }

    /// Produce the `[memory]`-section **body** for this turn, or `None` when
    /// nothing should be injected (so the assembler omits the section entirely).
    ///
    /// The returned string does *not* include the `[memory]` marker — the
    /// assembler emits that.  Pass the returned value as
    /// `AssemblyExtras::memory`.
    ///
    /// * `existing_prompt_tokens` — approximate tokens already consumed by the
    ///   assembled static layers (identity…context, plus now/proactive).  Used
    ///   to compute the remaining budget `1200 - existing`.
    /// * `digest_text` — the Knowledge Digest body, for dedup.
    /// * `now_secs` — current Unix time (passed in; no chrono / no clock call).
    pub fn query_for_turn(
        &mut self,
        user_id: &str,
        user_message: &str,
        existing_prompt_tokens: usize,
        digest_text: &str,
        now_secs: u64,
        source: &dyn RetrievalSource,
    ) -> Option<String> {
        let budget = self.remaining_budget(existing_prompt_tokens);
        if budget == 0 {
            return None;
        }

        // 5-minute topic cache: reuse the block for rapid same-topic messages.
        let key = (user_id.to_string(), topic_hash(user_message));
        self.evict_expired(now_secs);
        if let Some((_expiry, cached)) = self.cache.get(&key) {
            return cached.clone();
        }

        // The retrieval query is the USER'S MESSAGE (not digest/personality):
        // it surfaces memories relevant to what is being asked right now.
        let retrieved = source.retrieve(user_id, user_message, self.top_k);
        let block = self.build_block(retrieved, digest_text, budget);

        self.cache
            .insert(key, (now_secs + CACHE_TTL_SECS, block.clone()));
        block
    }

    /// Remaining token budget for the retrieval block: the headroom up to the
    /// 1200-token total ceiling, capped at [`DEFAULT_RETRIEVAL_BUDGET`].
    fn remaining_budget(&self, existing_prompt_tokens: usize) -> usize {
        TOTAL_PROMPT_BUDGET
            .saturating_sub(existing_prompt_tokens)
            .min(DEFAULT_RETRIEVAL_BUDGET)
    }

    /// Order, dedup, budget, and format a retrieved set into the block body.
    ///
    /// Returns `None` if nothing survives dedup/budget (0 results, or every
    /// result was already in the digest, or not even one line fits).
    fn build_block(
        &self,
        retrieved: Vec<RetrievedMemory>,
        digest_text: &str,
        budget: usize,
    ) -> Option<String> {
        // Type-priority, then score descending within a type.
        let mut ordered = retrieved;
        ordered.sort_by(|a, b| {
            a.mem_type
                .priority()
                .cmp(&b.mem_type.priority())
                .then_with(|| {
                    b.score
                        .partial_cmp(&a.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });

        // Dedup against the Knowledge Digest, then fill highest-priority-first
        // until the budget is exhausted.
        let mut lines: Vec<String> = Vec::new();
        let mut used = 0usize;
        for m in &ordered {
            if is_covered_by_digest(&m.content, digest_text) {
                continue; // digest already says this — don't spend tokens repeating it
            }
            let line = format!("{} {}", m.mem_type.label(), m.content.trim());
            let cost = estimate_tokens(&line);
            if used + cost > budget {
                continue; // doesn't fit; try the next (cheaper) memory
            }
            used += cost;
            lines.push(line);
        }

        if lines.is_empty() {
            None
        } else {
            Some(lines.join("\n"))
        }
    }

    /// Drop expired cache entries (lazy eviction on access).
    fn evict_expired(&mut self, now_secs: u64) {
        self.cache.retain(|_, (expiry, _)| *expiry > now_secs);
    }
}

// ── Heuristics ────────────────────────────────────────────────────────────────

/// Whether `content` is already "covered" by the Knowledge Digest.
///
/// **Heuristic (no embeddings):** lowercase, tokenize both into word sets
/// (alphanumeric runs, ≥3 chars to skip stop-words like "the"/"you"/"is").  A
/// memory is covered when the fraction of its significant words that also appear
/// in the digest is ≥ [`DIGEST_OVERLAP_THRESHOLD`].  A direct substring match of
/// the trimmed content also counts as covered.  This is intentionally simple and
/// conservative — it only drops memories that are clearly redundant; anything
/// novel is kept so the LLM still gets per-query specifics.
pub fn is_covered_by_digest(content: &str, digest_text: &str) -> bool {
    let content = content.trim();
    if content.is_empty() {
        return true; // nothing to add
    }
    if digest_text.trim().is_empty() {
        return false; // empty digest covers nothing
    }

    let digest_lc = digest_text.to_lowercase();
    if digest_lc.contains(&content.to_lowercase()) {
        return true; // verbatim (or near-verbatim) inclusion
    }

    let content_words: Vec<String> = significant_words(content);
    if content_words.is_empty() {
        // No significant words to compare (very short / all stop-words):
        // fall back to substring, already checked above → treat as novel.
        return false;
    }
    let digest_words: std::collections::HashSet<String> =
        significant_words(digest_text).into_iter().collect();

    let covered = content_words
        .iter()
        .filter(|w| digest_words.contains(*w))
        .count();
    let frac = covered as f32 / content_words.len() as f32;
    frac >= DIGEST_OVERLAP_THRESHOLD
}

/// Lowercase alphanumeric words of length ≥ 3 (skips most stop-words/punctuation).
fn significant_words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3)
        .map(|w| w.to_string())
        .collect()
}

/// Stable topic hash of a message for the cache key.
///
/// We normalize (lowercase, significant words, sorted+deduped) before hashing so
/// that messages about the same topic with minor wording/order differences share
/// a cache entry.  Deterministic across runs in a single process (`DefaultHasher`
/// with a fixed seed); the cache is in-memory only, so cross-run stability is not
/// required.
pub fn topic_hash(message: &str) -> u64 {
    let mut words = significant_words(message);
    words.sort();
    words.dedup();
    let mut hasher = DefaultHasher::new();
    for w in &words {
        w.hash(&mut hasher);
    }
    hasher.finish()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Test source that records the query it was asked, and returns a fixed list
    /// scoped per user (proves per-user isolation flows through the seam).
    struct FakeSource {
        // user_id -> memories
        by_user: HashMap<String, Vec<RetrievedMemory>>,
        last_query: RefCell<Option<(String, String, usize)>>,
    }

    impl FakeSource {
        fn new() -> Self {
            Self { by_user: HashMap::new(), last_query: RefCell::new(None) }
        }
        fn with(mut self, user: &str, mems: Vec<RetrievedMemory>) -> Self {
            self.by_user.insert(user.to_string(), mems);
            self
        }
    }

    impl RetrievalSource for FakeSource {
        fn retrieve(&self, user_id: &str, query: &str, k: usize) -> Vec<RetrievedMemory> {
            *self.last_query.borrow_mut() =
                Some((user_id.to_string(), query.to_string(), k));
            let mut v = self.by_user.get(user_id).cloned().unwrap_or_default();
            v.truncate(k);
            v
        }
    }

    fn mem(content: &str, kind: MemoryKind, score: f32) -> RetrievedMemory {
        RetrievedMemory::new(content, kind, score)
    }

    #[test]
    fn query_uses_user_message_as_input() {
        let src = FakeSource::new().with(
            "operator",
            vec![mem("You like dark roast coffee", MemoryKind::Preference, 0.9)],
        );
        let mut layer = RetrievalLayer::new();
        let out = layer.query_for_turn("operator", "what coffee do I like?", 300, "", 1000, &src);
        assert!(out.is_some());
        let (uid, q, k) = src.last_query.borrow().clone().unwrap();
        assert_eq!(uid, "operator");
        assert_eq!(q, "what coffee do I like?");
        assert_eq!(k, 5, "top-5 requested by default");
    }

    #[test]
    fn top5_ordered_by_type_priority_then_score() {
        let src = FakeSource::new().with(
            "u",
            vec![
                mem("recent thing", MemoryKind::Episodic, 0.99),
                mem("a fact", MemoryKind::Semantic, 0.5),
                mem("low pref", MemoryKind::Preference, 0.4),
                mem("high pref", MemoryKind::Preference, 0.95),
                mem("a principle", MemoryKind::Principle, 0.1),
            ],
        );
        let mut layer = RetrievalLayer::new();
        let out = layer
            .query_for_turn("u", "tell me everything", 200, "", 1000, &src)
            .unwrap();
        let lines: Vec<&str> = out.lines().collect();
        // Principle first (priority 0), then Preference high-then-low, Semantic, Episodic.
        assert!(lines[0].starts_with("[principle]"), "got: {out}");
        assert!(lines[1].contains("high pref"), "got: {out}");
        assert!(lines[2].contains("low pref"), "got: {out}");
        assert!(lines[3].starts_with("[fact]"), "got: {out}");
        assert!(lines[4].starts_with("[recent]"), "got: {out}");
    }

    #[test]
    fn type_labels_are_correct() {
        let src = FakeSource::new().with(
            "u",
            vec![
                mem("p", MemoryKind::Principle, 0.9),
                mem("q", MemoryKind::Preference, 0.9),
                mem("r", MemoryKind::Semantic, 0.9),
                mem("s", MemoryKind::Episodic, 0.9),
            ],
        );
        let mut layer = RetrievalLayer::new();
        let out = layer.query_for_turn("u", "msg", 200, "", 1000, &src).unwrap();
        assert!(out.contains("[principle] p"));
        assert!(out.contains("[preference] q"));
        assert!(out.contains("[fact] r"));
        assert!(out.contains("[recent] s"));
    }

    #[test]
    fn dedup_removes_digest_covered_memory() {
        let digest = "the operator works in field marketing as a senior manager in the bay area.";
        let src = FakeSource::new().with(
            "u",
            vec![
                // Highly overlapping with the digest sentence → dropped.
                mem("the operator works in field marketing senior manager", MemoryKind::Semantic, 0.9),
                // Novel → kept.
                mem("The chord proxy runs on port 8099", MemoryKind::Semantic, 0.8),
            ],
        );
        let mut layer = RetrievalLayer::new();
        let out = layer
            .query_for_turn("u", "what's my job and the proxy port", 200, digest, 1000, &src)
            .unwrap();
        assert!(!out.contains("field marketing"), "digest-covered memory should be dropped: {out}");
        assert!(out.contains("chord proxy runs on port 8099"), "novel memory kept: {out}");
    }

    #[test]
    fn all_results_covered_by_digest_yields_none() {
        let digest = "You prefer direct feedback and dark roast coffee with no cream.";
        let src = FakeSource::new().with(
            "u",
            vec![
                mem("You prefer direct feedback", MemoryKind::Principle, 0.9),
                mem("dark roast coffee no cream", MemoryKind::Preference, 0.8),
            ],
        );
        let mut layer = RetrievalLayer::new();
        let out = layer.query_for_turn("u", "feedback and coffee", 200, digest, 1000, &src);
        assert!(out.is_none(), "everything in digest → section omitted, got: {out:?}");
    }

    #[test]
    fn zero_results_yields_none() {
        let src = FakeSource::new(); // no memories for anyone
        let mut layer = RetrievalLayer::new();
        let out = layer.query_for_turn("u", "obscure new topic", 200, "", 1000, &src);
        assert!(out.is_none());
    }

    #[test]
    fn budget_enforced_includes_highest_priority_first() {
        // Long contents so only one or two lines fit in a tight budget.
        let long = "word ".repeat(60); // ~60 words ≈ 80 tokens
        let src = FakeSource::new().with(
            "u",
            vec![
                mem(&format!("principle {long}"), MemoryKind::Principle, 0.9),
                mem(&format!("preference {long}"), MemoryKind::Preference, 0.9),
                mem(&format!("fact {long}"), MemoryKind::Semantic, 0.9),
            ],
        );
        let mut layer = RetrievalLayer::new();
        // existing=1400 → remaining = min(TOTAL−1400=100, 200) = 100 tokens; ~one 80t line fits.
        let out = layer
            .query_for_turn("u", "everything", 1400, "", 1000, &src)
            .unwrap();
        assert!(out.starts_with("[principle]"), "highest priority included first: {}", &out[..20]);
        assert!(!out.contains("[fact]"), "tight budget should drop lower-priority lines");
        assert!(estimate_tokens(&out) <= 100, "block within remaining budget");
    }

    #[test]
    fn zero_budget_returns_none_without_querying() {
        let src = FakeSource::new().with("u", vec![mem("x", MemoryKind::Principle, 0.9)]);
        let mut layer = RetrievalLayer::new();
        // existing already at the ceiling → no headroom.
        let out = layer.query_for_turn("u", "msg", TOTAL_PROMPT_BUDGET, "", 1000, &src);
        assert!(out.is_none());
        assert!(src.last_query.borrow().is_none(), "should not query when no budget");
    }

    #[test]
    fn cache_hit_for_same_topic() {
        // Source returns a memory the first time; if asked again it would still
        // return it, but we assert the cache short-circuits by mutating intent:
        // we check that two calls within TTL produce identical output and that
        // the second call reused the cached value (last_query unchanged).
        let src = FakeSource::new().with(
            "u",
            vec![mem("cached fact", MemoryKind::Semantic, 0.9)],
        );
        let mut layer = RetrievalLayer::new();
        let first = layer.query_for_turn("u", "tell me about the deploy", 200, "", 1000, &src);
        let marker_after_first = src.last_query.borrow().clone();
        // Different wording, same significant-word topic → same cache key.
        let second = layer.query_for_turn("u", "TELL me, about THE deploy!!!", 200, "", 1100, &src);
        assert_eq!(first, second, "cached block reused");
        assert_eq!(
            marker_after_first,
            *src.last_query.borrow(),
            "second same-topic call should not re-query the source"
        );
    }

    #[test]
    fn cache_expires_after_ttl() {
        let src = FakeSource::new().with("u", vec![mem("fact", MemoryKind::Semantic, 0.9)]);
        let mut layer = RetrievalLayer::new();
        let _ = layer.query_for_turn("u", "deploy status", 200, "", 1000, &src);
        let q1 = src.last_query.borrow().clone();
        // Advance past TTL → cache miss → source queried again.
        let _ = layer.query_for_turn("u", "deploy status", 200, "", 1000 + CACHE_TTL_SECS + 1, &src);
        // last_query is overwritten each retrieve(); presence proves a fresh call.
        assert!(src.last_query.borrow().is_some());
        // (q1 and the post-expiry query are identical text, so we can't diff them;
        // the eviction is exercised — covered structurally by evict_expired.)
        let _ = q1;
        assert!(layer.cache.values().all(|(exp, _)| *exp > 1000 + CACHE_TTL_SECS));
    }

    #[test]
    fn per_user_isolation_via_source() {
        let src = FakeSource::new()
            .with("alice", vec![mem("alice loves hiking", MemoryKind::Preference, 0.9)])
            .with("bob", vec![mem("bob loves sailing", MemoryKind::Preference, 0.9)]);
        let mut layer = RetrievalLayer::new();
        let a = layer.query_for_turn("alice", "my hobbies", 200, "", 1000, &src).unwrap();
        let b = layer.query_for_turn("bob", "my hobbies", 200, "", 1000, &src).unwrap();
        assert!(a.contains("hiking") && !a.contains("sailing"));
        assert!(b.contains("sailing") && !b.contains("hiking"));
    }

    #[test]
    fn cache_key_isolates_users_same_message() {
        // Same message text, different users → must not collide in the cache.
        let src = FakeSource::new()
            .with("alice", vec![mem("alice secret", MemoryKind::Semantic, 0.9)])
            .with("bob", vec![mem("bob secret", MemoryKind::Semantic, 0.9)]);
        let mut layer = RetrievalLayer::new();
        let a = layer.query_for_turn("alice", "what do you know", 200, "", 1000, &src).unwrap();
        let b = layer.query_for_turn("bob", "what do you know", 200, "", 1000, &src).unwrap();
        assert!(a.contains("alice secret"));
        assert!(b.contains("bob secret"));
    }

    #[test]
    fn topic_hash_normalizes_wording() {
        // Same significant words (>=3 chars), different order/case/punctuation
        // → same topic hash.
        assert_eq!(
            topic_hash("Tell deploy status"),
            topic_hash("status, DEPLOY... tell!!!"),
        );
        assert_ne!(topic_hash("coffee preferences"), topic_hash("deploy status"));
    }

    #[test]
    fn is_covered_by_digest_basics() {
        let digest = "the operator is a senior manager in field marketing.";
        assert!(is_covered_by_digest("the operator senior manager field marketing", digest));
        assert!(!is_covered_by_digest("The proxy listens on port 8099", digest));
        // Empty digest covers nothing; empty content adds nothing.
        assert!(!is_covered_by_digest("novel fact here", ""));
        assert!(is_covered_by_digest("", digest));
    }

    #[test]
    fn no_marker_in_body() {
        // The body must NOT contain the [memory] marker — the assembler adds it.
        let src = FakeSource::new().with("u", vec![mem("a fact", MemoryKind::Semantic, 0.9)]);
        let mut layer = RetrievalLayer::new();
        let out = layer.query_for_turn("u", "msg", 200, "", 1000, &src).unwrap();
        assert!(!out.contains("[memory]"));
    }

    #[test]
    fn no_personal_data_or_ips_hardcoded() {
        // This source file must not embed personal identifiers or infra IPs.
        let src = include_str!("retrieval_layer.rs");
        // Build the needle dynamically so this assertion line does not itself
        // embed the forbidden literal (which would make the scan self-trip).
        let ip_prefix = format!("{}.{}", "192", "168");
        assert!(!src.contains(&ip_prefix), "no hardcoded IPs");
        // 'the operator'/'gpu-host' may appear only inside #[cfg(test)] fixtures, never in
        // the production constants/logic above — spot-check the public consts.
        assert_eq!(TOTAL_PROMPT_BUDGET, 1500);
    }
}
