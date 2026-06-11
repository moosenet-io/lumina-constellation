//! DPROMPT-14: Proactive observation engine.
//!
//! A good household member notices things and brings them up naturally —
//! "didn't you say you needed to call the dentist?".  Lumina does the same.
//! During nightly sleep-time consolidation [`ProactiveEngine::scan_for_observations`]
//! looks over the last day of activity for things worth mentioning:
//!
//! * **Action items** — the user said "I should..", "remind me to..",
//!   "I need to.." and hasn't followed up.
//! * **Patterns** — the same topic keeps coming up; a commute keeps growing;
//!   a memory was flagged stale.
//! * **Anticipatory** — an important calendar event is coming up.
//! * **Infrastructure** — a tool keeps failing; a budget threshold is near.
//!
//! Detected observations are queued in a per-user [`ObservationQueue`]
//! (`{user_id}/proactive-queue.json`, max 3, prioritised urgency > freshness >
//! relevance).  On the **first turn of the day** the top observation is handed
//! to the assembler as the `[proactive]` layer hint — a *prompt hint*, never a
//! forced response.  Lumina decides whether and how to bring it up.
//!
//! ## Design constraints
//! * **No network, no `chrono`.**  Every clock value (`now_secs`,
//!   `last_delivered_secs`) is passed in by the caller.  Scanning is pure
//!   keyword/threshold detection — *no LLM*.
//! * **Per-user, explicit path.**  The queue is loaded from / saved to an
//!   explicit [`Path`]; the caller derives it from
//!   [`crate::prompt::user_layer_dir`].
//! * Observations sourced from shared household memories can be enqueued into
//!   any user's queue by the caller — the queue itself is per-user.
//!
//! ## Integration (wired by the integration wave)
//! * `sleep_time.rs` nightly hook → [`ProactiveEngine::scan_for_observations`],
//!   then [`ObservationQueue::enqueue`] each result and [`ObservationQueue::save`].
//! * `agent_loop.rs` first turn of the day → [`ObservationQueue::peek_for_delivery`]
//!   into [`AssemblyExtras::proactive`](crate::prompt::AssemblyExtras); on a hit,
//!   call [`ObservationQueue::mark_delivered`] + save.
//! * matrix command handler → `/proactive off` ⇒ [`ObservationQueue::set_enabled`]`(false)`,
//!   `/proactive` ⇒ render [`ObservationQueue::observations`].

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Filename of the per-user proactive queue within their layer directory.
pub const QUEUE_FILENAME: &str = "proactive-queue.json";

/// Maximum observations held in the queue at once.
pub const MAX_QUEUE: usize = 3;

/// Observations older than this are dropped as stale (3 days, in seconds).
pub const STALENESS_SECS: i64 = 3 * 24 * 60 * 60;

/// One day in seconds — used for the max-1/day delivery rule and the
/// first-turn-of-day check.
pub const DAY_SECS: i64 = 24 * 60 * 60;

// ── Observation model ────────────────────────────────────────────────────

/// What kind of thing was noticed.  Ordering of urgency is encoded by
/// [`ObservationKind::base_urgency`], not by the variant order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObservationKind {
    /// User stated an intent / todo that hasn't been resolved.
    ActionItem,
    /// A recurring pattern worth surfacing.
    Pattern,
    /// Something coming up (a calendar event, a birthday).
    Anticipatory,
    /// Operational note (tool flakiness, budget threshold).
    Infrastructure,
}

impl ObservationKind {
    /// Base urgency contribution by kind.  Per spec the priority order is
    /// deadline/anticipatory ≳ action-item > pattern > infrastructure, but the
    /// concrete `urgency` field set by the scanner is what ultimately ranks
    /// observations; this just seeds a sensible default.
    pub fn base_urgency(self) -> u8 {
        match self {
            ObservationKind::Anticipatory => 80,
            ObservationKind::ActionItem => 70,
            ObservationKind::Pattern => 40,
            ObservationKind::Infrastructure => 30,
        }
    }
}

/// A single thing Lumina noticed and might mention.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    /// Human-readable description, e.g. `the operator said "I need to call the dentist"`.
    pub text: String,
    /// Category of observation.
    pub kind: ObservationKind,
    /// Unix seconds the observation was created (passed in; no clock here).
    pub created_at: i64,
    /// 0..=100 urgency. Higher wins. Defaults to `kind.base_urgency()` but the
    /// scanner bumps it for things like fast-approaching deadlines.
    pub urgency: u8,
}

impl Observation {
    /// Construct with `urgency` defaulted from the kind.
    pub fn new(text: impl Into<String>, kind: ObservationKind, created_at: i64) -> Self {
        Observation { text: text.into(), kind, created_at, urgency: kind.base_urgency() }
    }

    /// Construct with an explicit urgency (clamped to 0..=100).
    pub fn with_urgency(
        text: impl Into<String>,
        kind: ObservationKind,
        created_at: i64,
        urgency: u8,
    ) -> Self {
        Observation { text: text.into(), kind, created_at, urgency: urgency.min(100) }
    }

    /// Priority key for queue ordering: urgency, then freshness (newer wins),
    /// then a small relevance nudge for directly-stated action items.
    /// Returned as a tuple so callers can sort with `sort_by_key`.
    fn priority_key(&self) -> (u8, i64, u8) {
        let relevance = match self.kind {
            ObservationKind::ActionItem => 1, // directly mentioned by user
            _ => 0,
        };
        (self.urgency, self.created_at, relevance)
    }

    fn is_stale(&self, now_secs: i64) -> bool {
        now_secs.saturating_sub(self.created_at) > STALENESS_SECS
    }
}

// ── Scan inputs ────────────────────────────────────────────────────────────

/// An upcoming calendar event with a pre-computed countdown.  Days are passed
/// in (no clock/`chrono` here) so detection stays pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpcomingEvent {
    /// Title, e.g. `"Q3 demo"` or `"Mom's birthday"`.
    pub title: String,
    /// Whole days until the event (0 = today, 1 = tomorrow, ...).
    pub days_until: i64,
}

/// A topic the user has raised, with how many times in the trailing window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicCount {
    pub topic: String,
    pub count: u32,
}

/// A tool/operational flag noticed by monitoring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InfraFlag {
    /// Short label, e.g. `"weather tool"` or `"grocery budget"`.
    pub label: String,
    /// Human-readable note, e.g. `"has failed 4 times today"`.
    pub note: String,
}

/// A budget approaching its limit.
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetStatus {
    pub label: String,
    /// 0.0..=1.0 fraction of the budget consumed.
    pub fraction_used: f64,
}

/// Everything the nightly scanner looks at.  A plain testable struct: the
/// integration wave fills it from the conversation store, Engram, the calendar
/// cache and monitoring; tests fill it by hand.
#[derive(Debug, Clone, Default)]
pub struct ScanInputs {
    /// Recent user messages (last ~24h), most-recent last.
    pub recent_user_messages: Vec<String>,
    /// Repeated-topic counts over the trailing window.
    pub topic_counts: Vec<TopicCount>,
    /// Upcoming calendar events with day countdowns.
    pub upcoming_events: Vec<UpcomingEvent>,
    /// Operational / tool-failure flags.
    pub infra_flags: Vec<InfraFlag>,
    /// Budgets and their usage fractions.
    pub budgets: Vec<BudgetStatus>,
    /// Memories Reflexa flagged stale (rendered as conversational prompts).
    pub stale_memory_notes: Vec<String>,
}

// ── Detection thresholds ─────────────────────────────────────────────────

/// A topic raised this many times (or more) in the window is a pattern.
pub const PATTERN_TOPIC_THRESHOLD: u32 = 3;

/// Events within this many days are surfaced as anticipatory observations.
pub const EVENT_HORIZON_DAYS: i64 = 3;

/// Budget at/above this fraction is surfaced.
pub const BUDGET_ALERT_FRACTION: f64 = 0.80;

/// Action-item lead-ins (lowercased, matched as substrings).
const ACTION_PHRASES: &[&str] = &[
    "i should ",
    "i need to ",
    "i have to ",
    "i must ",
    "remind me to ",
    "i've got to ",
    "i gotta ",
    "i ought to ",
    "don't let me forget to ",
];

// ── Engine ─────────────────────────────────────────────────────────────────

/// Pure detection engine.  Holds no state; lives next to the queue for clarity.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProactiveEngine;

impl ProactiveEngine {
    pub fn new() -> Self {
        ProactiveEngine
    }

    /// Scan all inputs for observations.  Pure, deterministic, no LLM and no
    /// network: every result is derived from keyword matches and thresholds.
    pub fn scan_for_observations(&self, inputs: &ScanInputs, now_secs: i64) -> Vec<Observation> {
        let mut out = Vec::new();

        // (a) Unresolved action items from recent messages.
        for msg in &inputs.recent_user_messages {
            if let Some(item) = extract_action_item(msg) {
                out.push(Observation::with_urgency(
                    format!("The user said \"{item}\" recently and hasn't mentioned it since."),
                    ObservationKind::ActionItem,
                    now_secs,
                    ObservationKind::ActionItem.base_urgency(),
                ));
            }
        }

        // (b) Repeated-topic patterns.
        for tc in &inputs.topic_counts {
            if tc.count >= PATTERN_TOPIC_THRESHOLD {
                out.push(Observation::with_urgency(
                    format!(
                        "The user has asked about \"{}\" {} times recently — a deeper dive or a routine might help.",
                        tc.topic, tc.count
                    ),
                    ObservationKind::Pattern,
                    now_secs,
                    ObservationKind::Pattern.base_urgency(),
                ));
            }
        }

        // (b') Stale memories flagged by Reflexa — conversational re-checks.
        for note in &inputs.stale_memory_notes {
            out.push(Observation::with_urgency(
                note.clone(),
                ObservationKind::Pattern,
                now_secs,
                ObservationKind::Pattern.base_urgency(),
            ));
        }

        // (c) Anticipatory: approaching calendar events. Closer = more urgent.
        for ev in &inputs.upcoming_events {
            if ev.days_until >= 0 && ev.days_until <= EVENT_HORIZON_DAYS {
                // 0 days -> +20, 3 days -> +5.
                let bump = (EVENT_HORIZON_DAYS - ev.days_until).max(0) as u8 * 5 + 5;
                let urgency =
                    ObservationKind::Anticipatory.base_urgency().saturating_add(bump).min(100);
                let when = match ev.days_until {
                    0 => "today".to_string(),
                    1 => "tomorrow".to_string(),
                    n => format!("in {n} days"),
                };
                out.push(Observation::with_urgency(
                    format!("\"{}\" is coming up {when} — worth a heads-up or some prep.", ev.title),
                    ObservationKind::Anticipatory,
                    now_secs,
                    urgency,
                ));
            }
        }

        // (d) Infrastructure flags.
        for flag in &inputs.infra_flags {
            out.push(Observation::with_urgency(
                format!("Heads-up: the {} {}.", flag.label, flag.note),
                ObservationKind::Infrastructure,
                now_secs,
                ObservationKind::Infrastructure.base_urgency(),
            ));
        }

        // (d') Budget thresholds.
        for b in &inputs.budgets {
            if b.fraction_used >= BUDGET_ALERT_FRACTION {
                let pct = (b.fraction_used * 100.0).round() as i64;
                out.push(Observation::with_urgency(
                    format!("The {} budget is at {pct}% of its limit.", b.label),
                    ObservationKind::Infrastructure,
                    now_secs,
                    ObservationKind::Infrastructure.base_urgency().saturating_add(10),
                ));
            }
        }

        out
    }
}

/// Pull the action-item clause out of a message, if one is present.
/// Returns the matched lead-in plus the rest of the sentence (trimmed at the
/// first sentence terminator), e.g. `i need to call the dentist`.
fn extract_action_item(msg: &str) -> Option<String> {
    let lower = msg.to_lowercase();
    for phrase in ACTION_PHRASES {
        if let Some(idx) = lower.find(phrase) {
            // Work on the original-case slice from the lead-in onward.
            let tail = &msg[idx..];
            // Cut at the first sentence-ending punctuation.
            let end = tail.find(['.', '!', '?', '\n']).unwrap_or(tail.len());
            let clause = tail[..end].trim();
            if clause.len() > phrase.trim().len() {
                return Some(clause.to_string());
            }
        }
    }
    None
}

// ── Queue ────────────────────────────────────────────────────────────────

/// Per-user, persisted observation queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservationQueue {
    /// Pending observations (unordered on disk; ranked on demand).
    observations: Vec<Observation>,
    /// User toggle (`/proactive off`).  Default on.
    enabled: bool,
}

impl Default for ObservationQueue {
    fn default() -> Self {
        ObservationQueue { observations: Vec::new(), enabled: true }
    }
}

impl ObservationQueue {
    /// New empty, enabled queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Load from `path`; returns a fresh default when the file is absent or
    /// empty.  A corrupt file is a real error (so callers can log it) — but
    /// see [`ObservationQueue::load_or_default`] for a lenient variant.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) if !s.trim().is_empty() => Ok(serde_json::from_str(&s)?),
            Ok(_) => Ok(Self::new()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::new()),
            Err(e) => Err(e.into()),
        }
    }

    /// Like [`load`](Self::load) but a corrupt file yields a fresh default
    /// rather than an error — convenient for the agent loop.
    pub fn load_or_default(path: &Path) -> Self {
        Self::load(path).unwrap_or_default()
    }

    /// Persist to `path`, creating the parent directory if needed.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Read-only view of pending observations (for `/proactive`).
    pub fn observations(&self) -> &[Observation] {
        &self.observations
    }

    /// Whether proactive delivery is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Toggle proactive delivery (`/proactive off` / on).
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Number of pending observations.
    pub fn len(&self) -> usize {
        self.observations.len()
    }

    /// Whether the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.observations.is_empty()
    }

    /// Add an observation, keeping at most [`MAX_QUEUE`].  When full, the new
    /// one replaces the lowest-priority existing entry only if it outranks it;
    /// otherwise it is dropped.  De-duplicates on identical text.
    pub fn enqueue(&mut self, obs: Observation) {
        if self.observations.iter().any(|o| o.text == obs.text) {
            return;
        }
        if self.observations.len() < MAX_QUEUE {
            self.observations.push(obs);
            return;
        }
        // Full: find the current weakest entry.
        if let Some((idx, weakest)) = self
            .observations
            .iter()
            .enumerate()
            .min_by_key(|(_, o)| o.priority_key())
            .map(|(i, o)| (i, o.clone()))
        {
            if obs.priority_key() > weakest.priority_key() {
                self.observations[idx] = obs;
            }
        }
    }

    /// Drop observations older than [`STALENESS_SECS`].  Returns how many were
    /// removed.  Idempotent; safe to call before delivery and during scans.
    pub fn drop_stale(&mut self, now_secs: i64) -> usize {
        let before = self.observations.len();
        self.observations.retain(|o| !o.is_stale(now_secs));
        before - self.observations.len()
    }

    /// Highest-priority pending observation (after dropping stale), if any.
    fn top(&self) -> Option<&Observation> {
        self.observations.iter().max_by_key(|o| o.priority_key())
    }

    /// Whether `now_secs` is the first turn of a (calendar-agnostic) day
    /// relative to the last delivery: we treat "first turn of the day" as
    /// "at least [`DAY_SECS`] since we last delivered" — also satisfied when
    /// nothing has ever been delivered (`last_delivered_secs <= 0`).
    fn first_turn_of_day(now_secs: i64, last_delivered_secs: i64) -> bool {
        last_delivered_secs <= 0 || now_secs.saturating_sub(last_delivered_secs) >= DAY_SECS
    }

    /// Produce the `[proactive]` hint text to deliver this turn, or `None`.
    ///
    /// Returns `Some(hint)` only when **all** of the following hold:
    /// * proactive delivery is enabled,
    /// * the queue has a (non-stale) observation,
    /// * this is the first turn of the day (max 1/day enforced via
    ///   `last_delivered_secs`).
    ///
    /// This does **not** mutate the queue or record delivery — call
    /// [`mark_delivered`](Self::mark_delivered) once the assembler has actually
    /// consumed the hint for this turn.  Stale observations are dropped as a
    /// side effect so a stale queue never delivers.
    pub fn peek_for_delivery(
        &mut self,
        now_secs: i64,
        last_delivered_secs: i64,
    ) -> Option<String> {
        if !self.enabled {
            return None;
        }
        if !Self::first_turn_of_day(now_secs, last_delivered_secs) {
            return None;
        }
        self.drop_stale(now_secs);
        let obs = self.top()?;
        Some(format!(
            "You noticed something worth mentioning: {} \
             Bring it up naturally if there's a good moment — don't force it.",
            obs.text.trim()
        ))
    }

    /// Record that the top observation was delivered this turn: removes it from
    /// the queue so it isn't repeated.  Returns the consumed observation, if
    /// one was present.  Callers should persist the queue afterwards and store
    /// `now_secs` as the new `last_delivered_secs`.
    pub fn mark_delivered(&mut self, now_secs: i64) -> Option<Observation> {
        self.drop_stale(now_secs);
        let idx = self
            .observations
            .iter()
            .enumerate()
            .max_by_key(|(_, o)| o.priority_key())
            .map(|(i, _)| i)?;
        Some(self.observations.remove(idx))
    }

    /// Remove a queued observation the user addressed themselves (so Lumina
    /// doesn't bring up something already handled).  Matches on substring,
    /// case-insensitive.  Returns how many were removed.
    pub fn consume_matching(&mut self, needle: &str) -> usize {
        let needle = needle.to_lowercase();
        if needle.trim().is_empty() {
            return 0;
        }
        let before = self.observations.len();
        self.observations.retain(|o| !o.text.to_lowercase().contains(&needle));
        before - self.observations.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn inputs() -> ScanInputs {
        ScanInputs::default()
    }

    // ── detection ──────────────────────────────────────────────────────────

    #[test]
    fn detects_action_item_from_i_should() {
        let mut i = inputs();
        i.recent_user_messages = vec![
            "Ugh, I should call the dentist about that filling.".into(),
            "Anyway, nice weather today".into(),
        ];
        let obs = ProactiveEngine::new().scan_for_observations(&i, 1000);
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].kind, ObservationKind::ActionItem);
        assert!(obs[0].text.to_lowercase().contains("i should call the dentist"));
    }

    #[test]
    fn detects_action_item_variants() {
        for (msg, frag) in [
            ("I need to renew my passport before July", "i need to renew my passport"),
            ("remind me to water the plants", "remind me to water the plants"),
            ("I have to file taxes soon", "i have to file taxes soon"),
        ] {
            let mut i = inputs();
            i.recent_user_messages = vec![msg.into()];
            let obs = ProactiveEngine::new().scan_for_observations(&i, 0);
            assert_eq!(obs.len(), 1, "no detect for: {msg}");
            assert!(obs[0].text.to_lowercase().contains(frag), "bad clause for: {msg}");
        }
    }

    #[test]
    fn action_item_lead_in_without_body_is_ignored() {
        let mut i = inputs();
        i.recent_user_messages = vec!["I should.".into(), "Well, I need to".into()];
        // "I should." -> empty body; "I need to" -> trailing, no body after.
        let obs = ProactiveEngine::new().scan_for_observations(&i, 0);
        assert!(obs.is_empty(), "got: {obs:?}");
    }

    #[test]
    fn detects_approaching_deadline_more_urgent_when_closer() {
        let mut i = inputs();
        i.upcoming_events = vec![
            UpcomingEvent { title: "Q3 demo".into(), days_until: 0 },
            UpcomingEvent { title: "Conference".into(), days_until: 3 },
            UpcomingEvent { title: "Vacation".into(), days_until: 30 }, // beyond horizon
        ];
        let obs = ProactiveEngine::new().scan_for_observations(&i, 0);
        assert_eq!(obs.len(), 2, "30-day event should be filtered out");
        let demo = obs.iter().find(|o| o.text.contains("Q3 demo")).unwrap();
        let conf = obs.iter().find(|o| o.text.contains("Conference")).unwrap();
        assert!(demo.text.contains("today"));
        assert!(demo.urgency > conf.urgency, "closer event must be more urgent");
        assert_eq!(demo.kind, ObservationKind::Anticipatory);
    }

    #[test]
    fn detects_repeated_topic_pattern() {
        let mut i = inputs();
        i.topic_counts = vec![
            TopicCount { topic: "rust async".into(), count: 4 },
            TopicCount { topic: "weather".into(), count: 1 }, // below threshold
        ];
        let obs = ProactiveEngine::new().scan_for_observations(&i, 0);
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].kind, ObservationKind::Pattern);
        assert!(obs[0].text.contains("rust async"));
    }

    #[test]
    fn detects_budget_and_infra_flags() {
        let mut i = inputs();
        i.budgets = vec![
            BudgetStatus { label: "grocery".into(), fraction_used: 0.85 },
            BudgetStatus { label: "dining".into(), fraction_used: 0.40 }, // below
        ];
        i.infra_flags =
            vec![InfraFlag { label: "weather tool".into(), note: "has been flaky today".into() }];
        let obs = ProactiveEngine::new().scan_for_observations(&i, 0);
        assert_eq!(obs.len(), 2);
        assert!(obs.iter().any(|o| o.text.contains("grocery") && o.text.contains("85%")));
        assert!(obs.iter().any(|o| o.text.contains("weather tool")));
        assert!(obs.iter().all(|o| o.kind == ObservationKind::Infrastructure));
    }

    #[test]
    fn quiet_week_produces_nothing() {
        let obs = ProactiveEngine::new().scan_for_observations(&inputs(), 0);
        assert!(obs.is_empty());
    }

    // ── queue ────────────────────────────────────────────────────────────────

    fn act(text: &str, ts: i64, urg: u8) -> Observation {
        Observation::with_urgency(text, ObservationKind::ActionItem, ts, urg)
    }

    #[test]
    fn queue_caps_at_three_and_evicts_lowest_priority() {
        let mut q = ObservationQueue::new();
        q.enqueue(act("low", 100, 10));
        q.enqueue(act("mid", 100, 50));
        q.enqueue(act("high", 100, 90));
        assert_eq!(q.len(), 3);
        // A higher-priority arrival evicts "low".
        q.enqueue(act("highest", 100, 95));
        assert_eq!(q.len(), 3);
        let texts: Vec<&str> = q.observations().iter().map(|o| o.text.as_str()).collect();
        assert!(!texts.contains(&"low"), "lowest-priority should be evicted: {texts:?}");
        assert!(texts.contains(&"highest"));
    }

    #[test]
    fn queue_full_drops_weaker_arrival() {
        let mut q = ObservationQueue::new();
        q.enqueue(act("a", 100, 50));
        q.enqueue(act("b", 100, 60));
        q.enqueue(act("c", 100, 70));
        // Weaker than all current → dropped, queue unchanged.
        q.enqueue(act("weak", 100, 5));
        assert_eq!(q.len(), 3);
        assert!(!q.observations().iter().any(|o| o.text == "weak"));
    }

    #[test]
    fn enqueue_dedupes_identical_text() {
        let mut q = ObservationQueue::new();
        q.enqueue(act("call dentist", 100, 50));
        q.enqueue(act("call dentist", 200, 90));
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn freshness_breaks_urgency_ties_on_eviction() {
        let mut q = ObservationQueue::new();
        q.enqueue(act("old", 100, 50));
        q.enqueue(act("new", 500, 50));
        q.enqueue(act("filler", 300, 90));
        // Arrival ties urgency 50 but is newest → should evict "old".
        q.enqueue(act("newest", 900, 50));
        let texts: Vec<&str> = q.observations().iter().map(|o| o.text.as_str()).collect();
        assert!(!texts.contains(&"old"), "{texts:?}");
        assert!(texts.contains(&"newest"));
    }

    // ── delivery ─────────────────────────────────────────────────────────────

    #[test]
    fn delivers_on_first_turn_of_day() {
        let mut q = ObservationQueue::new();
        q.enqueue(act("call the dentist", 0, 70));
        // Never delivered before.
        let hint = q.peek_for_delivery(0, 0);
        assert!(hint.is_some());
        let h = hint.unwrap();
        assert!(h.starts_with("You noticed something worth mentioning:"));
        assert!(h.contains("call the dentist"));
        assert!(h.contains("don't force it"));
    }

    #[test]
    fn no_second_delivery_same_day_max_one_per_day() {
        let mut q = ObservationQueue::new();
        q.enqueue(act("x", 0, 70));
        let now = 5 * 3600; // 5h into the day
        let last = 1 * 3600; // delivered 4h ago
        assert!(q.peek_for_delivery(now, last).is_none(), "within 24h must not redeliver");
        // 24h+ later, delivery resumes.
        assert!(q.peek_for_delivery(last + DAY_SECS + 1, last).is_some());
    }

    #[test]
    fn mark_delivered_consumes_top_only() {
        let mut q = ObservationQueue::new();
        q.enqueue(act("low", 0, 10));
        q.enqueue(act("top", 0, 90));
        let consumed = q.mark_delivered(0).unwrap();
        assert_eq!(consumed.text, "top");
        assert_eq!(q.len(), 1);
        assert_eq!(q.observations()[0].text, "low");
    }

    #[test]
    fn stale_observations_dropped_after_three_days() {
        let mut q = ObservationQueue::new();
        q.enqueue(act("stale", 0, 90));
        q.enqueue(act("fresh", STALENESS_SECS, 50));
        let now = STALENESS_SECS + 10; // "stale" is now >3 days old
        let hint = q.peek_for_delivery(now, 0);
        // Stale dropped; only "fresh" remains and is delivered.
        assert_eq!(q.len(), 1);
        assert!(hint.unwrap().contains("fresh"));
        assert!(!q.observations().iter().any(|o| o.text == "stale"));
    }

    #[test]
    fn proactive_off_disables_delivery() {
        let mut q = ObservationQueue::new();
        q.enqueue(act("x", 0, 90));
        q.set_enabled(false);
        assert!(q.peek_for_delivery(0, 0).is_none());
        // Re-enabling restores delivery.
        q.set_enabled(true);
        assert!(q.peek_for_delivery(0, 0).is_some());
    }

    #[test]
    fn empty_queue_delivers_nothing() {
        let mut q = ObservationQueue::new();
        assert!(q.peek_for_delivery(0, 0).is_none());
    }

    #[test]
    fn consume_matching_removes_addressed_item() {
        let mut q = ObservationQueue::new();
        q.enqueue(act("The user said \"I need to call the dentist\"", 0, 70));
        q.enqueue(act("Conference is coming up", 0, 80));
        let removed = q.consume_matching("dentist");
        assert_eq!(removed, 1);
        assert_eq!(q.len(), 1);
    }

    // ── persistence ──────────────────────────────────────────────────────────

    #[test]
    fn round_trips_through_disk() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("operator").join(QUEUE_FILENAME);
        let mut q = ObservationQueue::new();
        q.enqueue(Observation::new("call dentist", ObservationKind::ActionItem, 42));
        q.set_enabled(false);
        q.save(&path).unwrap();

        let loaded = ObservationQueue::load(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(!loaded.is_enabled());
        assert_eq!(loaded.observations()[0].created_at, 42);
    }

    #[test]
    fn load_missing_file_is_default() {
        let dir = tempdir().unwrap();
        let q = ObservationQueue::load(&dir.path().join("nope.json")).unwrap();
        assert!(q.is_empty());
        assert!(q.is_enabled());
    }

    #[test]
    fn load_corrupt_falls_back_with_load_or_default() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("q.json");
        std::fs::write(&path, "not json{").unwrap();
        assert!(ObservationQueue::load(&path).is_err());
        let q = ObservationQueue::load_or_default(&path);
        assert!(q.is_empty() && q.is_enabled());
    }

    #[test]
    fn no_personal_data_or_ips_in_source() {
        let src = include_str!("proactive.rs");
        // Build the needle dynamically so this assertion does not itself embed
        // the forbidden literal (which would make the source scan self-trip).
        let ip_prefix = format!("{}.{}", "192", "168");
        assert!(!src.contains(&ip_prefix));
        let loc = format!("{} {}", "Foster", "City");
        assert!(!src.contains(&loc));
        let handle = format!("{}{}", "pbo", "ose");
        assert!(!src.contains(&handle));
    }
}
