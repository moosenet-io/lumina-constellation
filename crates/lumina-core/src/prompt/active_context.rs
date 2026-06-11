//! DPROMPT-05: Active Context — per-session theme summarization.
//!
//! At the start of each conversation session (first message after an idle
//! timeout or an explicit `/new`) Lumina summarises her last few sessions into
//! a compact `[context]` layer.  This gives her continuity: she knows what was
//! discussed recently and can reference it naturally without the user repeating
//! themselves.
//!
//! ## Design
//! * The builder is pure and synchronous: session data comes in as a slice of
//!   [`SessionSummary`] and generation goes through the shared [`LlmGenerator`]
//!   seam, so the whole thing is unit-testable with no conversation store or
//!   live Chord proxy.  The integration wave supplies real summaries from the
//!   conversation store / DPROMPT-08 archive and wires [`build_from_sessions`]
//!   into the session-start path in `agent_loop`.
//! * This layer is SEPARATE from the raw conversation history.  History is the
//!   verbatim message log; this is the distilled themes.
//! * Speed over quality: it runs once at session start and uses the fast model
//!   tier ([`FAST_MODEL_HINT`]).
//! * No network, no chrono — dates arrive as pre-formatted strings on the
//!   [`SessionSummary`] inputs.
//!
//! [`LlmGenerator`]: super::llm::LlmGenerator

use std::path::Path;

use crate::error::Result;
use crate::prompt::layers::truncate_to_tokens;
use crate::prompt::llm::LlmGenerator;

/// Filename of the active-context layer within a user's layer directory.
/// Mirrors the `Context` entry in [`crate::prompt::layers::LAYERS`].
pub const CONTEXT_FILENAME: &str = "active-context.txt";

/// Model tier used for summarization — speed over quality, so the fast model.
pub const FAST_MODEL_HINT: &str = "lumina-fast";

/// Hard cap on the assembled context block, per the spec ("150-token block").
pub const CONTEXT_TOKEN_LIMIT: usize = 150;

/// How many most-recent sessions are folded into the summary.
pub const MAX_SESSIONS: usize = 3;

/// A session with fewer than this many turns is flagged as "very short" so the
/// model is told to note it briefly rather than pad it out.
pub const SHORT_SESSION_TURNS: usize = 2;

/// Returned (and written) when the user has no previous sessions.
pub const FIRST_CONVERSATION: &str =
    "This is our first conversation. Let's get to know each other.";

/// A reduced projection of one past conversation session — only what the
/// summariser needs.  Defined here (rather than depending on a conversation
/// store type) so the builder is testable without a DB.
///
/// The integration wave maps real rows from the conversation store /
/// DPROMPT-08 `conversation_archive` into this shape.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSummary {
    /// Pre-formatted session date, e.g. `"2026-06-08"`.  A plain string so the
    /// builder pulls in no clock/`chrono` dependency.
    pub date: String,
    /// Number of conversational turns in the session.
    pub turn_count: usize,
    /// The first user message of the session.
    pub first_user_msg: String,
    /// The last user message of the session.
    pub last_user_msg: String,
}

impl SessionSummary {
    /// Convenience constructor.
    pub fn new(
        date: impl Into<String>,
        turn_count: usize,
        first_user_msg: impl Into<String>,
        last_user_msg: impl Into<String>,
    ) -> Self {
        SessionSummary {
            date: date.into(),
            turn_count,
            first_user_msg: first_user_msg.into(),
            last_user_msg: last_user_msg.into(),
        }
    }

    /// Whether this session is short enough to be noted only briefly.
    fn is_very_short(&self) -> bool {
        self.turn_count <= SHORT_SESSION_TURNS
    }
}

/// Builds and persists the active-context layer.
#[derive(Debug, Clone, Default)]
pub struct ActiveContextBuilder;

impl ActiveContextBuilder {
    pub fn new() -> Self {
        ActiveContextBuilder
    }

    /// Build the active-context block for `user_id` from `sessions` and write it
    /// to `out_dir/active-context.txt`.
    ///
    /// Returns the context text on success.
    ///
    /// * No previous sessions → [`FIRST_CONVERSATION`] (written + returned, no
    ///   LLM call).
    /// * Otherwise the last [`MAX_SESSIONS`] sessions (most-recent first) are
    ///   summarised by the fast model and the result is capped to
    ///   [`CONTEXT_TOKEN_LIMIT`].
    ///
    /// On generator error any pre-existing context file is **preserved** (never
    /// overwritten) and the error is returned.
    pub fn build_from_sessions(
        &self,
        user_id: &str,
        sessions: &[SessionSummary],
        gen: &dyn LlmGenerator,
        out_dir: &Path,
    ) -> Result<String> {
        // No history → first-conversation greeting. Safe to write (nothing to
        // lose) and cheap (no LLM call).
        if sessions.is_empty() {
            write_context(out_dir, FIRST_CONVERSATION)?;
            log::info!("active context: first conversation for user (0 prior sessions)");
            return Ok(FIRST_CONVERSATION.to_string());
        }

        // Take only the most-recent MAX_SESSIONS. Inputs are most-recent first.
        let recent: &[SessionSummary] = &sessions[..sessions.len().min(MAX_SESSIONS)];

        let user_prompt = build_summarization_prompt(recent);
        let raw = match gen.generate(FAST_MODEL_HINT, SUMMARIZATION_SYSTEM, &user_prompt) {
            Ok(text) => text,
            Err(e) => {
                // Preserve any existing context — do NOT overwrite on failure.
                log::warn!("active context summarization failed ({e}); preserving previous context");
                return Err(e);
            }
        };

        let context = truncate_to_tokens(raw.trim(), CONTEXT_TOKEN_LIMIT);
        if context.trim().is_empty() {
            // Generator returned nothing usable — preserve the previous file and
            // surface the first-conversation fallback so the layer is never blank.
            log::warn!("active context summarization produced empty output; using fallback");
            write_context(out_dir, FIRST_CONVERSATION)?;
            return Ok(FIRST_CONVERSATION.to_string());
        }

        write_context(out_dir, &context)?;
        log::info!(
            "active context reconstructed: {} words from {} session(s)",
            word_count(&context),
            recent.len()
        );
        Ok(context)
    }
}

/// System prompt for the summarization call.
const SUMMARIZATION_SYSTEM: &str =
    "You are Lumina, recalling the recent thread of conversation with the person you assist.";

/// Build the user prompt per the DPROMPT-05 spec.
fn build_summarization_prompt(sessions: &[SessionSummary]) -> String {
    let mut p = String::new();
    p.push_str("Summarize these recent conversation sessions in 150 words.\n");
    p.push_str("Focus on: what topics were discussed, any decisions made,\n");
    p.push_str("any unresolved items, and what the user might follow up on.\n\n");

    for (i, s) in sessions.iter().enumerate() {
        // Session 1 is the most recent.
        p.push_str(&format!(
            "Session {} ({}, {} turn{}): started with \"{}\", ended with \"{}\"",
            i + 1,
            s.date.trim(),
            s.turn_count,
            if s.turn_count == 1 { "" } else { "s" },
            s.first_user_msg.trim(),
            s.last_user_msg.trim(),
        ));
        if s.is_very_short() {
            p.push_str(" (very short — note only briefly)");
        }
        p.push('\n');
    }
    p
}

/// Write the context to `out_dir/active-context.txt`, creating the directory.
fn write_context(out_dir: &Path, context: &str) -> Result<()> {
    std::fs::create_dir_all(out_dir)?;
    std::fs::write(out_dir.join(CONTEXT_FILENAME), context)?;
    Ok(())
}

fn word_count(s: &str) -> usize {
    s.split_whitespace().count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::LuminaError;
    use crate::prompt::layers::estimate_tokens;
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

    fn context_path(dir: &Path) -> PathBuf {
        dir.join(CONTEXT_FILENAME)
    }

    fn sessions(n: usize) -> Vec<SessionSummary> {
        (0..n)
            .map(|i| {
                SessionSummary::new(
                    format!("2026-06-{:02}", i + 1),
                    5 + i,
                    format!("FIRSTMSG{i}"),
                    format!("LASTMSG{i}"),
                )
            })
            .collect()
    }

    #[test]
    fn no_sessions_returns_first_conversation_without_llm() {
        let dir = tempdir().unwrap();
        // Canned text must NOT be used — proves no LLM call on the empty path.
        let gen = MockGenerator::returning("SHOULD NOT BE USED");
        let out = ActiveContextBuilder::new()
            .build_from_sessions("operator", &[], &gen, dir.path())
            .unwrap();
        assert_eq!(out, FIRST_CONVERSATION);
        assert_eq!(
            std::fs::read_to_string(context_path(dir.path())).unwrap(),
            FIRST_CONVERSATION
        );
    }

    #[test]
    fn built_from_three_sessions() {
        let dir = tempdir().unwrap();
        let s = sessions(3);
        // Echo generator returns the prompt so we can inspect what was sent.
        let gen = MockGenerator::default();
        let out = ActiveContextBuilder::new()
            .build_from_sessions("operator", &s, &gen, dir.path())
            .unwrap();
        // Spec framing present.
        assert!(out.contains("Summarize these recent conversation sessions"));
        // All three sessions, labelled 1..=3, with their messages.
        for (i, sess) in s.iter().enumerate() {
            assert!(out.contains(&format!("Session {}", i + 1)), "missing session label {}", i + 1);
            assert!(out.contains(&sess.first_user_msg), "missing first msg of session {i}");
            assert!(out.contains(&sess.last_user_msg), "missing last msg of session {i}");
        }
        // Written to disk.
        assert_eq!(std::fs::read_to_string(context_path(dir.path())).unwrap(), out);
    }

    #[test]
    fn only_last_three_sessions_used() {
        let dir = tempdir().unwrap();
        // 5 sessions provided (most-recent first); only the first 3 are folded in.
        let mut s = sessions(3);
        s.push(SessionSummary::new("2026-05-01", 9, "OLDFIRST", "OLDLAST"));
        s.push(SessionSummary::new("2026-04-01", 9, "OLDEST", "OLDESTLAST"));
        let gen = MockGenerator::default();
        let out = ActiveContextBuilder::new()
            .build_from_sessions("operator", &s, &gen, dir.path())
            .unwrap();
        assert!(out.contains("Session 1"));
        assert!(out.contains("Session 3"));
        assert!(!out.contains("Session 4"));
        assert!(!out.contains("OLDFIRST"));
        assert!(!out.contains("OLDEST"));
    }

    #[test]
    fn single_session_summarized() {
        let dir = tempdir().unwrap();
        let s = sessions(1);
        let gen = MockGenerator::returning("Discussed the homelab deploy; follow up on the weather tool.");
        let out = ActiveContextBuilder::new()
            .build_from_sessions("operator", &s, &gen, dir.path())
            .unwrap();
        assert_eq!(out, "Discussed the homelab deploy; follow up on the weather tool.");
        assert_eq!(std::fs::read_to_string(context_path(dir.path())).unwrap(), out);
    }

    #[test]
    fn very_short_session_flagged_in_prompt() {
        let dir = tempdir().unwrap();
        let s = vec![SessionSummary::new("2026-06-09", 1, "hi", "thanks")];
        let gen = MockGenerator::default(); // echo prompt
        let out = ActiveContextBuilder::new()
            .build_from_sessions("operator", &s, &gen, dir.path())
            .unwrap();
        assert!(out.contains("very short — note only briefly"));
        // Singular "turn" for a 1-turn session.
        assert!(out.contains("1 turn)"));
    }

    #[test]
    fn output_enforced_to_150_tokens() {
        let dir = tempdir().unwrap();
        let s = sessions(3);
        let huge = "lorem ipsum ".repeat(2000);
        let gen = MockGenerator::returning(huge);
        let out = ActiveContextBuilder::new()
            .build_from_sessions("operator", &s, &gen, dir.path())
            .unwrap();
        assert!(
            estimate_tokens(&out) <= CONTEXT_TOKEN_LIMIT,
            "tokens {} exceed limit",
            estimate_tokens(&out)
        );
        assert!(out.ends_with('…'));
    }

    #[test]
    fn file_written_to_correct_path() {
        let dir = tempdir().unwrap();
        let s = sessions(2);
        let gen = MockGenerator::returning("A short recap of recent themes.");
        let out = ActiveContextBuilder::new()
            .build_from_sessions("operator", &s, &gen, dir.path())
            .unwrap();
        let written = std::fs::read_to_string(context_path(dir.path())).unwrap();
        assert_eq!(written, out);
        assert_eq!(written, "A short recap of recent themes.");
    }

    #[test]
    fn failed_generation_preserves_previous_context() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        let previous = "PREVIOUS GOOD CONTEXT";
        std::fs::write(context_path(dir.path()), previous).unwrap();

        let s = sessions(3);
        let gen = FailingGenerator;
        let err = ActiveContextBuilder::new()
            .build_from_sessions("operator", &s, &gen, dir.path())
            .unwrap_err();
        assert!(matches!(err, LuminaError::Chord(_)));
        // Existing context must be untouched.
        assert_eq!(std::fs::read_to_string(context_path(dir.path())).unwrap(), previous);
    }

    #[test]
    fn empty_generation_falls_back_to_first_conversation() {
        let dir = tempdir().unwrap();
        let s = sessions(3);
        let gen = MockGenerator::returning("   \n  ");
        let out = ActiveContextBuilder::new()
            .build_from_sessions("operator", &s, &gen, dir.path())
            .unwrap();
        assert_eq!(out, FIRST_CONVERSATION);
        assert_eq!(
            std::fs::read_to_string(context_path(dir.path())).unwrap(),
            FIRST_CONVERSATION
        );
    }

    #[test]
    fn no_personal_or_infra_data_in_constants() {
        for c in [FIRST_CONVERSATION, SUMMARIZATION_SYSTEM, FAST_MODEL_HINT] {
            assert!(!c.contains("the operator"));
            assert!(!c.contains("192.168"));
            assert!(!c.contains("Foster City"));
        }
    }
}
