//! DPROMPT-04: Behavioural pattern extraction from raw conversations.
//!
//! [`BehavioralAnalyzer`] computes implicit communication patterns directly
//! from raw conversation turns — *deterministically, with no LLM call* (per
//! PersonaMem-v2's implicit-behavioural-learning principle).  The resulting
//! [`BehavioralPatterns`] feed the weekly Personality Vector reconstruction
//! (see [`super::personality_vector`]): the analyzer supplies the numbers, the
//! LLM turns them into a narrative behavioural guide.
//!
//! Everything here works on plain `(user, lumina)` string pairs ([`RawTurn`])
//! so it is testable without a database.  Wave 3 wires real episodic archive
//! turns into [`BehavioralAnalyzer::extract_patterns`].

/// One raw conversation turn: the user's message and Lumina's reply.
///
/// Only the `user_msg` field drives behavioural statistics (we measure how the
/// *user* communicates); `lumina_msg` is retained for completeness and future
/// signal extraction.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RawTurn {
    pub user_msg: String,
    pub lumina_msg: String,
}

impl RawTurn {
    /// Convenience constructor.
    pub fn new(user_msg: impl Into<String>, lumina_msg: impl Into<String>) -> Self {
        RawTurn { user_msg: user_msg.into(), lumina_msg: lumina_msg.into() }
    }
}

/// Aggregate behavioural statistics computed from a set of raw turns.
///
/// All percentages are in the range `0.0..=100.0`.  Counts and averages are
/// over the *user* messages only.  `summary()` renders the stats into the
/// compact, LLM-ready block embedded in the reconstruction prompt.
#[derive(Debug, Clone, PartialEq)]
pub struct BehavioralPatterns {
    /// Number of user messages analysed.
    pub message_count: usize,
    /// Mean user message length in words.
    pub avg_message_words: f32,
    /// Share of user messages that are short (<20 words).
    pub pct_short: f32,
    /// Share of user messages that are medium (20–50 words inclusive).
    pub pct_medium: f32,
    /// Share of user messages that are long (>50 words).
    pub pct_long: f32,
    /// Share of user messages that read as questions.
    pub pct_questions: f32,
    /// Share of user messages that correct/contradict Lumina.
    pub correction_rate: f32,
    /// Heuristic topic persistence: average consecutive turns on one topic.
    pub topic_persistence: f32,
    /// Share of user messages containing at least one emoji.
    pub emoji_frequency: f32,
    /// Share of user messages requesting a tool-style action.
    pub tool_request_frequency: f32,
}

impl BehavioralPatterns {
    /// The patterns produced when there is nothing to analyse.
    pub fn empty() -> Self {
        BehavioralPatterns {
            message_count: 0,
            avg_message_words: 0.0,
            pct_short: 0.0,
            pct_medium: 0.0,
            pct_long: 0.0,
            pct_questions: 0.0,
            correction_rate: 0.0,
            topic_persistence: 0.0,
            emoji_frequency: 0.0,
            tool_request_frequency: 0.0,
        }
    }

    /// `true` when no user messages were analysed.
    pub fn is_empty(&self) -> bool {
        self.message_count == 0
    }

    /// Render the stats as the behavioural block for the reconstruction prompt.
    ///
    /// Mirrors the layout from the DPROMPT-04 spec so the LLM receives stable,
    /// well-labelled inputs.
    pub fn summary(&self) -> String {
        let bucket = if self.avg_message_words < 20.0 {
            "short"
        } else if self.avg_message_words <= 50.0 {
            "medium"
        } else {
            "long"
        };
        format!(
            "- Average message length: {avg:.0} words ({bucket}; {short:.0}% short / {medium:.0}% medium / {long:.0}% long)\n\
             - Questions: {q:.0}% of messages\n\
             - Correction rate: {corr:.0}%\n\
             - Topic persistence: {persist:.1} turns average\n\
             - Emoji usage: {emoji:.0}% of messages\n\
             - Tool requests: {tool:.0}% of messages",
            avg = self.avg_message_words,
            bucket = bucket,
            short = self.pct_short,
            medium = self.pct_medium,
            long = self.pct_long,
            q = self.pct_questions,
            corr = self.correction_rate,
            persist = self.topic_persistence,
            emoji = self.emoji_frequency,
            tool = self.tool_request_frequency,
        )
    }
}

/// Question-leading words: a message starting with one of these (or containing
/// a `?`) is treated as a question.
const QUESTION_LEADS: &[&str] = &["what", "why", "how", "when", "who", "can", "do"];

/// Phrases that indicate the user is correcting/contradicting Lumina.
const CORRECTION_MARKERS: &[&str] =
    &["no, i meant", "actually", "wrong", "not what", "that's not", "i didn't mean"];

/// Keywords indicating the user is requesting a tool-style action.
const TOOL_KEYWORDS: &[&str] =
    &["check", "look up", "lookup", "send", "search", "find", "fetch", "schedule", "remind"];

/// Deterministic extractor of behavioural patterns from raw turns.
#[derive(Debug, Default, Clone)]
pub struct BehavioralAnalyzer;

impl BehavioralAnalyzer {
    pub fn new() -> Self {
        BehavioralAnalyzer
    }

    /// Compute [`BehavioralPatterns`] from a slice of raw turns.
    ///
    /// Blank user messages are ignored.  When no analysable messages remain the
    /// result is [`BehavioralPatterns::empty`].
    pub fn extract_patterns(&self, conversations: &[RawTurn]) -> BehavioralPatterns {
        let msgs: Vec<&str> = conversations
            .iter()
            .map(|t| t.user_msg.trim())
            .filter(|m| !m.is_empty())
            .collect();

        let n = msgs.len();
        if n == 0 {
            return BehavioralPatterns::empty();
        }

        let mut total_words = 0usize;
        let mut short = 0usize;
        let mut medium = 0usize;
        let mut long = 0usize;
        let mut questions = 0usize;
        let mut corrections = 0usize;
        let mut with_emoji = 0usize;
        let mut tool_requests = 0usize;

        for m in &msgs {
            let words = m.split_whitespace().count();
            total_words += words;
            if words < 20 {
                short += 1;
            } else if words <= 50 {
                medium += 1;
            } else {
                long += 1;
            }
            if is_question(m) {
                questions += 1;
            }
            if is_correction(m) {
                corrections += 1;
            }
            if contains_emoji(m) {
                with_emoji += 1;
            }
            if is_tool_request(m) {
                tool_requests += 1;
            }
        }

        let pct = |c: usize| (c as f32 / n as f32) * 100.0;

        BehavioralPatterns {
            message_count: n,
            avg_message_words: total_words as f32 / n as f32,
            pct_short: pct(short),
            pct_medium: pct(medium),
            pct_long: pct(long),
            pct_questions: pct(questions),
            correction_rate: pct(corrections),
            topic_persistence: topic_persistence(&msgs),
            emoji_frequency: pct(with_emoji),
            tool_request_frequency: pct(tool_requests),
        }
    }
}

/// A message is a question if it contains `?` or begins with a question word.
fn is_question(msg: &str) -> bool {
    if msg.contains('?') {
        return true;
    }
    let lower = msg.to_lowercase();
    let first = lower.split_whitespace().next().unwrap_or("");
    // Strip trailing punctuation from the leading word ("how," → "how").
    let first = first.trim_matches(|c: char| !c.is_ascii_alphabetic());
    QUESTION_LEADS.contains(&first)
}

/// A message is a correction if it contains any correction marker phrase.
fn is_correction(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    CORRECTION_MARKERS.iter().any(|m| lower.contains(m))
}

/// A message requests a tool action if it contains any tool keyword.
fn is_tool_request(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    TOOL_KEYWORDS.iter().any(|k| lower.contains(k))
}

/// Detect emoji via Unicode ranges (no external dependency).
///
/// Covers the common emoji blocks: Misc Symbols & Pictographs, Emoticons,
/// Transport & Map, Supplemental Symbols, Dingbats, and regional indicators.
fn contains_emoji(msg: &str) -> bool {
    msg.chars().any(|c| {
        let u = c as u32;
        (0x1F300..=0x1FAFF).contains(&u) // pictographs / emoticons / transport / supplemental
            || (0x2600..=0x27BF).contains(&u) // misc symbols + dingbats
            || (0x1F1E6..=0x1F1FF).contains(&u) // regional indicators
            || u == 0x2764 // heavy black heart
    })
}

/// Heuristic topic persistence: average length of consecutive same-topic runs.
///
/// We approximate "topic" by the most salient content word in a message and
/// count how many consecutive messages share overlapping salient words.  This
/// is intentionally simple — the spec marks a heuristic as acceptable — and
/// returns the mean run length (≥ 1.0 whenever there is at least one message).
fn topic_persistence(msgs: &[&str]) -> f32 {
    if msgs.is_empty() {
        return 0.0;
    }
    if msgs.len() == 1 {
        return 1.0;
    }

    let salient: Vec<Vec<String>> = msgs.iter().map(|m| salient_words(m)).collect();

    let mut runs: Vec<usize> = Vec::new();
    let mut current_run = 1usize;
    for i in 1..salient.len() {
        if shares_topic(&salient[i - 1], &salient[i]) {
            current_run += 1;
        } else {
            runs.push(current_run);
            current_run = 1;
        }
    }
    runs.push(current_run);

    let total: usize = runs.iter().sum();
    total as f32 / runs.len() as f32
}

/// Two messages share a topic if their salient word sets overlap.
fn shares_topic(a: &[String], b: &[String]) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a.iter().any(|w| b.contains(w))
}

/// Extract content words (length > 3, not a common stop word), lowercased.
fn salient_words(msg: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "this", "that", "with", "from", "have", "what", "when", "your", "about",
        "would", "could", "should", "there", "their", "them", "then", "they",
        "want", "need", "just", "like", "into", "over", "more", "some", "okay",
    ];
    msg.to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| w.len() > 3 && !STOP.contains(w))
        .map(|w| w.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turns(user_msgs: &[&str]) -> Vec<RawTurn> {
        user_msgs.iter().map(|m| RawTurn::new(*m, "ok")).collect()
    }

    #[test]
    fn empty_input_yields_empty_patterns() {
        let p = BehavioralAnalyzer::new().extract_patterns(&[]);
        assert!(p.is_empty());
        assert_eq!(p.message_count, 0);
    }

    #[test]
    fn blank_user_messages_are_ignored() {
        let p = BehavioralAnalyzer::new().extract_patterns(&turns(&["   ", "\n", ""]));
        assert!(p.is_empty());
    }

    #[test]
    fn length_buckets_classified_correctly() {
        let short = "do this now"; // 3 words
        let medium = vec!["w"; 30].join(" "); // 30 words
        let long = vec!["w"; 60].join(" "); // 60 words
        let p = BehavioralAnalyzer::new()
            .extract_patterns(&turns(&[short, &medium, &long]));
        assert_eq!(p.message_count, 3);
        // 3 + 30 + 60 = 93 words / 3
        assert!((p.avg_message_words - 31.0).abs() < 0.01);
        assert!((p.pct_short - 33.333).abs() < 0.1);
        assert!((p.pct_medium - 33.333).abs() < 0.1);
        assert!((p.pct_long - 33.333).abs() < 0.1);
    }

    #[test]
    fn question_detection_marks_and_leading_words() {
        let p = BehavioralAnalyzer::new().extract_patterns(&turns(&[
            "What time is it",       // leading word, no ?
            "How does this work?",   // both
            "Tell me a story.",      // neither
            "can you reboot it",     // leading word
        ]));
        // 3 of 4 are questions
        assert!((p.pct_questions - 75.0).abs() < 0.01);
    }

    #[test]
    fn correction_rate_detected() {
        let p = BehavioralAnalyzer::new().extract_patterns(&turns(&[
            "No, I meant the other server",
            "Actually scrap that",
            "Sounds good thanks",
            "That's wrong, try again",
        ]));
        // 3 of 4 corrections
        assert!((p.correction_rate - 75.0).abs() < 0.01);
    }

    #[test]
    fn tool_request_frequency_detected() {
        let p = BehavioralAnalyzer::new().extract_patterns(&turns(&[
            "check the disk usage",
            "look up the weather",
            "send a message to the team",
            "I am feeling good today",
        ]));
        assert!((p.tool_request_frequency - 75.0).abs() < 0.01);
    }

    #[test]
    fn emoji_frequency_detected() {
        let p = BehavioralAnalyzer::new().extract_patterns(&turns(&[
            "great work 🎉",
            "lol 😂",
            "plain text here",
            "no emoji at all",
        ]));
        assert!((p.emoji_frequency - 50.0).abs() < 0.01);
    }

    #[test]
    fn topic_persistence_runs() {
        // Three deploy-topic msgs, then two weather-topic msgs.
        let msgs = turns(&[
            "deploy the rust agent today",
            "deploy looks healthy now",
            "deploy logs are clean",
            "weather forecast tomorrow morning",
            "weather alert tonight maybe",
        ]);
        let p = BehavioralAnalyzer::new().extract_patterns(&msgs);
        // Runs of 3 and 2 → mean 2.5
        assert!((p.topic_persistence - 2.5).abs() < 0.01, "got {}", p.topic_persistence);
    }

    #[test]
    fn single_message_persistence_is_one() {
        let p = BehavioralAnalyzer::new().extract_patterns(&turns(&["just one message here"]));
        assert!((p.topic_persistence - 1.0).abs() < 0.01);
    }

    #[test]
    fn summary_contains_all_labels() {
        let p = BehavioralAnalyzer::new().extract_patterns(&turns(&[
            "What is the status?",
            "check the logs please",
        ]));
        let s = p.summary();
        assert!(s.contains("Average message length"));
        assert!(s.contains("Questions:"));
        assert!(s.contains("Correction rate:"));
        assert!(s.contains("Topic persistence:"));
        assert!(s.contains("Emoji usage:"));
        assert!(s.contains("Tool requests:"));
    }
}
