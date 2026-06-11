//! DPROMPT-15: First-run Naming Ceremony.
//!
//! On the very first interaction with a new install, Lumina runs a short
//! conversational onboarding flow — the "Naming Ceremony" — that gives her an
//! identity relative to the user and seeds the initial prompt layers
//! (display name, trait vector, Knowledge Digest, active-context, settings).
//!
//! This module is the **state machine** that drives the flow; the question set
//! and answer→seed processing live in [`questions`].  Everything is pure and
//! filesystem-only (no network, no clock/`chrono`):
//!
//! * [`detect_first_run`] — true when the onboarding-complete marker is absent.
//! * [`NamingCeremony::start`] — open a ceremony for an admin or non-admin user.
//! * [`NamingCeremony::process_answer`] — record one answer and advance.
//! * [`NamingCeremony::complete`] — write the seeded trait vector, the initial
//!   knowledge digest, the active-context seed, and the completion marker.
//!
//! The ceremony is **resumable**: [`CeremonyState`] is serde-serialisable so the
//! agent loop can persist it between turns and resume from the next unanswered
//! question after a disconnect.  It is **skippable**: a task-first message
//! defers it with [`questions::DEFERRAL_MESSAGE`] and the user re-runs it later
//! via `/setup` (which simply starts a fresh ceremony).  It runs **once** per
//! user — once the marker exists, [`detect_first_run`] returns false.

pub mod questions;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::prompt::active_context::CONTEXT_FILENAME;
use crate::prompt::knowledge_digest::DIGEST_FILENAME;
use crate::prompt::CORE_IDENTITY;

use questions::{
    ceremony_questions, nonadmin_intro, process_answers, Answers, Question, QuestionKind, Seed,
    CEREMONY_CLOSING, DEFERRAL_MESSAGE,
};

/// Marker file written under the user's layer dir once onboarding is complete.
/// Its presence is what makes [`detect_first_run`] return `false`.
pub const ONBOARDING_MARKER: &str = "onboarding-complete";

/// Filename of the persisted trait vector (mirrors the assembler's contract).
pub const TRAIT_VECTOR_FILENAME: &str = "trait-vector.json";

/// First-run detection: a user is in first-run state when their layer directory
/// does **not** contain the onboarding-complete marker.
///
/// `user_dir` is the user's prompt-layer directory
/// (`crate::prompt::user_layer_dir(user_id)`), passed explicitly so this is
/// testable with a tempdir and has no environment dependency.
pub fn detect_first_run(user_dir: &Path) -> bool {
    !user_dir.join(ONBOARDING_MARKER).exists()
}

/// What the caller should do after a [`NamingCeremony::process_answer`] call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum CeremonyStep {
    /// Ask this question next (its `prompt` is the text to send).
    Ask(Question),
    /// All questions answered — the caller should now call
    /// [`NamingCeremony::complete`] and then send `closing` to the user.
    Done { closing: String },
}

/// Serialisable ceremony state, persisted between turns so the flow can resume.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CeremonyState {
    /// Whether this is the full admin flow or the lighter household flow.
    pub is_admin: bool,
    /// The ordered question set for this flow.
    pub questions: Vec<Question>,
    /// Index of the next question to ask (== `questions.len()` when finished).
    pub cursor: usize,
    /// Answers collected so far.
    pub answers: Answers,
}

impl CeremonyState {
    /// Whether every question has been answered.
    pub fn is_finished(&self) -> bool {
        self.cursor >= self.questions.len()
    }

    /// The next question to ask, if any.
    pub fn next_question(&self) -> Option<&Question> {
        self.questions.get(self.cursor)
    }

    /// The text to send for the next question.
    ///
    /// For the non-admin flow the first question's prompt is empty (it is the
    /// generic placeholder); use [`intro_text`](Self::intro_text) for the
    /// opening line instead.
    pub fn next_question_text(&self) -> Option<&str> {
        self.next_question().map(|q| q.prompt.as_str())
    }

    /// The opening line for this ceremony.
    ///
    /// * Admin → the full admin intro (which is also the first question).
    /// * Non-admin → [`nonadmin_intro`] with the admin's display name.
    pub fn intro_text(&self, admin_display_name: &str) -> String {
        if self.is_admin {
            // The admin intro IS the first (name) question prompt.
            self.questions
                .first()
                .map(|q| q.prompt.clone())
                .unwrap_or_default()
        } else {
            nonadmin_intro(admin_display_name)
        }
    }

    /// The message to send when the user defers the ceremony with a task.
    pub fn deferral_text(&self) -> &'static str {
        DEFERRAL_MESSAGE
    }
}

/// Drives the Naming Ceremony.  Stateless itself — state lives in
/// [`CeremonyState`] so the caller controls persistence/resume.
pub struct NamingCeremony;

impl NamingCeremony {
    /// Open a ceremony.  `is_admin` selects the 5-question admin flow or the
    /// 2-question household flow.
    pub fn start(is_admin: bool) -> CeremonyState {
        CeremonyState {
            is_admin,
            questions: ceremony_questions(is_admin),
            cursor: 0,
            answers: Answers::default(),
        }
    }

    /// Record `answer` against the current question and advance the cursor.
    ///
    /// Returns the next [`CeremonyStep`]: either the next [`Question`] to ask,
    /// or [`CeremonyStep::Done`] once the last question is answered.  Calling
    /// this on an already-finished state is a no-op that returns `Done`.
    pub fn process_answer(state: &mut CeremonyState, answer: &str) -> CeremonyStep {
        if let Some(q) = state.questions.get(state.cursor) {
            let kind = q.kind;
            state.answers.set(kind, answer);
            state.cursor += 1;
        }
        match state.next_question() {
            Some(q) => CeremonyStep::Ask(q.clone()),
            None => CeremonyStep::Done { closing: CEREMONY_CLOSING.to_string() },
        }
    }

    /// Finalise the ceremony: process the collected answers into a [`Seed`] and
    /// write the seeded artefacts under `layers_root/{user_id}`:
    ///
    /// * `trait-vector.json`     — adjusted personality vector
    /// * `knowledge-digest.txt`  — the initial one-line digest
    /// * `active-context.txt`    — the use-case seed (only when provided)
    /// * `core-identity.txt`     — the shared locked-in identity (if missing)
    /// * `onboarding-complete`   — the completion marker (records display name)
    ///
    /// `layers_root` is passed explicitly (tests use a tempdir).  Settings on
    /// the returned [`Seed`] are NOT persisted here — the agent loop applies
    /// them to the user-settings store and exports `VIGIL_COMMUTE_HOME`.
    pub fn complete(user_id: &str, answers: &Answers, layers_root: &Path) -> Result<Seed> {
        let seed = process_answers(answers);
        let user_dir = layers_root.join(sanitize_user(user_id));
        std::fs::create_dir_all(&user_dir)?;

        // Shared core identity (only write if absent — never overwrite).
        let identity = layers_root.join("core-identity.txt");
        if !identity.exists() {
            std::fs::write(&identity, CORE_IDENTITY)?;
        }

        // Seeded trait vector.
        seed.traits.save(&user_dir.join(TRAIT_VECTOR_FILENAME))?;

        // Initial knowledge digest.
        std::fs::write(user_dir.join(DIGEST_FILENAME), &seed.knowledge_digest)?;

        // Active-context seed (only when a use case was provided).
        if let Some(ctx) = &seed.active_context {
            std::fs::write(user_dir.join(CONTEXT_FILENAME), ctx)?;
        }

        // Completion marker — records the display name for audit; its mere
        // presence is what flips detect_first_run() to false.
        std::fs::write(
            user_dir.join(ONBOARDING_MARKER),
            format!("ceremony complete for {}\n", seed.display_name),
        )?;

        log::info!("naming ceremony complete for user (admin-flow seeded)");
        Ok(seed)
    }
}

/// Path to a user's onboarding-complete marker (convenience for the agent loop).
pub fn marker_path(user_dir: &Path) -> PathBuf {
    user_dir.join(ONBOARDING_MARKER)
}

/// Keep user ids filesystem-safe.  Must match `crate::prompt::sanitize_user`
/// exactly so `complete`'s output lands in the same directory that
/// `crate::prompt::user_layer_dir` and the [`PromptAssembler`] read from.
///
/// [`PromptAssembler`]: crate::prompt::PromptAssembler
fn sanitize_user(user_id: &str) -> String {
    let cleaned: String = user_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if cleaned.is_empty() { "default".to_string() } else { cleaned }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::traits::{TraitVector, INIT_FOCUS};
    use questions::{FOCUS_HEADLINES, QUIRKY_HUMOR, SETTING_COMMUTE_HOME, SETTING_LOCATION};
    use tempfile::tempdir;

    fn answer_all(state: &mut CeremonyState, answers: &[&str]) {
        for a in answers {
            NamingCeremony::process_answer(state, a);
        }
    }

    #[test]
    fn first_run_detected_when_no_marker() {
        let dir = tempdir().unwrap();
        let user_dir = dir.path().join("sam");
        std::fs::create_dir_all(&user_dir).unwrap();
        assert!(detect_first_run(&user_dir));
    }

    #[test]
    fn not_first_run_after_marker_written() {
        let dir = tempdir().unwrap();
        let user_dir = dir.path().join("sam");
        std::fs::create_dir_all(&user_dir).unwrap();
        std::fs::write(user_dir.join(ONBOARDING_MARKER), "done").unwrap();
        assert!(!detect_first_run(&user_dir));
    }

    #[test]
    fn admin_ceremony_asks_questions_in_order() {
        let mut state = NamingCeremony::start(true);
        // First question is the name question (admin intro).
        assert_eq!(state.next_question().unwrap().kind, QuestionKind::Name);

        let step = NamingCeremony::process_answer(&mut state, "Sam");
        match step {
            CeremonyStep::Ask(q) => assert_eq!(q.kind, QuestionKind::DetailLevel),
            other => panic!("expected DetailLevel, got {other:?}"),
        }
        let step = NamingCeremony::process_answer(&mut state, "headlines");
        assert!(matches!(step, CeremonyStep::Ask(q) if q.kind == QuestionKind::Personality));
        let step = NamingCeremony::process_answer(&mut state, "quirky");
        assert!(matches!(step, CeremonyStep::Ask(q) if q.kind == QuestionKind::Location));
        let step = NamingCeremony::process_answer(&mut state, "Rivertown");
        assert!(matches!(step, CeremonyStep::Ask(q) if q.kind == QuestionKind::UseCase));
        let step = NamingCeremony::process_answer(&mut state, "calendar");
        assert!(matches!(step, CeremonyStep::Done { .. }));
        assert!(state.is_finished());
    }

    #[test]
    fn nonadmin_lighter_set_two_questions() {
        let mut state = NamingCeremony::start(false);
        assert_eq!(state.questions.len(), 2);
        assert_eq!(state.next_question().unwrap().kind, QuestionKind::Name);
        let step = NamingCeremony::process_answer(&mut state, "Jo");
        assert!(matches!(step, CeremonyStep::Ask(q) if q.kind == QuestionKind::Location));
        let step = NamingCeremony::process_answer(&mut state, "Lakeside");
        assert!(matches!(step, CeremonyStep::Done { .. }));
        // Non-admin intro is generated, not the empty placeholder prompt.
        let intro = state.intro_text("Captain");
        assert!(intro.contains("Captain"));
    }

    #[test]
    fn answers_seed_correct_trait_and_settings_values() {
        let mut state = NamingCeremony::start(true);
        answer_all(&mut state, &["Sam", "just the headlines", "quirky and fun", "Rivertown", "my calendar"]);
        let seed = process_answers(&state.answers);
        assert_eq!(seed.display_name, "Sam");
        assert_eq!(seed.traits.focus, FOCUS_HEADLINES);
        assert_eq!(seed.traits.humor, QUIRKY_HUMOR);
        assert!(seed.settings.iter().any(|s| s.key == SETTING_LOCATION && s.value == "Rivertown"));
        assert!(seed.settings.iter().any(|s| s.key == SETTING_COMMUTE_HOME && s.value == "Rivertown"));
    }

    #[test]
    fn complete_writes_digest_traits_context_and_marker() {
        let dir = tempdir().unwrap();
        let mut state = NamingCeremony::start(true);
        answer_all(&mut state, &["Sam", "headlines", "quirky", "Rivertown", "home automation"]);

        let user_dir = dir.path().join("sam");
        assert!(detect_first_run(&user_dir)); // before
        NamingCeremony::complete("sam", &state.answers, dir.path()).unwrap();

        // Marker written → no longer first-run.
        assert!(!detect_first_run(&user_dir));
        // Digest seeded with ceremony data.
        let digest = std::fs::read_to_string(user_dir.join(DIGEST_FILENAME)).unwrap();
        assert!(digest.contains("Sam"));
        assert!(digest.contains("Rivertown"));
        assert!(digest.contains("home automation"));
        // Trait vector seeded (headlines → high focus).
        let tv = TraitVector::load(&user_dir.join(TRAIT_VECTOR_FILENAME));
        assert_eq!(tv.focus, FOCUS_HEADLINES);
        // Active context seeded from the use case.
        let ctx = std::fs::read_to_string(user_dir.join(CONTEXT_FILENAME)).unwrap();
        assert!(ctx.contains("home automation"));
        // Shared identity created.
        assert!(dir.path().join("core-identity.txt").exists());
    }

    #[test]
    fn not_retriggered_after_complete() {
        let dir = tempdir().unwrap();
        let mut state = NamingCeremony::start(true);
        answer_all(&mut state, &["Sam", "headlines", "quirky", "Town", "calendar"]);
        NamingCeremony::complete("sam", &state.answers, dir.path()).unwrap();
        let user_dir = dir.path().join("sam");
        // A second startup would call detect_first_run and skip the ceremony.
        assert!(!detect_first_run(&user_dir));
    }

    #[test]
    fn nonadmin_complete_without_use_case_writes_no_context() {
        let dir = tempdir().unwrap();
        let mut state = NamingCeremony::start(false);
        answer_all(&mut state, &["Jo", "Lakeside"]);
        let seed = NamingCeremony::complete("jo", &state.answers, dir.path()).unwrap();
        let user_dir = dir.path().join("jo");
        // No use case in the lighter flow → no active-context file.
        assert!(!user_dir.join(CONTEXT_FILENAME).exists());
        assert!(user_dir.join(DIGEST_FILENAME).exists());
        assert!(user_dir.join(ONBOARDING_MARKER).exists());
        // Traits untouched → locked-in default focus.
        assert_eq!(seed.traits.focus, INIT_FOCUS);
    }

    #[test]
    fn resume_mid_flow_continues_from_cursor() {
        // Answer the first two questions, then "serialise" and resume.
        let mut state = NamingCeremony::start(true);
        NamingCeremony::process_answer(&mut state, "Sam");
        NamingCeremony::process_answer(&mut state, "deep dive");

        let json = serde_json::to_string(&state).unwrap();
        let mut resumed: CeremonyState = serde_json::from_str(&json).unwrap();
        assert_eq!(resumed.cursor, 2);
        assert_eq!(resumed.next_question().unwrap().kind, QuestionKind::Personality);
        assert_eq!(resumed.answers.name.as_deref(), Some("Sam"));

        // Continue from where we left off.
        let step = NamingCeremony::process_answer(&mut resumed, "professional");
        assert!(matches!(step, CeremonyStep::Ask(q) if q.kind == QuestionKind::Location));
    }

    #[test]
    fn deferral_message_available() {
        let state = NamingCeremony::start(true);
        let msg = state.deferral_text();
        assert!(msg.to_lowercase().contains("/setup"));
    }

    #[test]
    fn process_answer_on_finished_state_is_noop() {
        let mut state = NamingCeremony::start(false);
        answer_all(&mut state, &["Jo", "Lakeside"]);
        assert!(state.is_finished());
        let before = state.answers.clone();
        let step = NamingCeremony::process_answer(&mut state, "stray input");
        assert!(matches!(step, CeremonyStep::Done { .. }));
        assert_eq!(state.answers, before); // unchanged
    }

    #[test]
    fn complete_does_not_overwrite_existing_identity() {
        let dir = tempdir().unwrap();
        let custom = "Custom identity text.";
        std::fs::write(dir.path().join("core-identity.txt"), custom).unwrap();
        let mut state = NamingCeremony::start(false);
        answer_all(&mut state, &["Jo", "Lakeside"]);
        NamingCeremony::complete("jo", &state.answers, dir.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("core-identity.txt")).unwrap(),
            custom
        );
    }

    #[test]
    fn no_personal_or_infra_data_in_marker_constant() {
        assert!(!ONBOARDING_MARKER.contains("the operator"));
        assert!(!ONBOARDING_MARKER.contains("192.168"));
    }
}
