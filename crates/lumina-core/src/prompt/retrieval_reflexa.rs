//! DPROMPT-12: Retrieval-triggered Reflexa consolidation.
//!
//! After every per-query retrieval (DPROMPT-11), the surfaced memories are
//! scanned for *problems* — contradictions with the Knowledge Digest, stale /
//! low-confidence memories, clusters that should be consolidated, and
//! high-importance memories the digest is missing.  Each problem becomes a
//! **deferred** [`ReflexaAction`] appended to a per-user queue file
//! (`~/.lumina/reflexa-queue.json`).  The trigger is deferred so it never
//! blocks the current turn.
//!
//! The sleep-time consolidator (DPROMPT-07) drains the queue during its nightly
//! run via [`RetrievalReflexaTrigger::process_queue`].  This creates a feedback
//! loop: retrieval finds issues → Reflexa fixes them → the next nightly digest
//! is better → retrieval results improve.
//!
//! ## Design
//! * **No network, no chrono.**  Every age/threshold computation takes
//!   `now_secs` and pre-computed `*_secs` timestamps as parameters, so the
//!   detectors are deterministic under test.
//! * **Explicit paths.**  The queue path is always passed in as `&Path`
//!   (production resolves it from [`reflexa_queue_path`]; tests pass a tempfile),
//!   so this module never touches `$HOME` on its own.
//! * **Injectable LLM.**  Contradiction resolution goes through the shared
//!   [`LlmGenerator`] seam, so `process_queue` is testable with a mock.
//! * **Per-user.**  Each user has an isolated queue file; dedup is by
//!   `memory_id` *within* a user's queue.
//! * **Testable detection.**  Detection consumes a small [`RetrievedRef`] struct
//!   (id + content + metadata) rather than a live DB row, so the four detectors
//!   can be exercised directly.

use super::user_layer_dir;
use crate::error::Result;
use crate::prompt::llm::LlmGenerator;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

// ── Tunable heuristics (documented constants) ──────────────────────────────────

/// A retrieved memory must share at least this fraction of its significant words
/// with the digest to be considered "about the same thing" for contradiction
/// detection. Mirrors the digest-overlap idea from `retrieval_layer`.
const CONTRADICTION_OVERLAP_THRESHOLD: f32 = 0.5;

/// Below this confidence a memory is a staleness candidate.
const STALE_CONFIDENCE_MAX: f32 = 0.5;

/// Staleness requires the memory to be untouched for at least this long.
const STALE_LAST_ACCESS_SECS: i64 = 90 * 24 * 60 * 60; // 90 days

/// …and to have been created at least this long ago.
const STALE_CREATED_SECS: i64 = 180 * 24 * 60 * 60; // 180 days

/// Two memories are "mutually similar" when they share at least this fraction of
/// significant words (a cheap stand-in for the spec's cosine > 0.75).
const CLUSTER_SIMILARITY_THRESHOLD: f32 = 0.5;

/// A consolidation cluster needs at least this many mutually-similar memories.
const CLUSTER_MIN_SIZE: usize = 3;

/// More than this many contradiction actions in a single day means the digest is
/// degrading → trigger an immediate reconstruction.
const CONTRADICTION_SPIKE_PER_DAY: usize = 5;

/// During `process_queue`, a stale memory whose last access is more recent than
/// this is judged "still relevant despite age" and kept instead of archived.
const RECENT_ACCESS_KEEP_SECS: i64 = 30 * 24 * 60 * 60; // 30 days

/// Negation / difference keywords that signal a content conflict.
const NEGATION_KEYWORDS: &[&str] = &[
    "not", "no longer", "never", "but", "however", "changed", "instead",
    "actually", "anymore", "stopped", "quit", "dislikes", "hates", "doesn't",
    "don't", "isn't", "wasn't", "former", "used to", "previously",
];

// ── ReflexaAction ───────────────────────────────────────────────────────────────

/// A deferred consolidation action queued by retrieval and processed at
/// sleep-time. Serializable so the queue survives as JSON on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ReflexaAction {
    /// A retrieved memory conflicts with the digest; resolve which is correct.
    ResolveContradiction {
        memory_id: String,
        digest_excerpt: String,
    },
    /// A stale / low-confidence memory needs verification or archival.
    VerifyStale { memory_id: String, reason: String },
    /// A cluster of similar memories should be merged into one.
    ConsolidateCluster {
        memory_ids: Vec<String>,
        topic: String,
    },
    /// A high-importance memory missing from the digest should be captured next
    /// nightly reconstruction.
    FlagDigestGap {
        memory_id: String,
        importance: String,
    },
}

impl ReflexaAction {
    /// The primary memory id this action keys on, for queue dedup.  Cluster
    /// actions key on their first (sorted) member.
    pub fn dedup_key(&self) -> String {
        match self {
            ReflexaAction::ResolveContradiction { memory_id, .. } => memory_id.clone(),
            ReflexaAction::VerifyStale { memory_id, .. } => memory_id.clone(),
            ReflexaAction::FlagDigestGap { memory_id, .. } => memory_id.clone(),
            ReflexaAction::ConsolidateCluster { memory_ids, .. } => {
                let mut ids = memory_ids.clone();
                ids.sort();
                format!("cluster:{}", ids.join(","))
            }
        }
    }

    fn is_contradiction(&self) -> bool {
        matches!(self, ReflexaAction::ResolveContradiction { .. })
    }
}

// ── RetrievedRef (testable detection input) ─────────────────────────────────────

/// A retrieved memory plus the metadata the detectors need, decoupled from any
/// DB row so detection is unit-testable.  The production [`agent_loop`] builds
/// these from Engram's scored rows alongside the [`super::retrieval_layer`]
/// formatting pass.
#[derive(Debug, Clone)]
pub struct RetrievedRef {
    /// Stable Engram memory id.
    pub id: String,
    /// The memory's text content.
    pub content: String,
    /// Lower-case cognitive type label ("principle"/"preference"/"semantic"/
    /// "episodic"). Used to judge importance for the digest-gap detector.
    pub mem_type: String,
    /// Retrieval / stored confidence in `[0,1]`.
    pub confidence: f32,
    /// Unix seconds of the last access (read).
    pub last_accessed_secs: i64,
    /// Unix seconds the memory was created.
    pub created_at_secs: i64,
}

impl RetrievedRef {
    /// High-importance memories: principles, or frequently-relevant preferences.
    fn is_high_importance(&self) -> bool {
        let t = self.mem_type.to_lowercase();
        t == "principle" || (t == "preference" && self.confidence >= 0.7)
    }
}

// ── Outcome of a sleep-time queue drain ─────────────────────────────────────────

/// Summary of one `process_queue` run, returned to the sleep-time orchestrator.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessOutcome {
    /// Contradictions for which a resolution direction was produced.
    pub contradictions_resolved: usize,
    /// Stale memories archived (not recently accessed).
    pub stale_archived: usize,
    /// Stale memories kept because they were accessed recently.
    pub stale_kept: usize,
    /// Memory clusters merged.
    pub clusters_merged: usize,
    /// Memories flagged for inclusion in the next digest reconstruction.
    pub digest_gaps_flagged: usize,
    /// Actions that could not be handled and remain queued for next night.
    pub deferred_again: usize,
}

impl ProcessOutcome {
    fn total_handled(&self) -> usize {
        self.contradictions_resolved
            + self.stale_archived
            + self.stale_kept
            + self.clusters_merged
            + self.digest_gaps_flagged
    }
}

/// A side-effecting resolution the sleep-time caller should apply to Engram.
/// `process_queue` is pure with respect to Engram — it returns the decisions and
/// the caller persists them — keeping this module DB-free and testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Supersede `wrong_memory_id` with `resolution_text` (LLM-decided).
    SupersedeContradiction {
        wrong_memory_id: String,
        resolution_text: String,
    },
    /// Archive a stale memory.
    ArchiveStale { memory_id: String },
    /// Merge a cluster into a single memory under `topic`.
    MergeCluster {
        memory_ids: Vec<String>,
        topic: String,
    },
    /// Tag a memory `digest_priority: high` for next reconstruction.
    FlagForDigest { memory_id: String },
}

// ── RetrievalReflexaTrigger ─────────────────────────────────────────────────────

/// Detects problems in retrieval results and manages the deferred Reflexa queue.
#[derive(Debug, Clone, Default)]
pub struct RetrievalReflexaTrigger;

impl RetrievalReflexaTrigger {
    pub fn new() -> Self {
        RetrievalReflexaTrigger
    }

    /// Scan retrieval results against the digest and emit deferred actions.
    ///
    /// Detectors (documented heuristics, all keyword/age based — no embeddings):
    /// * **Contradiction** — a memory shares enough significant words with a
    ///   digest sentence to be "about the same thing"
    ///   (≥ [`CONTRADICTION_OVERLAP_THRESHOLD`]) *and* either side carries a
    ///   negation/difference keyword the other lacks.
    /// * **Staleness** — `confidence < 0.5` AND last-accessed > 90d AND
    ///   created > 180d, OR the content carries a past temporal marker.
    /// * **Consolidation** — 3+ retrieved memories are mutually similar
    ///   (≥ [`CLUSTER_SIMILARITY_THRESHOLD`] shared words pairwise).
    /// * **Digest gap** — a high-importance memory (principle, or strong
    ///   preference) whose content is not covered by the digest.
    pub fn check_results(
        &self,
        retrieved: &[RetrievedRef],
        digest: &str,
        now_secs: i64,
    ) -> Vec<ReflexaAction> {
        let mut actions = Vec::new();

        // (a) contradictions + (b) staleness + (d) digest gap — per memory.
        for m in retrieved {
            if let Some(excerpt) = self.contradicts_digest(&m.content, digest) {
                actions.push(ReflexaAction::ResolveContradiction {
                    memory_id: m.id.clone(),
                    digest_excerpt: excerpt,
                });
            }

            if let Some(reason) = self.staleness_reason(m, now_secs) {
                actions.push(ReflexaAction::VerifyStale {
                    memory_id: m.id.clone(),
                    reason,
                });
            }

            if m.is_high_importance() && !is_covered(&m.content, digest) {
                actions.push(ReflexaAction::FlagDigestGap {
                    memory_id: m.id.clone(),
                    importance: m.mem_type.to_lowercase(),
                });
            }
        }

        // (c) consolidation clusters — across the set.
        if let Some(cluster) = self.detect_cluster(retrieved) {
            actions.push(cluster);
        }

        actions
    }

    /// Contradiction detector. Returns the matched digest sentence (the
    /// `digest_excerpt`) when the memory is about the same thing as a digest
    /// sentence but their negation polarity differs.
    fn contradicts_digest(&self, content: &str, digest: &str) -> Option<String> {
        if content.trim().is_empty() || digest.trim().is_empty() {
            return None;
        }
        let content_words: HashSet<String> = significant_words(content).into_iter().collect();
        if content_words.is_empty() {
            return None;
        }
        let content_neg = has_negation(content);

        for sentence in split_sentences(digest) {
            let s_words: HashSet<String> = significant_words(&sentence).into_iter().collect();
            if s_words.is_empty() {
                continue;
            }
            // Overlap as a fraction of the *memory's* words — "are these about
            // the same entity?".
            let shared = content_words.intersection(&s_words).count();
            let frac = shared as f32 / content_words.len() as f32;
            if frac < CONTRADICTION_OVERLAP_THRESHOLD {
                continue;
            }
            // Same topic. Conflict if negation polarity differs between the two.
            if content_neg != has_negation(&sentence) {
                return Some(sentence.trim().to_string());
            }
        }
        None
    }

    /// Staleness detector. Returns a human reason string when stale.
    fn staleness_reason(&self, m: &RetrievedRef, now_secs: i64) -> Option<String> {
        let age = now_secs.saturating_sub(m.created_at_secs);
        let since_access = now_secs.saturating_sub(m.last_accessed_secs);

        if m.confidence < STALE_CONFIDENCE_MAX
            && since_access > STALE_LAST_ACCESS_SECS
            && age > STALE_CREATED_SECS
        {
            return Some(format!(
                "low confidence ({:.2}), last accessed {}d ago, created {}d ago",
                m.confidence,
                since_access / 86_400,
                age / 86_400,
            ));
        }

        if has_past_temporal_marker(&m.content) {
            return Some("contains a temporal marker that is now in the past".to_string());
        }

        None
    }

    /// Consolidation detector. Greedily grows a cluster from the first memory
    /// that has ≥ `CLUSTER_MIN_SIZE` mutually-similar members.
    fn detect_cluster(&self, retrieved: &[RetrievedRef]) -> Option<ReflexaAction> {
        let word_sets: Vec<HashSet<String>> = retrieved
            .iter()
            .map(|m| significant_words(&m.content).into_iter().collect())
            .collect();

        for i in 0..retrieved.len() {
            if word_sets[i].is_empty() {
                continue;
            }
            let mut members = vec![i];
            for (j, set_j) in word_sets.iter().enumerate() {
                if j == i || set_j.is_empty() {
                    continue;
                }
                if jaccard(&word_sets[i], set_j) >= CLUSTER_SIMILARITY_THRESHOLD {
                    members.push(j);
                }
            }
            if members.len() >= CLUSTER_MIN_SIZE {
                let memory_ids: Vec<String> =
                    members.iter().map(|&k| retrieved[k].id.clone()).collect();
                let topic = cluster_topic(&members, retrieved, &word_sets);
                return Some(ReflexaAction::ConsolidateCluster { memory_ids, topic });
            }
        }
        None
    }

    /// Append `action` to the per-user queue at `queue_path`, deduped by
    /// [`ReflexaAction::dedup_key`].  Creates the file (and parent dirs) if
    /// absent. The queue is a JSON map: `{ user_id: [action, …] }`.
    pub fn queue_action(
        &self,
        user_id: &str,
        action: ReflexaAction,
        queue_path: &Path,
    ) -> Result<()> {
        let mut queue = load_queue(queue_path)?;
        let bucket = queue.entry(user_id.to_string()).or_default();
        let key = action.dedup_key();
        if bucket.iter().any(|a| a.dedup_key() == key) {
            return Ok(()); // already queued for this user — dedup
        }
        bucket.push(action);
        save_queue(queue_path, &queue)
    }

    /// Convenience: queue many actions for one user (each deduped).
    pub fn queue_actions(
        &self,
        user_id: &str,
        actions: Vec<ReflexaAction>,
        queue_path: &Path,
    ) -> Result<usize> {
        let mut queued = 0;
        for a in actions {
            let before = self.queue_len(user_id, queue_path)?;
            self.queue_action(user_id, a, queue_path)?;
            if self.queue_len(user_id, queue_path)? > before {
                queued += 1;
            }
        }
        Ok(queued)
    }

    fn queue_len(&self, user_id: &str, queue_path: &Path) -> Result<usize> {
        let queue = load_queue(queue_path)?;
        Ok(queue.get(user_id).map(|v| v.len()).unwrap_or(0))
    }

    /// Sleep-time queue drain for one user.
    ///
    /// Reads the user's queued actions, decides a [`Resolution`] for each
    /// (contradictions consult the [`LlmGenerator`] seam against `archive_text`;
    /// stale memories are archived unless accessed within
    /// [`RECENT_ACCESS_KEEP_SECS`]; clusters merge; gaps flag), then **clears the
    /// user's bucket** from the queue file.
    ///
    /// Returns the [`ProcessOutcome`] counts and the [`Resolution`]s the caller
    /// should apply to Engram. `last_access_secs` maps `memory_id ->
    /// last_access_secs` so the stale-archival decision can respect recent
    /// access without a DB call here.
    pub fn process_queue(
        &self,
        user_id: &str,
        queue_path: &Path,
        llm: &dyn LlmGenerator,
        archive_text: &str,
        last_access_secs: &std::collections::HashMap<String, i64>,
        now_secs: i64,
    ) -> Result<(ProcessOutcome, Vec<Resolution>)> {
        let mut queue = load_queue(queue_path)?;
        let actions = match queue.remove(user_id) {
            Some(a) => a,
            None => return Ok((ProcessOutcome::default(), Vec::new())), // fast skip
        };

        let mut outcome = ProcessOutcome::default();
        let mut resolutions = Vec::new();

        // Contradictions first (spec: prioritize when the queue is large).
        let mut ordered = actions;
        ordered.sort_by_key(|a| if a.is_contradiction() { 0 } else { 1 });

        for action in ordered {
            match action {
                ReflexaAction::ResolveContradiction {
                    memory_id,
                    digest_excerpt,
                } => {
                    match self.resolve_contradiction(llm, &memory_id, &digest_excerpt, archive_text)
                    {
                        Ok(text) => {
                            resolutions.push(Resolution::SupersedeContradiction {
                                wrong_memory_id: memory_id,
                                resolution_text: text,
                            });
                            outcome.contradictions_resolved += 1;
                        }
                        Err(_) => {
                            // LLM timeout/failure → keep for next night.
                            queue
                                .entry(user_id.to_string())
                                .or_default()
                                .push(ReflexaAction::ResolveContradiction {
                                    memory_id,
                                    digest_excerpt,
                                });
                            outcome.deferred_again += 1;
                        }
                    }
                }
                ReflexaAction::VerifyStale { memory_id, .. } => {
                    let last = last_access_secs.get(&memory_id).copied().unwrap_or(0);
                    let recently = now_secs.saturating_sub(last) < RECENT_ACCESS_KEEP_SECS;
                    if recently {
                        outcome.stale_kept += 1; // still relevant despite age
                    } else {
                        resolutions.push(Resolution::ArchiveStale { memory_id });
                        outcome.stale_archived += 1;
                    }
                }
                ReflexaAction::ConsolidateCluster { memory_ids, topic } => {
                    resolutions.push(Resolution::MergeCluster {
                        memory_ids,
                        topic,
                    });
                    outcome.clusters_merged += 1;
                }
                ReflexaAction::FlagDigestGap { memory_id, .. } => {
                    resolutions.push(Resolution::FlagForDigest { memory_id });
                    outcome.digest_gaps_flagged += 1;
                }
            }
        }

        // Persist: the user's bucket is now cleared (minus anything re-deferred).
        if let Some(b) = queue.get(user_id) {
            if b.is_empty() {
                queue.remove(user_id);
            }
        }
        save_queue(queue_path, &queue)?;

        let _ = outcome.total_handled(); // keep helper exercised / referenced
        Ok((outcome, resolutions))
    }

    /// Ask the LLM which of (memory, digest) is correct given the archive.
    fn resolve_contradiction(
        &self,
        llm: &dyn LlmGenerator,
        memory_id: &str,
        digest_excerpt: &str,
        archive_text: &str,
    ) -> Result<String> {
        let system = "You resolve contradictions between a stored memory and a \
                      knowledge digest. Decide which is more recent and accurate \
                      based on the conversation archive. Reply with the corrected \
                      statement only.";
        let user = format!(
            "Memory id: {memory_id}\nDigest says: {digest_excerpt}\n\nConversation archive:\n{archive_text}\n\nWhich is correct, and what is the accurate statement?"
        );
        llm.generate("lumina-deep", system, &user)
    }

    /// Spike metric: more than [`CONTRADICTION_SPIKE_PER_DAY`] contradictions in
    /// a day means the digest is degrading → trigger immediate reconstruction.
    pub fn should_trigger_immediate_reconstruction(&self, contradictions_today: usize) -> bool {
        contradictions_today > CONTRADICTION_SPIKE_PER_DAY
    }
}

/// Free-function form of the spike metric (spec signature
/// `should_trigger_immediate_reconstruction(actions_today)`).
pub fn should_trigger_immediate_reconstruction(actions_today: usize) -> bool {
    actions_today > CONTRADICTION_SPIKE_PER_DAY
}

// ── Queue file path ─────────────────────────────────────────────────────────────

/// Per-user Reflexa queue path: `{user_layer_dir}/reflexa-queue.json`.
///
/// Reuses [`user_layer_dir`] so the queue lives under the same per-user
/// `~/.lumina/...` tree as the prompt layers and honours `LUMINA_PROMPT_DIR`.
pub fn reflexa_queue_path(user_id: &str) -> PathBuf {
    user_layer_dir(user_id).join("reflexa-queue.json")
}

// ── Queue (de)serialization ─────────────────────────────────────────────────────

type Queue = std::collections::BTreeMap<String, Vec<ReflexaAction>>;

fn load_queue(path: &Path) -> Result<Queue> {
    match std::fs::read_to_string(path) {
        Ok(s) if !s.trim().is_empty() => Ok(serde_json::from_str(&s)?),
        Ok(_) => Ok(Queue::new()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Queue::new()),
        Err(e) => Err(e.into()),
    }
}

fn save_queue(path: &Path, queue: &Queue) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(queue)?;
    std::fs::write(path, json)?;
    Ok(())
}

// ── Text heuristics ─────────────────────────────────────────────────────────────

/// Lowercase alphanumeric words of length ≥ 3 (skips most stop-words/punctuation).
fn significant_words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3)
        .map(|w| w.to_string())
        .collect()
}

/// Whether `content` is covered by the digest (≥70% of significant words appear,
/// or verbatim inclusion). Shared shape with `retrieval_layer::is_covered_by_digest`.
fn is_covered(content: &str, digest: &str) -> bool {
    let content = content.trim();
    if content.is_empty() {
        return true;
    }
    if digest.trim().is_empty() {
        return false;
    }
    if digest.to_lowercase().contains(&content.to_lowercase()) {
        return true;
    }
    let cw = significant_words(content);
    if cw.is_empty() {
        return false;
    }
    let dw: HashSet<String> = significant_words(digest).into_iter().collect();
    let covered = cw.iter().filter(|w| dw.contains(*w)).count();
    (covered as f32 / cw.len() as f32) >= 0.7
}

/// Jaccard similarity of two word sets.
fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f32;
    let union = a.union(b).count() as f32;
    inter / union
}

/// Does the text contain a negation / difference keyword?
fn has_negation(text: &str) -> bool {
    let lc = format!(" {} ", text.to_lowercase());
    NEGATION_KEYWORDS.iter().any(|kw| {
        if kw.contains(' ') {
            lc.contains(&format!(" {kw} ")) || lc.contains(&format!(" {kw}"))
        } else {
            lc.contains(&format!(" {kw} ")) || lc.contains(&format!(" {kw}."))
        }
    })
}

/// Past temporal markers that, once stored, refer to a moment now elapsed.
fn has_past_temporal_marker(text: &str) -> bool {
    let lc = text.to_lowercase();
    const MARKERS: &[&str] = &[
        "last week",
        "last month",
        "next week",
        "next month",
        "yesterday",
        "tomorrow",
        "this morning",
        "later today",
        "next quarter",
    ];
    MARKERS.iter().any(|m| lc.contains(m))
}

/// Split a digest into rough sentences (on `.`/`!`/`?`/newline).
fn split_sentences(text: &str) -> Vec<String> {
    text.split(|c| c == '.' || c == '!' || c == '?' || c == '\n')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// A short topic label for a cluster: the most frequent shared significant word.
fn cluster_topic(
    members: &[usize],
    retrieved: &[RetrievedRef],
    word_sets: &[HashSet<String>],
) -> String {
    let mut counts: std::collections::HashMap<&String, usize> = std::collections::HashMap::new();
    for &m in members {
        for w in &word_sets[m] {
            *counts.entry(w).or_insert(0) += 1;
        }
    }
    counts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(a.0)))
        .map(|(w, _)| w.clone())
        .unwrap_or_else(|| {
            retrieved
                .get(members[0])
                .map(|m| m.content.chars().take(24).collect())
                .unwrap_or_default()
        })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use crate::prompt::llm::MockGenerator;
    use std::collections::HashMap;
    use tempfile::tempdir;

    const DAY: i64 = 86_400;

    fn rref(id: &str, content: &str, kind: &str, conf: f32, last: i64, created: i64) -> RetrievedRef {
        RetrievedRef {
            id: id.to_string(),
            content: content.to_string(),
            mem_type: kind.to_string(),
            confidence: conf,
            last_accessed_secs: last,
            created_at_secs: created,
        }
    }

    // ── Detectors ────────────────────────────────────────────────────────────

    #[test]
    fn detects_contradiction_against_digest() {
        let trig = RetrievalReflexaTrigger::new();
        let digest = "the operator likes dark roast coffee.";
        // Same topic (coffee/dark/roast) but negated → contradiction.
        let mems = vec![rref(
            "m1",
            "the operator does not like dark roast coffee anymore",
            "preference",
            0.9,
            0,
            0,
        )];
        let actions = trig.check_results(&mems, digest, DAY * 365);
        assert!(actions.iter().any(|a| matches!(
            a,
            ReflexaAction::ResolveContradiction { memory_id, .. } if memory_id == "m1"
        )));
    }

    #[test]
    fn no_contradiction_when_polarity_matches() {
        let trig = RetrievalReflexaTrigger::new();
        let digest = "the operator likes dark roast coffee.";
        let mems = vec![rref("m1", "the operator likes dark roast coffee very much", "preference", 0.9, DAY * 365, DAY * 365)];
        let actions = trig.check_results(&mems, digest, DAY * 366);
        assert!(!actions.iter().any(|a| a.is_contradiction()));
    }

    #[test]
    fn detects_staleness_by_age_and_confidence() {
        let trig = RetrievalReflexaTrigger::new();
        let now = DAY * 400;
        // confidence < 0.5, last access 100d ago, created 200d ago.
        let mems = vec![rref(
            "old1",
            "some obscure semantic fact",
            "semantic",
            0.3,
            now - DAY * 100,
            now - DAY * 200,
        )];
        let actions = trig.check_results(&mems, "", now);
        assert!(actions
            .iter()
            .any(|a| matches!(a, ReflexaAction::VerifyStale { memory_id, .. } if memory_id == "old1")));
    }

    #[test]
    fn fresh_memory_is_not_stale() {
        let trig = RetrievalReflexaTrigger::new();
        let now = DAY * 400;
        let mems = vec![rref("fresh", "a normal fact", "semantic", 0.9, now - DAY, now - DAY * 5)];
        let actions = trig.check_results(&mems, "", now);
        assert!(!actions.iter().any(|a| matches!(a, ReflexaAction::VerifyStale { .. })));
    }

    #[test]
    fn detects_staleness_by_temporal_marker() {
        let trig = RetrievalReflexaTrigger::new();
        let mems = vec![rref("t1", "We agreed to ship it next week", "episodic", 0.9, 0, 0)];
        let actions = trig.check_results(&mems, "", DAY);
        assert!(actions
            .iter()
            .any(|a| matches!(a, ReflexaAction::VerifyStale { memory_id, .. } if memory_id == "t1")));
    }

    #[test]
    fn detects_consolidation_cluster_of_three() {
        let trig = RetrievalReflexaTrigger::new();
        let mems = vec![
            rref("c1", "the chord proxy deploys to the server nightly", "semantic", 0.8, 0, 0),
            rref("c2", "the chord proxy server runs the deploy", "semantic", 0.8, 0, 0),
            rref("c3", "chord proxy deploy server config", "semantic", 0.8, 0, 0),
            rref("z1", "completely unrelated cooking recipe", "episodic", 0.8, 0, 0),
        ];
        let actions = trig.check_results(&mems, "", DAY);
        let cluster = actions
            .iter()
            .find_map(|a| match a {
                ReflexaAction::ConsolidateCluster { memory_ids, .. } => Some(memory_ids),
                _ => None,
            })
            .expect("expected a cluster");
        assert!(cluster.len() >= 3);
        assert!(!cluster.contains(&"z1".to_string()));
    }

    #[test]
    fn no_cluster_when_topics_differ() {
        let trig = RetrievalReflexaTrigger::new();
        let mems = vec![
            rref("a", "coffee preferences morning", "preference", 0.8, 0, 0),
            rref("b", "deploy server pipeline", "semantic", 0.8, 0, 0),
            rref("c", "hiking weekend mountains", "episodic", 0.8, 0, 0),
        ];
        let actions = trig.check_results(&mems, "", DAY);
        assert!(!actions.iter().any(|a| matches!(a, ReflexaAction::ConsolidateCluster { .. })));
    }

    #[test]
    fn detects_digest_gap_for_high_importance_memory() {
        let trig = RetrievalReflexaTrigger::new();
        let digest = "the operator works in marketing.";
        // A principle absent from the digest → digest gap.
        let mems = vec![rref(
            "p1",
            "Always prefers traditional unflavored preparations",
            "principle",
            0.9,
            0,
            0,
        )];
        let actions = trig.check_results(&mems, digest, DAY);
        assert!(actions
            .iter()
            .any(|a| matches!(a, ReflexaAction::FlagDigestGap { memory_id, .. } if memory_id == "p1")));
    }

    #[test]
    fn no_digest_gap_when_low_importance_or_covered() {
        let trig = RetrievalReflexaTrigger::new();
        // Low-importance episodic → never a gap, even if absent.
        let m_low = rref("e1", "we chatted about the weather", "episodic", 0.9, 0, 0);
        // High-importance but already covered by digest → not a gap.
        let digest = "Always prefers traditional unflavored preparations.";
        let m_covered = rref("p1", "Always prefers traditional unflavored preparations", "principle", 0.9, 0, 0);
        let actions = trig.check_results(&[m_low, m_covered], digest, DAY);
        assert!(!actions.iter().any(|a| matches!(a, ReflexaAction::FlagDigestGap { .. })));
    }

    // ── Queue append + dedup ──────────────────────────────────────────────────

    #[test]
    fn queue_action_appends_and_dedups_by_memory_id() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("reflexa-queue.json");
        let trig = RetrievalReflexaTrigger::new();
        let a = ReflexaAction::VerifyStale { memory_id: "m1".into(), reason: "old".into() };
        trig.queue_action("operator", a.clone(), &path).unwrap();
        // Same memory_id again (different reason) → deduped.
        let a2 = ReflexaAction::VerifyStale { memory_id: "m1".into(), reason: "different".into() };
        trig.queue_action("operator", a2, &path).unwrap();
        let q = load_queue(&path).unwrap();
        assert_eq!(q.get("operator").unwrap().len(), 1, "deduped by memory_id");
    }

    #[test]
    fn queue_action_keeps_distinct_memory_ids() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("q.json");
        let trig = RetrievalReflexaTrigger::new();
        trig.queue_action("operator", ReflexaAction::VerifyStale { memory_id: "m1".into(), reason: "a".into() }, &path).unwrap();
        trig.queue_action("operator", ReflexaAction::VerifyStale { memory_id: "m2".into(), reason: "b".into() }, &path).unwrap();
        assert_eq!(load_queue(&path).unwrap().get("operator").unwrap().len(), 2);
    }

    #[test]
    fn cluster_dedup_is_order_independent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("q.json");
        let trig = RetrievalReflexaTrigger::new();
        let c1 = ReflexaAction::ConsolidateCluster { memory_ids: vec!["a".into(), "b".into(), "c".into()], topic: "x".into() };
        let c2 = ReflexaAction::ConsolidateCluster { memory_ids: vec!["c".into(), "a".into(), "b".into()], topic: "y".into() };
        trig.queue_action("u", c1, &path).unwrap();
        trig.queue_action("u", c2, &path).unwrap();
        assert_eq!(load_queue(&path).unwrap().get("u").unwrap().len(), 1);
    }

    // ── Per-user isolation ─────────────────────────────────────────────────────

    #[test]
    fn queue_is_per_user_isolated() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("q.json");
        let trig = RetrievalReflexaTrigger::new();
        trig.queue_action("alice", ReflexaAction::VerifyStale { memory_id: "m1".into(), reason: "r".into() }, &path).unwrap();
        trig.queue_action("bob", ReflexaAction::VerifyStale { memory_id: "m1".into(), reason: "r".into() }, &path).unwrap();
        let q = load_queue(&path).unwrap();
        // Same memory_id under different users coexist.
        assert_eq!(q.get("alice").unwrap().len(), 1);
        assert_eq!(q.get("bob").unwrap().len(), 1);
    }

    #[test]
    #[serial]
    fn per_user_queue_paths_differ() {
        // reflexa_queue_path keys on the user dir → distinct per user.
        let dir = tempdir().unwrap();
        std::env::set_var("LUMINA_PROMPT_DIR", dir.path());
        let a = reflexa_queue_path("alice");
        let b = reflexa_queue_path("bob");
        std::env::remove_var("LUMINA_PROMPT_DIR");
        assert_ne!(a, b);
        assert!(a.to_string_lossy().contains("alice"));
        assert!(b.to_string_lossy().contains("bob"));
    }

    // ── process_queue ──────────────────────────────────────────────────────────

    #[test]
    fn process_queue_handles_each_type_and_clears() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("q.json");
        let trig = RetrievalReflexaTrigger::new();
        let now = DAY * 400;

        trig.queue_action("u", ReflexaAction::ResolveContradiction { memory_id: "c".into(), digest_excerpt: "digest".into() }, &path).unwrap();
        trig.queue_action("u", ReflexaAction::ConsolidateCluster { memory_ids: vec!["a".into(), "b".into(), "x".into()], topic: "t".into() }, &path).unwrap();
        trig.queue_action("u", ReflexaAction::FlagDigestGap { memory_id: "g".into(), importance: "principle".into() }, &path).unwrap();
        // Stale, not accessed recently → archived.
        trig.queue_action("u", ReflexaAction::VerifyStale { memory_id: "s_old".into(), reason: "r".into() }, &path).unwrap();
        // Stale, accessed recently → kept.
        trig.queue_action("u", ReflexaAction::VerifyStale { memory_id: "s_new".into(), reason: "r".into() }, &path).unwrap();

        let mut last = HashMap::new();
        last.insert("s_old".to_string(), now - DAY * 120); // long ago
        last.insert("s_new".to_string(), now - DAY * 2); // recent

        let llm = MockGenerator::returning("the operator no longer likes dark roast.");
        let (outcome, resolutions) = trig
            .process_queue("u", &path, &llm, "archive text", &last, now)
            .unwrap();

        assert_eq!(outcome.contradictions_resolved, 1);
        assert_eq!(outcome.clusters_merged, 1);
        assert_eq!(outcome.digest_gaps_flagged, 1);
        assert_eq!(outcome.stale_archived, 1);
        assert_eq!(outcome.stale_kept, 1);

        assert!(resolutions.iter().any(|r| matches!(r, Resolution::ArchiveStale { memory_id } if memory_id == "s_old")));
        assert!(!resolutions.iter().any(|r| matches!(r, Resolution::ArchiveStale { memory_id } if memory_id == "s_new")));
        assert!(resolutions.iter().any(|r| matches!(r, Resolution::SupersedeContradiction { wrong_memory_id, .. } if wrong_memory_id == "c")));

        // Queue cleared for the user.
        let q = load_queue(&path).unwrap();
        assert!(q.get("u").is_none(), "user bucket cleared after processing");
    }

    #[test]
    fn process_queue_empty_user_is_fast_skip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("q.json");
        let trig = RetrievalReflexaTrigger::new();
        let llm = MockGenerator::default();
        let (outcome, res) = trig
            .process_queue("nobody", &path, &llm, "", &HashMap::new(), DAY)
            .unwrap();
        assert_eq!(outcome, ProcessOutcome::default());
        assert!(res.is_empty());
    }

    #[test]
    fn process_queue_only_drains_requested_user() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("q.json");
        let trig = RetrievalReflexaTrigger::new();
        trig.queue_action("alice", ReflexaAction::FlagDigestGap { memory_id: "g".into(), importance: "principle".into() }, &path).unwrap();
        trig.queue_action("bob", ReflexaAction::FlagDigestGap { memory_id: "h".into(), importance: "principle".into() }, &path).unwrap();
        let llm = MockGenerator::default();
        trig.process_queue("alice", &path, &llm, "", &HashMap::new(), DAY).unwrap();
        let q = load_queue(&path).unwrap();
        assert!(q.get("alice").is_none());
        assert_eq!(q.get("bob").unwrap().len(), 1, "bob's queue untouched");
    }

    #[test]
    fn process_queue_redefers_on_llm_failure() {
        // A generator that errors lets us exercise the re-defer branch.
        struct FailingLlm;
        impl LlmGenerator for FailingLlm {
            fn generate(&self, _: &str, _: &str, _: &str) -> Result<String> {
                Err(crate::error::LuminaError::Chord("timeout".into()))
            }
        }
        let dir = tempdir().unwrap();
        let path = dir.path().join("q.json");
        let trig = RetrievalReflexaTrigger::new();
        trig.queue_action("u", ReflexaAction::ResolveContradiction { memory_id: "c".into(), digest_excerpt: "d".into() }, &path).unwrap();
        let (outcome, res) = trig
            .process_queue("u", &path, &FailingLlm, "archive", &HashMap::new(), DAY)
            .unwrap();
        assert_eq!(outcome.deferred_again, 1);
        assert_eq!(outcome.contradictions_resolved, 0);
        assert!(res.is_empty());
        // Action remains queued for next night.
        assert_eq!(load_queue(&path).unwrap().get("u").unwrap().len(), 1);
    }

    // ── Spike threshold ────────────────────────────────────────────────────────

    #[test]
    fn spike_threshold_triggers_above_five() {
        let trig = RetrievalReflexaTrigger::new();
        assert!(!trig.should_trigger_immediate_reconstruction(5));
        assert!(trig.should_trigger_immediate_reconstruction(6));
        // Free function mirrors the method.
        assert!(!should_trigger_immediate_reconstruction(5));
        assert!(should_trigger_immediate_reconstruction(6));
    }

    // ── Serde round-trip ────────────────────────────────────────────────────────

    #[test]
    fn action_serde_round_trip() {
        let actions = vec![
            ReflexaAction::ResolveContradiction { memory_id: "1".into(), digest_excerpt: "d".into() },
            ReflexaAction::VerifyStale { memory_id: "2".into(), reason: "r".into() },
            ReflexaAction::ConsolidateCluster { memory_ids: vec!["3".into(), "4".into()], topic: "t".into() },
            ReflexaAction::FlagDigestGap { memory_id: "5".into(), importance: "principle".into() },
        ];
        let json = serde_json::to_string(&actions).unwrap();
        let back: Vec<ReflexaAction> = serde_json::from_str(&json).unwrap();
        assert_eq!(actions, back);
    }

    // ── No hardcoded IPs / personal data in production code ──────────────────────

    #[test]
    fn no_hardcoded_ips_in_source() {
        let src = include_str!("retrieval_reflexa.rs");
        let ip_prefix = format!("{}.{}", "192", "168");
        assert!(!src.contains(&ip_prefix), "no hardcoded IPs");
    }
}
