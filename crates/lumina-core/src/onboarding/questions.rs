//! DPROMPT-15: Naming-ceremony question set and answer→seed processing.
//!
//! The ceremony is a short, conversational onboarding flow.  An **admin** user
//! gets the full five-question set (name, detail level, personality, location,
//! primary use); a **non-admin** household member gets a lighter two-question
//! set (name, location only).
//!
//! Everything here is pure and synchronous so it is fully unit-testable:
//! [`ceremony_questions`] returns the ordered question list, and
//! [`process_answers`] folds a set of free-text answers into a [`Seed`] — the
//! display name, the adjusted [`TraitVector`], the user settings, the active
//! context seed, and the values used to render the initial Knowledge Digest.
//!
//! No network, no clock/`chrono`, no hardcoded personal or infrastructure data.

use serde::{Deserialize, Serialize};

use crate::prompt::traits::TraitVector;

/// Which question (by stable id) a [`Question`] represents.  Answer processing
/// keys off this, not on positional order, so the admin/non-admin sets can
/// reuse the same processing code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionKind {
    /// "What should I call you?" → display name.
    Name,
    /// Deep dive vs. headlines → focus trait.
    DetailLevel,
    /// Professional vs. quirky → humor/flair traits.
    Personality,
    /// Where the user is based → location setting.
    Location,
    /// What they most want help with → active-context seed.
    UseCase,
}

/// A single ceremony question: its kind plus the conversational prompt text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Question {
    pub kind: QuestionKind,
    pub prompt: String,
}

impl Question {
    fn new(kind: QuestionKind, prompt: &str) -> Self {
        Question { kind, prompt: prompt.to_string() }
    }
}

/// Lumina's opening line — shown before the first question (admin flow).
pub const ADMIN_INTRO: &str = "Hey! I'm brand new here — freshly installed and \
ready to become your assistant. But first, let's make this personal. What \
should I call you?";

/// Opening line for a non-admin household member.  `{admin}` is substituted
/// with the admin's display name when known (see [`nonadmin_intro`]).
pub const NONADMIN_INTRO_TEMPLATE: &str =
    "Hi! I'm Lumina. {admin} set me up for your household. What should I call you?";

/// Closing line once all questions are answered.
pub const CEREMONY_CLOSING: &str = "Then let's do all of it. I'm Lumina, I'm \
yours, and I'm going to get better at this every single day. Let's get \
started — what can I help you with right now?";

/// Message shown when the user defers the ceremony with a task-first message.
pub const DEFERRAL_MESSAGE: &str = "I can help with that! By the way, I'm new \
here — when you have a moment, I'd love to learn a bit about you so I can be \
more useful. Just say /setup when you're ready.";

/// Render the non-admin intro, substituting the admin's display name.  When the
/// admin name is unknown/empty a neutral phrasing is used.
pub fn nonadmin_intro(admin_display_name: &str) -> String {
    let admin = admin_display_name.trim();
    if admin.is_empty() {
        "Hi! I'm Lumina. The owner set me up for your household. What should I call you?"
            .to_string()
    } else {
        NONADMIN_INTRO_TEMPLATE.replace("{admin}", admin)
    }
}

/// The ordered question set for the ceremony.
///
/// * `is_admin == true`  → 5 questions: name, detail level, personality,
///   location, use case.
/// * `is_admin == false` → 2 questions: name, location.
pub fn ceremony_questions(is_admin: bool) -> Vec<Question> {
    if is_admin {
        vec![
            Question::new(QuestionKind::Name, ADMIN_INTRO),
            Question::new(
                QuestionKind::DetailLevel,
                "Nice to meet you! Quick question — when I give you information, \
                 do you want the full deep dive, or just the highlights? (I can \
                 always go deeper if you ask.)",
            ),
            Question::new(
                QuestionKind::Personality,
                "Got it. How about my personality? Should I be more buttoned-up \
                 professional, or is a little quirky and fun okay with you?",
            ),
            Question::new(
                QuestionKind::Location,
                "Love it. Two more — where are you based? This helps me with \
                 weather, commute, and local stuff.",
            ),
            Question::new(
                QuestionKind::UseCase,
                "And last one — what's the most important thing you want me to \
                 help with? Your calendar? Your home systems? Just being someone \
                 to bounce ideas off of?",
            ),
        ]
    } else {
        vec![
            Question::new(QuestionKind::Name, ""), // intro supplied via nonadmin_intro
            Question::new(
                QuestionKind::Location,
                "Where are you based? This helps me with weather and local stuff.",
            ),
        ]
    }
}

// ── Answer interpretation ────────────────────────────────────────────────

/// Focus trait when the user wants only headlines (terse, answer-first).
pub const FOCUS_HEADLINES: f32 = 0.75;
/// Focus trait when the user wants the full deep dive.
pub const FOCUS_DEEP_DIVE: f32 = 0.45;

/// Humor/flair deltas applied on top of the defaults for a quirky preference.
pub const QUIRKY_HUMOR: f32 = 0.80;
pub const QUIRKY_FLAIR: f32 = 0.80;
/// Humor/flair values for a professional preference.
pub const PRO_HUMOR: f32 = 0.30;
pub const PRO_FLAIR: f32 = 0.35;

/// Canonical detail-level label used in the digest text.
fn detail_label(headlines: bool) -> &'static str {
    if headlines { "concise" } else { "in-depth" }
}

/// Canonical personality label used in the digest text.
fn personality_label(quirky: bool) -> &'static str {
    if quirky { "warm and playful" } else { "professional" }
}

/// Interpret a free-text detail-level answer.  Returns `true` for "headlines".
///
/// Defaults to headlines (the locked-in `focus=0.75`) when ambiguous.
fn wants_headlines(answer: &str) -> bool {
    let a = answer.to_lowercase();
    let deep = ["deep dive", "deep-dive", "deep", "full", "detail", "everything", "thorough", "in-depth", "in depth"];
    let brief = ["headline", "highlight", "brief", "short", "summary", "concise", "to the point", "tldr", "tl;dr", "quick"];
    let deep_hit = deep.iter().any(|k| a.contains(k));
    let brief_hit = brief.iter().any(|k| a.contains(k));
    // If only the deep-dive signal is present, prefer deep; otherwise headlines.
    if deep_hit && !brief_hit {
        false
    } else {
        true
    }
}

/// Interpret a free-text personality answer.  Returns `true` for "quirky".
///
/// Defaults to quirky (Lumina's locked-in personality) when ambiguous.
fn wants_quirky(answer: &str) -> bool {
    let a = answer.to_lowercase();
    let pro = ["professional", "buttoned", "formal", "serious", "business", "straight", "no-nonsense", "reserved"];
    let quirky = ["quirky", "fun", "playful", "casual", "personality", "humor", "humour", "relaxed", "friendly", "warm"];
    let pro_hit = pro.iter().any(|k| a.contains(k));
    let quirky_hit = quirky.iter().any(|k| a.contains(k));
    if pro_hit && !quirky_hit {
        false
    } else {
        true
    }
}

/// Tidy a free-text name/location answer into a stored value: trims, strips a
/// trailing period, and collapses obvious lead-ins ("call me ", "I'm ").
fn clean_value(answer: &str) -> String {
    let mut s = answer.trim().to_string();
    for lead in ["call me ", "i'm ", "im ", "i am ", "my name is ", "it's ", "its ", "name's "] {
        if s.to_lowercase().starts_with(lead) {
            s = s[lead.len()..].trim().to_string();
            break;
        }
    }
    s.trim_end_matches(['.', '!', ',']).trim().to_string()
}

/// The collected answers for a ceremony, keyed by [`QuestionKind`].
///
/// Unanswered questions are simply absent.  This is what gets passed to
/// [`process_answers`] (and persisted between turns by the state machine).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Answers {
    pub name: Option<String>,
    pub detail_level: Option<String>,
    pub personality: Option<String>,
    pub location: Option<String>,
    pub use_case: Option<String>,
}

impl Answers {
    /// Record a raw answer against a question kind.
    pub fn set(&mut self, kind: QuestionKind, value: &str) {
        let v = value.to_string();
        match kind {
            QuestionKind::Name => self.name = Some(v),
            QuestionKind::DetailLevel => self.detail_level = Some(v),
            QuestionKind::Personality => self.personality = Some(v),
            QuestionKind::Location => self.location = Some(v),
            QuestionKind::UseCase => self.use_case = Some(v),
        }
    }

    /// Whether a given question has been answered.
    pub fn has(&self, kind: QuestionKind) -> bool {
        match kind {
            QuestionKind::Name => self.name.is_some(),
            QuestionKind::DetailLevel => self.detail_level.is_some(),
            QuestionKind::Personality => self.personality.is_some(),
            QuestionKind::Location => self.location.is_some(),
            QuestionKind::UseCase => self.use_case.is_some(),
        }
    }
}

/// A single user setting derived from the ceremony (key/value).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Setting {
    pub key: String,
    pub value: String,
}

/// The seeded result of processing ceremony answers.
///
/// All fields degrade gracefully: a one-word ceremony or a deferred-then-partial
/// flow still produces a valid `Seed` (defaults fill the gaps).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Seed {
    /// User's preferred name (defaults to a neutral placeholder if unanswered).
    pub display_name: String,
    /// Adjusted personality vector (clamped to soft bounds).
    pub traits: TraitVector,
    /// Key/value settings to persist (display_name, location, …).
    pub settings: Vec<Setting>,
    /// Initial active-context seed line, when a use case was given.
    pub active_context: Option<String>,
    /// The one-line Knowledge Digest seeded from the answers.
    pub knowledge_digest: String,
}

/// Setting keys (stable identifiers, not personal data).
pub const SETTING_DISPLAY_NAME: &str = "display_name";
pub const SETTING_LOCATION: &str = "location";
pub const SETTING_COMMUTE_HOME: &str = "VIGIL_COMMUTE_HOME";
pub const SETTING_USE_CASE: &str = "primary_use_case";

/// Fold the raw answers into a [`Seed`] — pure, deterministic, testable.
///
/// Starts from the locked-in [`TraitVector::default`] and only nudges the traits
/// the user actually expressed a preference about; everything else stays at the
/// default.  The Knowledge Digest is rendered from whatever fields are present.
pub fn process_answers(answers: &Answers) -> Seed {
    let mut traits = TraitVector::default();
    let mut settings: Vec<Setting> = Vec::new();

    // Name → display_name + setting.
    let display_name = answers
        .name
        .as_deref()
        .map(clean_value)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "friend".to_string());
    settings.push(Setting { key: SETTING_DISPLAY_NAME.into(), value: display_name.clone() });

    // Detail level → focus trait.
    let headlines = answers
        .detail_level
        .as_deref()
        .map(wants_headlines)
        .unwrap_or(true); // default to headlines (matches locked-in focus 0.75)
    if answers.detail_level.is_some() {
        traits.focus = if headlines { FOCUS_HEADLINES } else { FOCUS_DEEP_DIVE };
    }

    // Personality → humor + flair.
    let quirky = answers
        .personality
        .as_deref()
        .map(wants_quirky)
        .unwrap_or(true); // default to quirky (Lumina's locked-in personality)
    if answers.personality.is_some() {
        if quirky {
            traits.humor = QUIRKY_HUMOR;
            traits.flair = QUIRKY_FLAIR;
        } else {
            traits.humor = PRO_HUMOR;
            traits.flair = PRO_FLAIR;
        }
    }

    // Location → location + commute-home settings.
    let location = answers
        .location
        .as_deref()
        .map(clean_value)
        .filter(|s| !s.is_empty());
    if let Some(loc) = &location {
        settings.push(Setting { key: SETTING_LOCATION.into(), value: loc.clone() });
        settings.push(Setting { key: SETTING_COMMUTE_HOME.into(), value: loc.clone() });
    }

    // Use case → active-context seed + setting.
    let use_case = answers
        .use_case
        .as_deref()
        .map(clean_value)
        .filter(|s| !s.is_empty());
    let active_context = use_case.as_ref().map(|uc| {
        format!("Getting started — {display_name} most wants help with: {uc}.")
    });
    if let Some(uc) = &use_case {
        settings.push(Setting { key: SETTING_USE_CASE.into(), value: uc.clone() });
    }

    let traits = traits.clamped();

    let knowledge_digest = render_digest(
        &display_name,
        location.as_deref(),
        detail_label(headlines),
        personality_label(quirky),
        use_case.as_deref(),
    );

    Seed {
        display_name,
        traits,
        settings,
        active_context,
        knowledge_digest,
    }
}

/// Render the initial one-line Knowledge Digest from the seeded values.
///
/// Mirrors the spec template; fields that weren't provided are phrased
/// gracefully rather than left as empty blanks.
pub fn render_digest(
    name: &str,
    location: Option<&str>,
    detail: &str,
    personality: &str,
    use_case: Option<&str>,
) -> String {
    let loc = match location {
        Some(l) if !l.trim().is_empty() => format!("{name} lives in {l}."),
        _ => format!("{name} hasn't shared a location yet."),
    };
    let prefs = format!(
        " They prefer {detail} responses and a {personality} style.",
    );
    let interest = match use_case {
        Some(u) if !u.trim().is_empty() => format!(" Their primary interest is {u}."),
        _ => String::new(),
    };
    format!("{loc}{prefs}{interest}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::traits::{INIT_FLAIR, INIT_FOCUS, INIT_HUMOR};

    fn full_admin_answers(name: &str, detail: &str, pers: &str, loc: &str, use_case: &str) -> Answers {
        let mut a = Answers::default();
        a.set(QuestionKind::Name, name);
        a.set(QuestionKind::DetailLevel, detail);
        a.set(QuestionKind::Personality, pers);
        a.set(QuestionKind::Location, loc);
        a.set(QuestionKind::UseCase, use_case);
        a
    }

    #[test]
    fn admin_has_five_questions_in_order() {
        let qs = ceremony_questions(true);
        let kinds: Vec<_> = qs.iter().map(|q| q.kind).collect();
        assert_eq!(
            kinds,
            vec![
                QuestionKind::Name,
                QuestionKind::DetailLevel,
                QuestionKind::Personality,
                QuestionKind::Location,
                QuestionKind::UseCase,
            ]
        );
    }

    #[test]
    fn nonadmin_has_two_questions_name_then_location() {
        let qs = ceremony_questions(false);
        let kinds: Vec<_> = qs.iter().map(|q| q.kind).collect();
        assert_eq!(kinds, vec![QuestionKind::Name, QuestionKind::Location]);
    }

    #[test]
    fn nonadmin_intro_substitutes_admin_name() {
        let admin = "Captain";
        let intro = nonadmin_intro(admin);
        assert!(intro.contains(admin));
        assert!(!intro.contains("{admin}"));
        // Empty admin falls back to neutral phrasing.
        let neutral = nonadmin_intro("  ");
        assert!(!neutral.contains("{admin}"));
        assert!(neutral.to_lowercase().contains("owner"));
    }

    #[test]
    fn headlines_answer_sets_high_focus() {
        // Build the needle dynamically — no hardcoded personal data.
        let a = full_admin_answers("Sam", "just the headlines please", "quirky and fun", "Springfield", "calendar");
        let seed = process_answers(&a);
        assert_eq!(seed.traits.focus, FOCUS_HEADLINES);
    }

    #[test]
    fn deep_dive_answer_sets_low_focus() {
        let a = full_admin_answers("Sam", "give me the full deep dive", "quirky", "Springfield", "calendar");
        let seed = process_answers(&a);
        assert_eq!(seed.traits.focus, FOCUS_DEEP_DIVE);
    }

    #[test]
    fn quirky_answer_raises_humor_and_flair() {
        let a = full_admin_answers("Sam", "headlines", "quirky and fun for sure", "Springfield", "ideas");
        let seed = process_answers(&a);
        assert_eq!(seed.traits.humor, QUIRKY_HUMOR);
        assert_eq!(seed.traits.flair, QUIRKY_FLAIR);
    }

    #[test]
    fn professional_answer_lowers_humor_and_flair() {
        let a = full_admin_answers("Sam", "headlines", "buttoned-up professional please", "Springfield", "ideas");
        let seed = process_answers(&a);
        assert_eq!(seed.traits.humor, PRO_HUMOR);
        assert_eq!(seed.traits.flair, PRO_FLAIR);
        assert!(seed.traits.humor < INIT_HUMOR);
        assert!(seed.traits.flair < INIT_FLAIR);
    }

    #[test]
    fn name_becomes_display_name_and_setting() {
        let a = full_admin_answers("call me Sam.", "headlines", "quirky", "Springfield", "ideas");
        let seed = process_answers(&a);
        assert_eq!(seed.display_name, "Sam");
        let dn = seed.settings.iter().find(|s| s.key == SETTING_DISPLAY_NAME).unwrap();
        assert_eq!(dn.value, "Sam");
    }

    #[test]
    fn location_sets_location_and_commute_home() {
        let town = "Rivertown";
        let a = full_admin_answers("Sam", "headlines", "quirky", town, "ideas");
        let seed = process_answers(&a);
        let loc = seed.settings.iter().find(|s| s.key == SETTING_LOCATION).unwrap();
        let home = seed.settings.iter().find(|s| s.key == SETTING_COMMUTE_HOME).unwrap();
        assert_eq!(loc.value, town);
        assert_eq!(home.value, town);
    }

    #[test]
    fn use_case_seeds_active_context() {
        let uc = "my calendar and home systems";
        let a = full_admin_answers("Sam", "headlines", "quirky", "Rivertown", uc);
        let seed = process_answers(&a);
        let ctx = seed.active_context.expect("active context seeded");
        assert!(ctx.contains(uc));
        assert!(ctx.contains("Sam"));
    }

    #[test]
    fn digest_includes_all_seeded_fields() {
        let name = "Sam";
        let town = "Rivertown";
        let uc = "calendar management";
        let a = full_admin_answers(name, "headlines", "quirky", town, uc);
        let seed = process_answers(&a);
        assert!(seed.knowledge_digest.contains(name));
        assert!(seed.knowledge_digest.contains(town));
        assert!(seed.knowledge_digest.contains(uc));
        assert!(seed.knowledge_digest.contains("concise"));
        assert!(seed.knowledge_digest.to_lowercase().contains("playful"));
    }

    #[test]
    fn nonadmin_partial_answers_still_seed() {
        // Only name + location (the lighter set), one-word answers.
        let mut a = Answers::default();
        a.set(QuestionKind::Name, "Jo");
        a.set(QuestionKind::Location, "Lakeside");
        let seed = process_answers(&a);
        assert_eq!(seed.display_name, "Jo");
        // Traits untouched → locked-in defaults (no detail/personality answer).
        assert_eq!(seed.traits.focus, INIT_FOCUS);
        assert_eq!(seed.traits.humor, INIT_HUMOR);
        // No use case → no active context.
        assert!(seed.active_context.is_none());
        assert!(seed.knowledge_digest.contains("Lakeside"));
    }

    #[test]
    fn empty_answers_produce_graceful_defaults() {
        let seed = process_answers(&Answers::default());
        assert_eq!(seed.display_name, "friend");
        assert_eq!(seed.traits, TraitVector::default());
        assert!(seed.active_context.is_none());
        // Digest still renders without panicking and notes the missing location.
        assert!(seed.knowledge_digest.to_lowercase().contains("hasn't shared a location"));
    }

    #[test]
    fn traits_are_clamped() {
        // All seeded trait values are within the soft bounds.
        let a = full_admin_answers("Sam", "deep dive", "professional", "Town", "stuff");
        let seed = process_answers(&a);
        let c = seed.traits.clamped();
        assert_eq!(seed.traits, c);
    }

    #[test]
    fn no_personal_or_infra_data_in_constants() {
        for c in [ADMIN_INTRO, NONADMIN_INTRO_TEMPLATE, CEREMONY_CLOSING, DEFERRAL_MESSAGE] {
            assert!(!c.contains("the operator"));
            assert!(!c.contains("Operator"));
            assert!(!c.contains("192.168"));
            assert!(!c.contains("Foster City"));
        }
    }
}
