//! DPROMPT-13: Opinion formation engine — weekly sleep-time consolidation.
//!
//! Once a week Lumina reviews her Knowledge Digest, her abstracted Principle
//! memories, the themes of the last week's conversations, and her current
//! trait vector, and forms 3–5 *grounded* opinions — inferences and
//! perspectives that emerge from real data, not hallucinated positions.  These
//! become the `[opinions]` layer (see [`crate::prompt::layers::LayerKind::Opinions`]),
//! a small rotating block of ~50–80 tokens rendered between the personality
//! vector and the knowledge digest.
//!
//! ## Design
//! * Pure and synchronous: the digest / principles / themes / traits all arrive
//!   as arguments and generation goes through the shared [`LlmGenerator`] seam,
//!   so the whole engine is unit-testable with no live Engram DB or Chord proxy.
//!   Wave 3 wires [`OpinionEngine::form_opinions`] into the weekly
//!   [`super::sleep_time`] orchestrator.
//! * Safety in depth: the generation prompt *instructs* the model to avoid
//!   politics / religion / controversial / judgmental content, and a
//!   post-generation keyword scan drops any opinion that slips through anyway.
//! * Token budget: 50–80 tokens.  If 5 opinions exceed the budget we trim to 3.
//! * Staleness: [`OpinionEngine::load_fresh`] returns the block only if it is
//!   younger than 14 days; older blocks are dropped ("stale opinions feel weird").
//! * No network, no chrono — `now_secs` (Unix seconds) is passed in by the
//!   caller, keeping the engine deterministic and testable.
//!
//! [`LlmGenerator`]: super::llm::LlmGenerator

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::prompt::layers::estimate_tokens;
use crate::prompt::llm::LlmGenerator;
use crate::prompt::traits::TraitVector;

/// Rendered opinions block filename within a user's layer directory.
/// Mirrors the `Opinions` entry in [`crate::prompt::layers::LAYERS`].
pub const OPINIONS_FILE: &str = "opinions.txt";

/// Provenance sidecar (the [`OpinionSet`] as JSON), kept alongside the block.
pub const OPINIONS_META_FILE: &str = "opinions.json";

/// Deep model tier — opinion quality matters, so use the deep model.
pub const DEEP_MODEL_HINT: &str = "lumina-deep";

/// Soft lower bound of the opinions token budget.
pub const MIN_OPINION_TOKENS: usize = 50;
/// Hard upper bound of the opinions token budget.
pub const MAX_OPINION_TOKENS: usize = 80;

/// Minimum / maximum number of opinions per set.
pub const MIN_OPINIONS: usize = 3;
pub const MAX_OPINIONS: usize = 5;

/// Opinions older than this are dropped on load.  14 days, in seconds.
pub const STALENESS_SECS: i64 = 14 * 24 * 60 * 60;

/// Prefix of the rendered block (kept in one place so the parser and the
/// renderer agree).
pub const BLOCK_PREFIX: &str = "Things on my mind: ";

/// Banned topics: any opinion mentioning one of these (case-insensitive,
/// substring) is dropped post-generation.  Deliberately small and conservative
/// — the generation prompt is the first line of defence; this is the net.
const BANNED_TOPICS: &[&str] = &[
    // politics
    "politic", "election", "democrat", "republican", "liberal", "conservative",
    "president", "congress", "senator", "vote", "government", "immigration",
    // religion
    "religion", "religious", "god", "church", "mosque", "synagogue", "islam",
    "christian", "muslim", "jewish", "atheist", "bible", "quran", "pray",
    // controversial / social-issue flashpoints
    "abortion", "gun control", "vaccine", "climate change", "racism",
    "racist", "sexist", "lgbt", "transgender", "gender identity",
];

/// A weekly set of grounded opinions plus provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpinionSet {
    /// The opinions, one short sentence each (3–5 after enforcement).
    pub opinions: Vec<String>,
    /// Unix seconds at which this set was generated.
    pub generated_at: i64,
    /// Provenance: which memories / digest fragments informed the opinions.
    pub source_memories: Vec<String>,
}

impl OpinionSet {
    /// Render the opinions as the `[opinions]` block body.
    ///
    /// `"Things on my mind: <op1> <op2> …"` — each opinion ends with a period
    /// so they read as one flowing paragraph.
    pub fn render_block(&self) -> String {
        if self.opinions.is_empty() {
            return String::new();
        }
        let body = self
            .opinions
            .iter()
            .map(|o| ensure_sentence(o))
            .collect::<Vec<_>>()
            .join(" ");
        format!("{BLOCK_PREFIX}{body}")
    }
}

/// Opinion formation engine.  Stateless; construct with [`OpinionEngine::new`].
#[derive(Debug, Clone, Default)]
pub struct OpinionEngine;

impl OpinionEngine {
    pub fn new() -> Self {
        OpinionEngine
    }

    /// Form this week's opinions for `user_id` and persist them under `out_dir`
    /// (the user's per-user layer directory).
    ///
    /// Writes the rendered block to `{out_dir}/opinions.txt` and the
    /// [`OpinionSet`] JSON (provenance) to `{out_dir}/opinions.json`.  When the
    /// user has no grounding material, or every generated opinion is filtered
    /// out, an *empty* set is written (so the stale block from a previous week
    /// stops being served) and returned — the engine retries next week.
    pub fn form_opinions(
        &self,
        user_id: &str,
        digest: &str,
        principles: &[String],
        recent_themes: &[String],
        traits: &TraitVector,
        gen: &dyn LlmGenerator,
        now_secs: i64,
        out_dir: &Path,
    ) -> Result<OpinionSet> {
        let have_digest = !digest.trim().is_empty();
        let usable_principles: Vec<&String> =
            principles.iter().filter(|p| !p.trim().is_empty()).collect();
        let usable_themes: Vec<&String> =
            recent_themes.iter().filter(|t| !t.trim().is_empty()).collect();

        // New user / no grounding → no opinions ("still getting to know you").
        // Persist an empty set so any stale block is cleared, and bail.
        if !have_digest && usable_principles.is_empty() && usable_themes.is_empty() {
            let empty = OpinionSet { opinions: vec![], generated_at: now_secs, source_memories: vec![] };
            persist(out_dir, &empty)?;
            return Ok(empty);
        }

        let prompt = build_prompt(user_id, digest, &usable_principles, &usable_themes, traits);
        let raw = gen.generate(DEEP_MODEL_HINT, OPINION_SYSTEM, &prompt)?;

        // Parse one opinion per line, drop blanks / list bullets.
        let parsed: Vec<String> = raw
            .lines()
            .map(clean_line)
            .filter(|l| !l.is_empty())
            .collect();

        // Post-generation hard filter: drop anything touching a banned topic.
        let mut kept: Vec<String> = parsed.into_iter().filter(|o| !is_banned(o)).collect();

        // Cap to MAX_OPINIONS first, then enforce the token budget.
        if kept.len() > MAX_OPINIONS {
            kept.truncate(MAX_OPINIONS);
        }
        kept = enforce_token_budget(kept);

        // Provenance: the grounding material that informed this set.
        let source_memories: Vec<String> = if kept.is_empty() {
            vec![]
        } else {
            build_provenance(digest, &usable_principles, &usable_themes)
        };

        let set = OpinionSet { opinions: kept, generated_at: now_secs, source_memories };
        persist(out_dir, &set)?;

        log::info!(
            "opinions formed for {user_id}: {} opinion(s), ~{} tokens",
            set.opinions.len(),
            estimate_tokens(&set.render_block()),
        );
        Ok(set)
    }

    /// Load the rendered opinions block for a user iff it is fresh (younger than
    /// 14 days).  Returns `None` when the file/meta is missing, unreadable,
    /// empty, or stale — the assembler then renders an empty `[opinions]` layer.
    pub fn load_fresh(out_dir: &Path, now_secs: i64) -> Option<String> {
        let meta_path = out_dir.join(OPINIONS_META_FILE);
        let raw = std::fs::read_to_string(&meta_path).ok()?;
        let set: OpinionSet = serde_json::from_str(&raw).ok()?;

        if set.opinions.is_empty() {
            return None;
        }
        // Drop stale (and ignore clock-skew "future" sets defensively).
        let age = now_secs.saturating_sub(set.generated_at);
        if age < 0 || age > STALENESS_SECS {
            return None;
        }

        let block = set.render_block();
        if block.trim().is_empty() {
            None
        } else {
            Some(block)
        }
    }
}

/// Persist a set: rendered block + JSON provenance.  Creates parent dirs.
fn persist(out_dir: &Path, set: &OpinionSet) -> Result<()> {
    std::fs::create_dir_all(out_dir)?;
    let block = set.render_block();
    std::fs::write(out_dir.join(OPINIONS_FILE), &block)?;
    let json = serde_json::to_string_pretty(set)?;
    std::fs::write(out_dir.join(OPINIONS_META_FILE), json)?;
    Ok(())
}

/// Trim the opinion list so the rendered block fits `MAX_OPINION_TOKENS`.
/// If 5 (or more) opinions blow the budget we trim to 3, per the spec.
fn enforce_token_budget(mut opinions: Vec<String>) -> Vec<String> {
    if opinions.is_empty() {
        return opinions;
    }
    let render = |ops: &[String]| {
        let set = OpinionSet { opinions: ops.to_vec(), generated_at: 0, source_memories: vec![] };
        estimate_tokens(&set.render_block())
    };
    // Step down to MIN_OPINIONS (3) if over budget.
    while opinions.len() > MIN_OPINIONS && render(&opinions) > MAX_OPINION_TOKENS {
        opinions.pop();
    }
    opinions
}

/// Build the provenance list from the grounding material actually used.
fn build_provenance(digest: &str, principles: &[&String], themes: &[&String]) -> Vec<String> {
    let mut out = Vec::new();
    if !digest.trim().is_empty() {
        out.push("knowledge-digest".to_string());
    }
    for p in principles {
        out.push(format!("principle: {}", truncate_provenance(p)));
    }
    for t in themes {
        out.push(format!("recent-theme: {}", truncate_provenance(t)));
    }
    out
}

/// Keep provenance entries short (first ~12 words).
fn truncate_provenance(s: &str) -> String {
    let words: Vec<&str> = s.split_whitespace().take(12).collect();
    words.join(" ")
}

/// Strip a leading list marker (`-`, `*`, `1.`) and surrounding whitespace.
fn clean_line(line: &str) -> String {
    let t = line.trim();
    let t = t.trim_start_matches(['-', '*', '•']).trim_start();
    // Strip a leading "N." / "N)" enumerator.
    let t = match t.find([')', '.']) {
        Some(idx) if idx <= 3 && t[..idx].chars().all(|c| c.is_ascii_digit()) && idx > 0 => {
            t[idx + 1..].trim_start()
        }
        _ => t,
    };
    t.to_string()
}

/// True if `opinion` mentions a banned topic (case-insensitive substring).
fn is_banned(opinion: &str) -> bool {
    let lc = opinion.to_lowercase();
    BANNED_TOPICS.iter().any(|t| lc.contains(t))
}

/// Ensure an opinion ends with terminal punctuation so the block reads cleanly.
fn ensure_sentence(s: &str) -> String {
    let t = s.trim();
    if t.ends_with(['.', '!', '?']) {
        t.to_string()
    } else {
        format!("{t}.")
    }
}

/// System prompt — sets the voice and the hard safety boundary.
const OPINION_SYSTEM: &str =
    "You are Lumina: curious, quirky, warm, and sharp when delivering info. \
     You form lightweight opinions grounded in what you actually know about the \
     user. You NEVER opine on politics, religion, controversial social issues, \
     other people's private data, or pass judgment on the user's personal \
     choices. Output only the opinions, one per line, no preamble.";

/// Build the user prompt with the spec's good/bad examples and hard filter.
fn build_prompt(
    user_id: &str,
    digest: &str,
    principles: &[&String],
    themes: &[&String],
    traits: &TraitVector,
) -> String {
    let profile = if digest.trim().is_empty() {
        "(little known yet)".to_string()
    } else {
        digest.trim().to_string()
    };
    let patterns = if principles.is_empty() {
        "(no abstracted patterns yet)".to_string()
    } else {
        principles
            .iter()
            .map(|p| format!("- {}", p.trim()))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let recent = if themes.is_empty() {
        "(no notable themes this week)".to_string()
    } else {
        themes
            .iter()
            .map(|t| format!("- {}", t.trim()))
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        "You are Lumina, reviewing what you know about {user_id} this week.\n\n\
         Their profile: {profile}\n\
         Their patterns:\n{patterns}\n\
         Recent conversations:\n{recent}\n\
         Your personality tuning: {style}\n\n\
         Based on what you know, form {min}-{max} current opinions or observations.\n\
         These should be:\n\
         - Grounded in real data (things you actually know about them)\n\
         - Helpful (connected to their life, not random)\n\
         - Opinionated (take a stance, suggest something, express a view)\n\
         - Brief (one sentence each)\n\n\
         Examples of good opinions:\n\
         - \"I think the Chord proxy migration was the right call — the architecture is much cleaner now.\"\n\
         - \"You've been working late a lot this week. Maybe Thursday evening should be screen-free.\"\n\
         - \"Based on your coffee and whiskey preferences, I bet you'd like that new tasting room downtown.\"\n\n\
         Examples of bad opinions (don't do these):\n\
         - Generic advice with no personal connection\n\
         - Opinions about politics, religion, or controversial topics\n\
         - Judgments about the user's choices\n\
         - Made-up facts presented as opinions\n\n\
         HARD RULE: never mention politics, religion, or controversial social issues.\n\n\
         Write {min}-{max} opinions, one per line:",
        style = traits.to_instructions(),
        min = MIN_OPINIONS,
        max = MAX_OPINIONS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::llm::MockGenerator;
    use tempfile::tempdir;

    fn traits() -> TraitVector {
        TraitVector::default()
    }

    fn five_opinions() -> &'static str {
        "I think the Chord migration was the right call.\n\
         You've been hiking a lot; the Alum Rock trail has great reviews.\n\
         Your morning briefing would be better with transit times.\n\
         Based on your coffee taste, you'd like that new dark roast.\n\
         You work late on Thursdays — maybe make those screen-free."
    }

    #[test]
    fn parses_three_to_five_opinions() {
        let dir = tempdir().unwrap();
        let gen = MockGenerator::returning(five_opinions());
        let set = OpinionEngine::new()
            .form_opinions("alice", "loves coffee and hiking", &[], &[], &traits(), &gen, 1_000, dir.path())
            .unwrap();
        assert!(set.opinions.len() >= MIN_OPINIONS && set.opinions.len() <= MAX_OPINIONS);
    }

    #[test]
    fn strips_list_markers() {
        let dir = tempdir().unwrap();
        let gen = MockGenerator::returning(
            "- first opinion here\n* second opinion here\n1. third opinion here",
        );
        let set = OpinionEngine::new()
            .form_opinions("bob", "digest", &[], &[], &traits(), &gen, 0, dir.path())
            .unwrap();
        assert_eq!(set.opinions.len(), 3);
        assert!(set.opinions.iter().all(|o| !o.starts_with(['-', '*'])));
        assert!(set.opinions[2].starts_with("third"));
    }

    #[test]
    fn provenance_tracked_from_grounding() {
        let dir = tempdir().unwrap();
        let gen = MockGenerator::returning("Opinion one.\nOpinion two.\nOpinion three.");
        let principles = vec!["prefers dark roast coffee".to_string()];
        let themes = vec!["asked about hiking trails repeatedly".to_string()];
        let set = OpinionEngine::new()
            .form_opinions("carol", "a digest", &principles, &themes, &traits(), &gen, 5, dir.path())
            .unwrap();
        assert!(set.source_memories.iter().any(|s| s == "knowledge-digest"));
        assert!(set.source_memories.iter().any(|s| s.starts_with("principle:")));
        assert!(set.source_memories.iter().any(|s| s.starts_with("recent-theme:")));
    }

    #[test]
    fn political_content_filtered() {
        let dir = tempdir().unwrap();
        let gen = MockGenerator::returning(
            "I think the election results were great.\n\
             You'd enjoy the new whiskey tasting room.\n\
             The president should resign.\n\
             Your briefing needs transit times.\n\
             Religion aside, you seem happy lately.",
        );
        let set = OpinionEngine::new()
            .form_opinions("dave", "digest", &[], &[], &traits(), &gen, 0, dir.path())
            .unwrap();
        // Three banned lines dropped; two clean ones survive.
        assert_eq!(set.opinions.len(), 2);
        assert!(set.opinions.iter().all(|o| !is_banned(o)));
        assert!(set.opinions.iter().any(|o| o.contains("whiskey")));
    }

    #[test]
    fn all_filtered_yields_empty_set_no_block() {
        let dir = tempdir().unwrap();
        let gen = MockGenerator::returning(
            "The election was rigged.\nVote for change.\nReligion matters most.",
        );
        let set = OpinionEngine::new()
            .form_opinions("eve", "digest", &[], &[], &traits(), &gen, 100, dir.path())
            .unwrap();
        assert!(set.opinions.is_empty());
        assert!(set.source_memories.is_empty());
        // No fresh block should be served.
        assert!(OpinionEngine::load_fresh(dir.path(), 100).is_none());
        // Block file written but empty.
        let block = std::fs::read_to_string(dir.path().join(OPINIONS_FILE)).unwrap();
        assert!(block.trim().is_empty());
    }

    #[test]
    fn token_budget_trims_to_three() {
        let dir = tempdir().unwrap();
        // Five long opinions that together blow the 80-token budget.
        let long = "this is a deliberately long winded opinion that uses many words to consume tokens";
        let raw = format!("{long} one\n{long} two\n{long} three\n{long} four\n{long} five");
        let gen = MockGenerator::returning(raw);
        let set = OpinionEngine::new()
            .form_opinions("frank", "digest", &[], &[], &traits(), &gen, 0, dir.path())
            .unwrap();
        assert_eq!(set.opinions.len(), MIN_OPINIONS);
        assert!(estimate_tokens(&set.render_block()) <= MAX_OPINION_TOKENS);
    }

    #[test]
    fn within_budget_keeps_all_five() {
        let dir = tempdir().unwrap();
        let gen = MockGenerator::returning("a b.\nc d.\ne f.\ng h.\ni j.");
        let set = OpinionEngine::new()
            .form_opinions("grace", "digest", &[], &[], &traits(), &gen, 0, dir.path())
            .unwrap();
        assert_eq!(set.opinions.len(), MAX_OPINIONS);
    }

    #[test]
    fn fresh_block_loads_and_renders() {
        let dir = tempdir().unwrap();
        let gen = MockGenerator::returning("Opinion one.\nOpinion two.\nOpinion three.");
        OpinionEngine::new()
            .form_opinions("hank", "digest", &[], &[], &traits(), &gen, 1_000_000, dir.path())
            .unwrap();
        let block = OpinionEngine::load_fresh(dir.path(), 1_000_000).unwrap();
        assert!(block.starts_with(BLOCK_PREFIX));
        assert!(block.contains("Opinion one."));
    }

    #[test]
    fn staleness_drops_after_14_days() {
        let dir = tempdir().unwrap();
        let gen = MockGenerator::returning("Opinion one.\nOpinion two.\nOpinion three.");
        let gen_at = 1_000_000_i64;
        OpinionEngine::new()
            .form_opinions("ivy", "digest", &[], &[], &traits(), &gen, gen_at, dir.path())
            .unwrap();
        // 13 days later → still fresh.
        assert!(OpinionEngine::load_fresh(dir.path(), gen_at + 13 * 24 * 3600).is_some());
        // 15 days later → stale, dropped.
        assert!(OpinionEngine::load_fresh(dir.path(), gen_at + 15 * 24 * 3600).is_none());
    }

    #[test]
    fn missing_meta_loads_none() {
        let dir = tempdir().unwrap();
        assert!(OpinionEngine::load_fresh(dir.path(), 0).is_none());
    }

    #[test]
    fn new_user_no_memories_forms_nothing() {
        let dir = tempdir().unwrap();
        let gen = MockGenerator::returning("should not be used");
        let set = OpinionEngine::new()
            .form_opinions("newbie", "", &[], &[], &traits(), &gen, 42, dir.path())
            .unwrap();
        assert!(set.opinions.is_empty());
        assert!(OpinionEngine::load_fresh(dir.path(), 42).is_none());
    }

    #[test]
    fn per_user_paths_isolated() {
        let root = tempdir().unwrap();
        let dir_a = root.path().join("alice");
        let dir_b = root.path().join("bob");
        let gen_a = MockGenerator::returning("Alice opinion one.\nAlice two.\nAlice three.");
        let gen_b = MockGenerator::returning("Bob opinion one.\nBob two.\nBob three.");
        let eng = OpinionEngine::new();
        eng.form_opinions("alice", "d", &[], &[], &traits(), &gen_a, 10, &dir_a).unwrap();
        eng.form_opinions("bob", "d", &[], &[], &traits(), &gen_b, 10, &dir_b).unwrap();
        let a = OpinionEngine::load_fresh(&dir_a, 10).unwrap();
        let b = OpinionEngine::load_fresh(&dir_b, 10).unwrap();
        assert!(a.contains("Alice"));
        assert!(!a.contains("Bob"));
        assert!(b.contains("Bob"));
        assert!(!b.contains("Alice"));
    }

    #[test]
    fn render_block_adds_terminal_punctuation() {
        let set = OpinionSet {
            opinions: vec!["no period here".to_string(), "already done.".to_string()],
            generated_at: 0,
            source_memories: vec![],
        };
        let block = set.render_block();
        assert!(block.contains("no period here."));
        assert!(block.contains("already done."));
    }

    #[test]
    fn prompt_contains_examples_and_hard_filter() {
        let p = build_prompt("zoe", "loves coffee", &[], &[], &traits());
        assert!(p.contains("Examples of good opinions"));
        assert!(p.contains("Examples of bad opinions"));
        assert!(p.to_lowercase().contains("never mention politics"));
        // Trait instruction is woven in.
        assert!(p.contains(&traits().to_instructions()));
    }
}
