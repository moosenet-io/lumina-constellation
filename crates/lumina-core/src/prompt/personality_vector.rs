//! DPROMPT-04: Personality Vector — weekly behavioural reconstruction.
//!
//! Once a week (Sunday 3am, driven by the sleep-time orchestrator in Wave 3),
//! Lumina reconstructs the `[personality]` layer from two sources:
//!
//! 1. **Stated principles** — `Principle`-type Engram memories (what the user
//!    has *told* Lumina about their preferences), passed in as strings.
//! 2. **Observed behaviour** — [`BehavioralPatterns`] computed deterministically
//!    from raw conversation turns (how the user *actually* communicates).
//!
//! These, plus the current self-tuned [`TraitVector`], become a reconstruction
//! prompt for the `lumina-deep` model.  The model returns a ~200-token
//! behavioural guide describing *how* to interact with the user (not *what*
//! they know); we enforce the 200-token budget and write it to
//! `personality-vector.txt` in the user's layer directory.
//!
//! The builder makes no network calls itself — it depends on the
//! [`LlmGenerator`] seam so it is fully unit-testable with [`MockGenerator`].

use crate::error::Result;
use std::path::Path;

use super::behavioral_analysis::BehavioralPatterns;
use super::layers::{self, truncate_to_tokens};
use super::llm::LlmGenerator;
use super::traits::TraitVector;

/// Filename of the personality-vector layer within a user's layer dir.
pub const PERSONALITY_VECTOR_FILE: &str = "personality-vector.txt";

/// Hard token budget for the personality vector layer (~200 tokens per spec).
pub const PERSONALITY_TOKEN_LIMIT: usize = 200;

/// Minimum analysable conversations before behavioural analysis is included.
///
/// Below this, the reconstruction uses stated principles only (spec edge case:
/// "<5 conversations total → skip behavioral analysis, use principles only").
pub const MIN_CONVERSATIONS_FOR_BEHAVIOR: usize = 5;

/// Model hint for the deep reconstruction call.
const DEEP_MODEL: &str = "lumina-deep";

/// Builds and persists the weekly personality vector.
#[derive(Debug, Default, Clone)]
pub struct PersonalityVectorBuilder;

impl PersonalityVectorBuilder {
    pub fn new() -> Self {
        PersonalityVectorBuilder
    }

    /// Reconstruct the personality vector for `user_id` and write it to
    /// `{out_dir}/personality-vector.txt`.
    ///
    /// * `principles` — stated preference/principle memories (may be empty).
    /// * `patterns` — behavioural stats from raw turns (may be empty when there
    ///   are too few conversations; see [`MIN_CONVERSATIONS_FOR_BEHAVIOR`]).
    /// * `traits` — the current self-tuned trait vector (referenced, not set).
    /// * `gen` — the LLM seam (production: Chord; tests: [`MockGenerator`]).
    ///
    /// Returns the written (token-capped) personality vector text.  When
    /// `patterns` has fewer than [`MIN_CONVERSATIONS_FOR_BEHAVIOR`] messages it
    /// is treated as absent.  When both principles and behaviour are absent a
    /// minimal principles-only prompt is still issued (the model is told it has
    /// little to go on) so the layer always has *some* grounded guidance.
    pub fn reconstruct(
        &self,
        user_id: &str,
        principles: &[String],
        patterns: &BehavioralPatterns,
        traits: &TraitVector,
        gen: &dyn LlmGenerator,
        out_dir: &Path,
    ) -> Result<String> {
        let have_behavior =
            !patterns.is_empty() && patterns.message_count >= MIN_CONVERSATIONS_FOR_BEHAVIOR;
        let usable_principles: Vec<&String> =
            principles.iter().filter(|p| !p.trim().is_empty()).collect();

        let prompt = build_prompt(user_id, &usable_principles, patterns, have_behavior, traits);

        let raw = gen.generate(DEEP_MODEL, RECONSTRUCTION_SYSTEM, &prompt)?;
        let vector = enforce_limit(&raw);

        let path = out_dir.join(PERSONALITY_VECTOR_FILE);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, &vector)?;

        log::info!(
            "personality vector reconstructed for {user_id}: {} tokens ({} principles, behavior={})",
            layers::estimate_tokens(&vector),
            usable_principles.len(),
            have_behavior,
        );
        Ok(vector)
    }
}

/// System prompt for the reconstruction call (the role framing).
const RECONSTRUCTION_SYSTEM: &str =
    "You are reconstructing your understanding of how a person communicates so you \
     can adapt your own behaviour to them.";

/// Build the user prompt per the DPROMPT-04 spec, including only the sections
/// that have content.
fn build_prompt(
    _user_id: &str,
    principles: &[&String],
    patterns: &BehavioralPatterns,
    have_behavior: bool,
    traits: &TraitVector,
) -> String {
    let mut p = String::new();
    p.push_str("You are reconstructing your understanding of how a person communicates.\n\n");

    if principles.is_empty() {
        p.push_str(
            "Their stated principles: none recorded yet — rely on observed behaviour.\n\n",
        );
    } else {
        p.push_str("Their stated principles (things they've told you about their preferences):\n");
        for pr in principles {
            p.push_str("- ");
            p.push_str(pr.trim());
            p.push('\n');
        }
        p.push('\n');
    }

    if have_behavior {
        p.push_str(&format!(
            "Their behavioral patterns (observed from {} conversations):\n",
            patterns.message_count
        ));
        p.push_str(&patterns.summary());
        p.push_str("\n\n");
    } else {
        p.push_str(
            "Their behavioral patterns: too few conversations so far to analyse reliably.\n\n",
        );
    }

    let t = traits.clamped();
    p.push_str(&format!(
        "Current personality traits: flair={:.2}, spontaneity={:.2}, humor={:.2}, focus={:.2}\n\n",
        t.flair, t.spontaneity, t.humor, t.focus
    ));

    p.push_str(
        "Write a 200-word behavioral guide for yourself — how should you communicate with this \
         person? Focus on HOW to interact, not WHAT they know. Include specific guidance like \
         \"keep responses under X sentences\" or \"always lead with the actionable item.\"",
    );
    p
}

/// Enforce the 200-token limit on the generated vector, trimming whitespace.
fn enforce_limit(raw: &str) -> String {
    truncate_to_tokens(raw.trim(), PERSONALITY_TOKEN_LIMIT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::behavioral_analysis::{BehavioralAnalyzer, RawTurn};
    use crate::prompt::llm::MockGenerator;
    use tempfile::tempdir;

    fn sample_patterns(n: usize) -> BehavioralPatterns {
        let turns: Vec<RawTurn> = (0..n)
            .map(|i| RawTurn::new(format!("check status of service {i}?"), "ok"))
            .collect();
        BehavioralAnalyzer::new().extract_patterns(&turns)
    }

    #[test]
    fn writes_file_and_returns_text() {
        let dir = tempdir().unwrap();
        let gen = MockGenerator::returning("Lead with the answer. Keep replies under three sentences.");
        let out = PersonalityVectorBuilder::new()
            .reconstruct(
                "operator",
                &["Prefers direct feedback".to_string()],
                &sample_patterns(10),
                &TraitVector::default(),
                &gen,
                dir.path(),
            )
            .unwrap();
        assert!(out.contains("Lead with the answer"));
        let written =
            std::fs::read_to_string(dir.path().join(PERSONALITY_VECTOR_FILE)).unwrap();
        assert_eq!(written, out);
    }

    #[test]
    fn prompt_includes_principles_and_behavioral_data() {
        let dir = tempdir().unwrap();
        // Echo generator returns the user prompt so we can inspect it.
        let gen = MockGenerator::default();
        let out = PersonalityVectorBuilder::new()
            .reconstruct(
                "operator",
                &["Prefers systems thinking".to_string(), "Wants honest pushback".to_string()],
                &sample_patterns(8),
                &TraitVector::default(),
                &gen,
                dir.path(),
            )
            .unwrap();
        // Principles present
        assert!(out.contains("Prefers systems thinking"));
        assert!(out.contains("Wants honest pushback"));
        // Behavioral block present
        assert!(out.contains("behavioral patterns (observed from 8 conversations)"));
        assert!(out.contains("Questions:"));
        // Trait values referenced
        assert!(out.contains("flair=0.70"));
        assert!(out.contains("focus=0.75"));
        // The instruction line
        assert!(out.contains("200-word behavioral guide"));
    }

    #[test]
    fn enforces_200_token_limit() {
        let dir = tempdir().unwrap();
        // A 600-word blob → well over 200 tokens (~800).
        let huge = "word ".repeat(600);
        let gen = MockGenerator::returning(huge);
        let out = PersonalityVectorBuilder::new()
            .reconstruct(
                "operator",
                &["p".to_string()],
                &sample_patterns(10),
                &TraitVector::default(),
                &gen,
                dir.path(),
            )
            .unwrap();
        assert!(
            layers::estimate_tokens(&out) <= PERSONALITY_TOKEN_LIMIT,
            "tokens: {}",
            layers::estimate_tokens(&out)
        );
        assert!(out.ends_with('…'));
    }

    #[test]
    fn few_conversations_skips_behavioral_analysis() {
        let dir = tempdir().unwrap();
        let gen = MockGenerator::default(); // echo prompt
        let out = PersonalityVectorBuilder::new()
            .reconstruct(
                "operator",
                &["Prefers direct feedback".to_string()],
                &sample_patterns(3), // below MIN_CONVERSATIONS_FOR_BEHAVIOR
                &TraitVector::default(),
                &gen,
                dir.path(),
            )
            .unwrap();
        assert!(out.contains("too few conversations"));
        assert!(!out.contains("Questions:"));
        // Principles still present.
        assert!(out.contains("Prefers direct feedback"));
    }

    #[test]
    fn no_principles_uses_behavioral_only() {
        let dir = tempdir().unwrap();
        let gen = MockGenerator::default(); // echo prompt
        let out = PersonalityVectorBuilder::new()
            .reconstruct(
                "operator",
                &[],
                &sample_patterns(12),
                &TraitVector::default(),
                &gen,
                dir.path(),
            )
            .unwrap();
        assert!(out.contains("none recorded yet"));
        // Behavioral block still present.
        assert!(out.contains("behavioral patterns (observed from 12 conversations)"));
    }

    #[test]
    fn blank_principles_are_filtered_out() {
        let dir = tempdir().unwrap();
        let gen = MockGenerator::default();
        let out = PersonalityVectorBuilder::new()
            .reconstruct(
                "operator",
                &["  ".to_string(), "\n".to_string()],
                &sample_patterns(8),
                &TraitVector::default(),
                &gen,
                dir.path(),
            )
            .unwrap();
        // All principles blank → treated as none.
        assert!(out.contains("none recorded yet"));
    }

    #[test]
    fn empty_patterns_treated_as_too_few() {
        let dir = tempdir().unwrap();
        let gen = MockGenerator::default();
        let out = PersonalityVectorBuilder::new()
            .reconstruct(
                "operator",
                &["p".to_string()],
                &BehavioralPatterns::empty(),
                &TraitVector::default(),
                &gen,
                dir.path(),
            )
            .unwrap();
        assert!(out.contains("too few conversations"));
    }
}
