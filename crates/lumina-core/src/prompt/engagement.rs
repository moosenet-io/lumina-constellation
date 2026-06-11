//! DPROMPT-02: Implicit engagement-signal detection.
//!
//! After each conversation turn, Lumina observes *how* the user reacted to her
//! previous response — not what they say about her personality, but their
//! behavioural response.  [`EngagementAnalyzer::analyze_turn`] inspects the
//! user's latest message (and, for some signals, the previous Lumina response)
//! and returns the set of [`EngagementSignal`]s it detected.
//!
//! Detection is **keyword/pattern based only** — no LLM, no network, fast
//! enough to run inline on every turn (<1ms).  Each signal maps to a small
//! per-trait adjustment (see [`EngagementSignal::deltas`]); those deltas are
//! buffered and applied during nightly sleep-time consolidation by the
//! [`super::trait_tuner::TraitTuner`].

/// Per-trait adjustment produced by a single engagement signal.
///
/// Values are intentionally tiny (±0.01–0.03); a day's worth are averaged and
/// then exponentially smoothed across a week before being applied.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TraitDeltas {
    pub flair: f32,
    pub spontaneity: f32,
    pub humor: f32,
    pub focus: f32,
}

impl TraitDeltas {
    /// Component-wise sum, used when accumulating multiple signals.
    pub fn add(self, other: TraitDeltas) -> TraitDeltas {
        TraitDeltas {
            flair: self.flair + other.flair,
            spontaneity: self.spontaneity + other.spontaneity,
            humor: self.humor + other.humor,
            focus: self.focus + other.focus,
        }
    }

    /// Component-wise scale, used when averaging.
    pub fn scale(self, k: f32) -> TraitDeltas {
        TraitDeltas {
            flair: self.flair * k,
            spontaneity: self.spontaneity * k,
            humor: self.humor * k,
            focus: self.focus * k,
        }
    }
}

/// An implicit behavioural reaction inferred from a user's turn.
///
/// Multiple signals may fire for a single turn (e.g. `Laughter` +
/// `FollowUpQuestion`).  Conflicting signals are allowed and partially cancel
/// once their deltas are summed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EngagementSignal {
    /// User asked a related follow-up question → positive engagement.
    FollowUpQuestion,
    /// User changed topic within one turn → neutral-negative.
    TopicChange,
    /// User corrected Lumina ("no, I meant…", "actually…") → negative.
    Correction,
    /// User responded with >50 words → positive engagement.
    LongReply,
    /// User responded with <5 words → neutral.
    ShortReply,
    /// Explicit praise ("great", "perfect", "thanks!", positive emoji).
    ExplicitPositive,
    /// Explicit rejection ("no", "wrong", "stop").
    ExplicitNegative,
    /// User repeated the same question → negative (Lumina missed it).
    ReAsked,
    /// User laughed ("haha", "lol", "😂") → humor positive.
    Laughter,
    /// User moved on after a tool result → focus positive.
    TaskCompleted,
}

impl EngagementSignal {
    /// The signal → trait-delta mapping (exactly per the DPROMPT-02 table).
    pub fn deltas(&self) -> TraitDeltas {
        use EngagementSignal::*;
        let d = |flair, spontaneity, humor, focus| TraitDeltas { flair, spontaneity, humor, focus };
        match self {
            FollowUpQuestion => d(0.01, 0.01, 0.0, 0.0),
            TopicChange => d(-0.01, -0.01, 0.0, 0.0),
            Correction => d(0.0, -0.02, 0.0, 0.02),
            LongReply => d(0.01, 0.01, 0.01, 0.0),
            ShortReply => d(0.0, 0.0, 0.0, 0.0),
            ExplicitPositive => d(0.01, 0.01, 0.01, 0.01),
            ExplicitNegative => d(-0.01, -0.01, -0.01, 0.01),
            ReAsked => d(0.0, -0.02, -0.01, 0.03),
            Laughter => d(0.02, 0.01, 0.03, 0.0),
            TaskCompleted => d(0.0, 0.0, 0.0, 0.02),
        }
    }
}

/// Stateless, allocation-light detector for engagement signals.
pub struct EngagementAnalyzer;

impl EngagementAnalyzer {
    /// Analyse a single turn and return every detected signal.
    ///
    /// * `user_message` — the user's reply to Lumina's previous turn.
    /// * `lumina_response` — Lumina's response in *this* turn (used to detect
    ///   that a tool result / task completion preceded the user moving on).
    /// * `previous_lumina_response` — Lumina's prior turn, compared against the
    ///   user's message to detect re-asks / topic changes.  `None` on the first
    ///   turn of a session.
    pub fn analyze_turn(
        user_message: &str,
        lumina_response: &str,
        previous_lumina_response: Option<&str>,
    ) -> Vec<EngagementSignal> {
        use EngagementSignal::*;
        let mut out: Vec<EngagementSignal> = Vec::new();
        let msg = user_message.trim();
        let lower = msg.to_lowercase();
        let words = word_count(msg);

        // --- Laughter -------------------------------------------------------
        if contains_laughter(&lower) {
            out.push(Laughter);
        }

        // --- Explicit sentiment --------------------------------------------
        // Negative checked before positive so a bare "no" is not masked.
        let negative = contains_negative(&lower);
        if negative {
            out.push(ExplicitNegative);
        }
        if !negative && contains_positive(&lower) {
            out.push(ExplicitPositive);
        }

        // --- Correction -----------------------------------------------------
        if is_correction(&lower) {
            out.push(Correction);
        }

        // --- Length-based ---------------------------------------------------
        if words > 50 {
            out.push(LongReply);
        } else if words < 5 && words > 0 {
            out.push(ShortReply);
        }

        // --- Follow-up question --------------------------------------------
        // A question that builds on context (there was a prior Lumina turn) and
        // is not a verbatim re-ask.
        let is_question = msg.ends_with('?') || starts_with_interrogative(&lower);
        let re_asked = previous_lumina_response
            .map(|prev| is_reask(&lower, prev))
            .unwrap_or(false);
        if re_asked {
            out.push(ReAsked);
        } else if is_question && previous_lumina_response.is_some() {
            out.push(FollowUpQuestion);
        }

        // --- Topic change ---------------------------------------------------
        if let Some(prev) = previous_lumina_response {
            if !re_asked && is_topic_change(&lower, prev) {
                out.push(TopicChange);
            }
        }

        // --- Task completed -------------------------------------------------
        // Lumina just delivered a tool result and the user acknowledged / moved
        // on rather than pushing back.
        if looks_like_tool_result(lumina_response) && words <= 12 && !negative && !re_asked {
            out.push(TaskCompleted);
        }

        out
    }
}

// --- detection helpers ------------------------------------------------------

fn word_count(s: &str) -> usize {
    s.split_whitespace().filter(|w| w.chars().any(|c| c.is_alphanumeric())).count()
}

/// Tokenise to lowercase alphanumeric words (punctuation stripped).
fn tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect()
}

fn contains_laughter(lower: &str) -> bool {
    const LAUGHS: &[&str] = &["haha", "hehe", "lol", "lmao", "rofl", "😂", "🤣", "😆", ":d"];
    LAUGHS.iter().any(|k| lower.contains(k))
}

fn contains_positive(lower: &str) -> bool {
    const POS_EMOJI: &[&str] = &["👍", "🙏", "❤", "🎉", "🔥", "💯", "😊", "🙌"];
    if POS_EMOJI.iter().any(|e| lower.contains(e)) {
        return true;
    }
    const POS_WORDS: &[&str] = &[
        "great", "perfect", "thanks", "thank", "awesome", "nice", "love",
        "excellent", "amazing", "brilliant", "wonderful", "exactly", "good",
        "helpful", "appreciate",
    ];
    let toks = tokens(lower);
    toks.iter().any(|t| POS_WORDS.contains(&t.as_str()))
}

fn contains_negative(lower: &str) -> bool {
    const NEG_WORDS: &[&str] = &[
        "no", "wrong", "stop", "nope", "incorrect", "nonsense", "useless",
        "bad", "terrible", "annoying",
    ];
    let toks = tokens(lower);
    toks.iter().any(|t| NEG_WORDS.contains(&t.as_str()))
}

fn is_correction(lower: &str) -> bool {
    const PHRASES: &[&str] = &[
        "no, i meant", "i meant", "actually,", "actually ", "that's not",
        "thats not", "not what i", "i said", "rather,", "instead i",
        "should be", "you misunderstood", "let me rephrase", "correction",
    ];
    PHRASES.iter().any(|p| lower.contains(p))
}

fn starts_with_interrogative(lower: &str) -> bool {
    const Q: &[&str] = &[
        "what", "why", "how", "when", "where", "who", "which", "can you",
        "could you", "would you", "do you", "is there", "are there", "and what",
        "and how", "what about", "how about",
    ];
    Q.iter().any(|q| lower.starts_with(q))
}

/// True when the user's message restates the same question the previous turn
/// already addressed — measured by high token overlap with a question form.
fn is_reask(lower: &str, previous_lumina_response: &str) -> bool {
    let is_q = lower.trim_end().ends_with('?') || starts_with_interrogative(lower);
    if !is_q {
        return false;
    }
    // If the previous Lumina turn already shares most of the user's content
    // words, the user is repeating a question Lumina supposedly answered.
    let user_toks = content_tokens(lower);
    if user_toks.len() < 2 {
        return false;
    }
    let prev_toks = content_tokens(previous_lumina_response);
    let overlap = user_toks.iter().filter(|t| prev_toks.contains(*t)).count();
    (overlap as f32) / (user_toks.len() as f32) >= 0.6
}

/// True when the user's message shares almost no content with the previous
/// Lumina turn — i.e. they pivoted to a new subject.
fn is_topic_change(lower: &str, previous_lumina_response: &str) -> bool {
    let user_toks = content_tokens(lower);
    if user_toks.len() < 3 {
        // Too short to judge a topic shift reliably.
        return false;
    }
    let prev_toks = content_tokens(previous_lumina_response);
    if prev_toks.is_empty() {
        return false;
    }
    let overlap = user_toks.iter().filter(|t| prev_toks.contains(*t)).count();
    overlap == 0
}

/// Content tokens = tokens with stop-words removed (so overlap reflects topic).
fn content_tokens(s: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "the", "a", "an", "is", "are", "was", "were", "be", "to", "of", "and",
        "or", "in", "on", "at", "for", "it", "this", "that", "you", "i", "me",
        "my", "your", "we", "do", "did", "does", "can", "could", "would",
        "should", "will", "with", "what", "how", "why", "when", "where", "who",
        "which", "about", "so", "but", "if", "then", "have", "has", "had",
    ];
    tokens(s)
        .into_iter()
        .filter(|t| !STOP.contains(&t.as_str()) && t.len() > 1)
        .collect()
}

/// Heuristic: Lumina's previous response delivered a concrete tool result.
fn looks_like_tool_result(lumina_response: &str) -> bool {
    let l = lumina_response.to_lowercase();
    const MARKERS: &[&str] = &[
        "done", "completed", "created", "scheduled", "sent", "added",
        "deployed", "result:", "here's", "here is", "i've", "finished",
        "updated", "fetched", "found",
    ];
    MARKERS.iter().any(|m| l.contains(m))
}

#[cfg(test)]
mod tests {
    use super::EngagementSignal::*;
    use super::*;

    fn analyze(user: &str, prev: Option<&str>) -> Vec<EngagementSignal> {
        EngagementAnalyzer::analyze_turn(user, "ok", prev)
    }

    #[test]
    fn detects_laughter() {
        assert!(analyze("haha that's great", Some("here's a joke")).contains(&Laughter));
        assert!(analyze("lol", Some("x")).contains(&Laughter));
        assert!(analyze("😂", Some("x")).contains(&Laughter));
    }

    #[test]
    fn detects_explicit_positive() {
        assert!(analyze("perfect, thanks!", Some("done")).contains(&ExplicitPositive));
        assert!(analyze("that's exactly right 👍", Some("x")).contains(&ExplicitPositive));
    }

    #[test]
    fn detects_explicit_negative() {
        assert!(analyze("no, that's wrong", Some("x")).contains(&ExplicitNegative));
        assert!(analyze("stop", Some("x")).contains(&ExplicitNegative));
    }

    #[test]
    fn negative_suppresses_positive() {
        // "no" present → negative wins, positive not also emitted.
        let s = analyze("no thanks", Some("x"));
        assert!(s.contains(&ExplicitNegative));
        assert!(!s.contains(&ExplicitPositive));
    }

    #[test]
    fn detects_correction() {
        assert!(analyze("no, I meant the other file", Some("x")).contains(&Correction));
        assert!(analyze("actually, use postgres instead", Some("x")).contains(&Correction));
    }

    #[test]
    fn detects_long_reply() {
        let long = "word ".repeat(60);
        assert!(analyze(&long, Some("x")).contains(&LongReply));
    }

    #[test]
    fn detects_short_reply() {
        assert!(analyze("ok sure", Some("blah blah")).contains(&ShortReply));
    }

    #[test]
    fn detects_follow_up_question() {
        let s = analyze("and what about the second server?", Some("the first server is healthy"));
        assert!(s.contains(&FollowUpQuestion));
    }

    #[test]
    fn first_turn_question_is_not_follow_up() {
        let s = analyze("what is the weather?", None);
        assert!(!s.contains(&FollowUpQuestion));
    }

    #[test]
    fn detects_reask() {
        let prev = "the deployment status of the weather service is healthy";
        let s = analyze("what is the deployment status of the weather service?", Some(prev));
        assert!(s.contains(&ReAsked));
        // A re-ask is not also counted as a follow-up.
        assert!(!s.contains(&FollowUpQuestion));
    }

    #[test]
    fn detects_topic_change() {
        let prev = "the postgres database backup completed successfully";
        let s = analyze("remind me to buy groceries tomorrow", Some(prev));
        assert!(s.contains(&TopicChange));
    }

    #[test]
    fn detects_task_completed() {
        // Lumina delivered a result; user acknowledges briefly and moves on.
        let s = EngagementAnalyzer::analyze_turn("great thanks", "Done — created the issue.", Some("x"));
        assert!(s.contains(&TaskCompleted));
    }

    #[test]
    fn mapping_matches_spec_table() {
        assert_eq!(FollowUpQuestion.deltas(), TraitDeltas { flair: 0.01, spontaneity: 0.01, humor: 0.0, focus: 0.0 });
        assert_eq!(TopicChange.deltas(), TraitDeltas { flair: -0.01, spontaneity: -0.01, humor: 0.0, focus: 0.0 });
        assert_eq!(Correction.deltas(), TraitDeltas { flair: 0.0, spontaneity: -0.02, humor: 0.0, focus: 0.02 });
        assert_eq!(LongReply.deltas(), TraitDeltas { flair: 0.01, spontaneity: 0.01, humor: 0.01, focus: 0.0 });
        assert_eq!(ShortReply.deltas(), TraitDeltas::default());
        assert_eq!(ExplicitPositive.deltas(), TraitDeltas { flair: 0.01, spontaneity: 0.01, humor: 0.01, focus: 0.01 });
        assert_eq!(ExplicitNegative.deltas(), TraitDeltas { flair: -0.01, spontaneity: -0.01, humor: -0.01, focus: 0.01 });
        assert_eq!(ReAsked.deltas(), TraitDeltas { flair: 0.0, spontaneity: -0.02, humor: -0.01, focus: 0.03 });
        assert_eq!(Laughter.deltas(), TraitDeltas { flair: 0.02, spontaneity: 0.01, humor: 0.03, focus: 0.0 });
        assert_eq!(TaskCompleted.deltas(), TraitDeltas { flair: 0.0, spontaneity: 0.0, humor: 0.0, focus: 0.02 });
    }

    #[test]
    fn deltas_add_and_scale() {
        let sum = Laughter.deltas().add(ExplicitPositive.deltas());
        assert!((sum.humor - 0.04).abs() < 1e-6);
        let avg = sum.scale(0.5);
        assert!((avg.humor - 0.02).abs() < 1e-6);
    }

    #[test]
    fn conflicting_signals_partially_cancel() {
        // Correction (focus +0.02, spontaneity -0.02) + Laughter (humor +0.03)
        let s = analyze("actually that's wrong haha", Some("x"));
        assert!(s.contains(&Correction));
        assert!(s.contains(&Laughter));
    }
}
