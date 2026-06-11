//! EDGE-06: EventBus — fire-and-forget event system for event-triggered routines.
//!
//! Any component can emit an [`EventType`] via [`EventBus::emit`].
//! The scheduler subscribes via [`EventBus::subscribe`] and fires any routine
//! whose [`EventTrigger`] matches the incoming event.
//!
//! Events are debounced per routine key: if the same event triggers the same
//! routine within 60 seconds the duplicate is silently dropped.

use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

// ── EventType ─────────────────────────────────────────────────────────────────

/// Events that can trigger event-driven routines.
#[derive(Debug, Clone)]
pub enum EventType {
    /// Emitted once when the agent process finishes startup.
    Startup,
    /// Emitted when a tool call returns an error.
    ToolFailure { tool_name: String },
    /// Emitted when a circuit breaker transitions to the open state.
    ///
    /// `name` identifies the service/model that opened the circuit.
    CircuitOpen { name: String },
    /// Emitted when the global rate limiter rejects a request.
    RateLimitHit,
    /// Emitted when the skills engine stores a new skill document.
    SkillCreated { skill_name: String },
    /// Generic event with a name and a payload.
    ///
    /// The `payload` field is **automatically truncated to 1 024 bytes** (UTF-8
    /// code-point safe) when emitted through the [`EventBus`].  Callers that
    /// construct `Custom` events and pass them directly to `emit()` do not need
    /// to truncate beforehand.
    Custom { name: String, payload: String },
}

impl EventType {
    /// Return a stable, variant-level string name used for debounce keying and
    /// config matching.  Payload details are intentionally omitted so that, for
    /// example, any `ToolFailure` (regardless of which tool) shares a single
    /// debounce bucket per routine.
    ///
    /// Custom events are prefixed with `"custom::"` to prevent name collisions
    /// with built-in variants (e.g. a Custom event named `"ToolFailure"` yields
    /// `"custom::ToolFailure"`, not `"ToolFailure"`).
    pub fn variant_name(&self) -> String {
        match self {
            EventType::Startup => "Startup".to_string(),
            EventType::ToolFailure { .. } => "ToolFailure".to_string(),
            EventType::CircuitOpen { .. } => "CircuitOpen".to_string(),
            EventType::RateLimitHit => "RateLimitHit".to_string(),
            EventType::SkillCreated { .. } => "SkillCreated".to_string(),
            EventType::Custom { name, .. } => format!("custom::{}", name),
        }
    }
}

// ── EventTrigger ──────────────────────────────────────────────────────────────

/// The event kind that a routine wants to be triggered by.
///
/// Used in [`crate::scheduler::routine::RoutineTrigger::Event`].
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum EventTrigger {
    Startup,
    ToolFailure,
    CircuitOpen,
    RateLimitHit,
    SkillCreated,
    /// Match a `Custom` event by name.
    Custom(String),
}

/// Returns `true` if `event` satisfies `trigger`.
///
/// Matching rules:
/// - Variant-level triggers (e.g. `ToolFailure`) match any event of that
///   variant regardless of payload.
/// - `Custom(name)` matches `EventType::Custom` only when the names are equal.
pub fn matches_trigger(event: &EventType, trigger: &EventTrigger) -> bool {
    match (event, trigger) {
        (EventType::Startup, EventTrigger::Startup) => true,
        (EventType::ToolFailure { .. }, EventTrigger::ToolFailure) => true,
        (EventType::CircuitOpen { .. }, EventTrigger::CircuitOpen) => true,
        (EventType::RateLimitHit, EventTrigger::RateLimitHit) => true,
        (EventType::SkillCreated { .. }, EventTrigger::SkillCreated) => true,
        // Custom trigger matches by the raw event name (not the `custom::` prefixed
        // variant_name string, which is only used for debounce keys).
        (EventType::Custom { name, .. }, EventTrigger::Custom(trigger_name)) => {
            name == trigger_name
        }
        _ => false,
    }
}

// ── EventBus ──────────────────────────────────────────────────────────────────

/// A thin wrapper around a `tokio::sync::broadcast` channel.
///
/// Capacity is 256 events; lagged receivers silently drop missed events.
pub struct EventBus {
    sender: broadcast::Sender<EventType>,
}

impl EventBus {
    /// Create a new `EventBus` with a broadcast channel of capacity 256.
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(256);
        EventBus { sender }
    }

    /// Emit an event.  If there are no subscribers the send error is silently
    /// ignored — events are fire-and-forget.
    ///
    /// `Custom` event payloads are truncated to 1 024 bytes at the nearest
    /// UTF-8 code-point boundary before broadcasting.
    pub fn emit(&self, event: EventType) {
        let event = match event {
            EventType::Custom { name, payload } => {
                let truncated = truncate_to_bytes(payload, 1024);
                EventType::Custom { name, payload: truncated }
            }
            other => other,
        };
        let _ = self.sender.send(event);
    }

    /// Subscribe to events.  Returns a [`broadcast::Receiver`] that receives a
    /// copy of every subsequent event.
    pub fn subscribe(&self) -> broadcast::Receiver<EventType> {
        self.sender.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

// ── Debouncer ─────────────────────────────────────────────────────────────────

/// Per-routine debounce gate.
///
/// Calling [`Debouncer::should_trigger`] with a key returns `true` the first
/// time and then `false` for any call within the next 60 seconds.  After the
/// 60-second window expires the routine can fire again.
pub struct Debouncer {
    last: HashMap<String, Instant>,
    window: Duration,
}

impl Debouncer {
    /// Create a new `Debouncer` with the default 60-second window.
    pub fn new() -> Self {
        Debouncer {
            last: HashMap::new(),
            window: Duration::from_secs(60),
        }
    }

    /// Create a `Debouncer` with a custom window (useful for tests).
    pub fn with_window(window: Duration) -> Self {
        Debouncer { last: HashMap::new(), window }
    }

    /// Returns `true` and records `now` if the key has never fired or last
    /// fired more than `window` ago.  Returns `false` otherwise.
    pub fn should_trigger(&mut self, key: &str) -> bool {
        let now = Instant::now();
        match self.last.get(key) {
            Some(last) if now.duration_since(*last) < self.window => false,
            _ => {
                self.last.insert(key.to_string(), now);
                true
            }
        }
    }
}

impl Default for Debouncer {
    fn default() -> Self {
        Self::new()
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Truncate a UTF-8 string to at most `max_bytes` bytes without splitting a
/// multi-byte code point.
fn truncate_to_bytes(s: String, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s;
    }
    // Walk code-point boundaries from the end of the max window.
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // ── matches_trigger ───────────────────────────────────────────────────────

    #[test]
    fn test_startup_matches_startup() {
        assert!(matches_trigger(&EventType::Startup, &EventTrigger::Startup));
    }

    #[test]
    fn test_tool_failure_matches_tool_failure_any_tool() {
        let ev = EventType::ToolFailure { tool_name: "some_tool".to_string() };
        assert!(matches_trigger(&ev, &EventTrigger::ToolFailure));
    }

    #[test]
    fn test_circuit_open_matches_circuit_open() {
        let ev = EventType::CircuitOpen { name: "db".to_string() };
        assert!(matches_trigger(&ev, &EventTrigger::CircuitOpen));
    }

    #[test]
    fn test_rate_limit_hit_matches() {
        assert!(matches_trigger(&EventType::RateLimitHit, &EventTrigger::RateLimitHit));
    }

    #[test]
    fn test_skill_created_matches() {
        let ev = EventType::SkillCreated { skill_name: "foo".to_string() };
        assert!(matches_trigger(&ev, &EventTrigger::SkillCreated));
    }

    #[test]
    fn test_custom_matches_by_name() {
        let ev = EventType::Custom { name: "alert".to_string(), payload: "data".to_string() };
        assert!(matches_trigger(&ev, &EventTrigger::Custom("alert".to_string())));
    }

    #[test]
    fn test_custom_does_not_match_different_name() {
        let ev = EventType::Custom { name: "alert".to_string(), payload: "data".to_string() };
        assert!(!matches_trigger(&ev, &EventTrigger::Custom("other".to_string())));
    }

    #[test]
    fn test_mismatched_variants_do_not_match() {
        assert!(!matches_trigger(&EventType::Startup, &EventTrigger::ToolFailure));
        assert!(!matches_trigger(&EventType::RateLimitHit, &EventTrigger::CircuitOpen));
    }

    // ── EventBus ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_event_emission_and_subscription() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();

        bus.emit(EventType::Startup);

        let ev = rx.recv().await.unwrap();
        assert!(matches!(ev, EventType::Startup));
    }

    #[tokio::test]
    async fn test_emit_with_no_subscriber_does_not_panic() {
        let bus = EventBus::new();
        // No subscriber — should not panic.
        bus.emit(EventType::RateLimitHit);
    }

    #[tokio::test]
    async fn test_custom_payload_truncated_to_1kb() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();

        // Payload larger than 1 KB.
        let big_payload = "x".repeat(2048);
        bus.emit(EventType::Custom {
            name: "test".to_string(),
            payload: big_payload,
        });

        if let Ok(EventType::Custom { payload, .. }) = rx.recv().await {
            assert!(
                payload.len() <= 1024,
                "Payload should be truncated to <= 1024 bytes, got {}",
                payload.len()
            );
        } else {
            panic!("Expected Custom event");
        }
    }

    #[test]
    fn test_truncate_to_bytes_ascii() {
        let s = "hello world".to_string();
        assert_eq!(truncate_to_bytes(s, 5), "hello");
    }

    #[test]
    fn test_truncate_to_bytes_utf8_safe() {
        // "café" — 'é' is 2 bytes in UTF-8
        let s = "café".to_string();
        // Byte 4 is in the middle of 'é', so we must truncate to byte 3 ("caf")
        let result = truncate_to_bytes(s, 4);
        assert!(result.is_empty() || result == "caf", "got: {:?}", result);
    }

    #[test]
    fn test_truncate_to_bytes_no_change_when_under_limit() {
        let s = "short".to_string();
        let result = truncate_to_bytes(s.clone(), 1024);
        assert_eq!(result, s);
    }

    #[tokio::test]
    async fn test_multiple_subscribers_each_receive_event() {
        let bus = EventBus::new();
        let mut rx1 = bus.subscribe();
        let mut rx2 = bus.subscribe();

        bus.emit(EventType::ToolFailure { tool_name: "tool_a".to_string() });

        let ev1 = rx1.recv().await.unwrap();
        let ev2 = rx2.recv().await.unwrap();
        assert!(matches!(ev1, EventType::ToolFailure { .. }));
        assert!(matches!(ev2, EventType::ToolFailure { .. }));
    }

    #[test]
    fn test_variant_name_stable() {
        assert_eq!(EventType::Startup.variant_name(), "Startup");
        assert_eq!(
            EventType::ToolFailure { tool_name: "x".to_string() }.variant_name(),
            "ToolFailure"
        );
        // Custom events get the "custom::" prefix to avoid collisions with built-ins.
        assert_eq!(
            EventType::Custom { name: "my_event".to_string(), payload: String::new() }.variant_name(),
            "custom::my_event"
        );
    }

    #[test]
    fn test_custom_variant_name_no_collision_with_builtin() {
        // A Custom event named "ToolFailure" must NOT produce the same variant_name
        // as the real ToolFailure variant.
        let custom = EventType::Custom {
            name: "ToolFailure".to_string(),
            payload: String::new(),
        };
        let real = EventType::ToolFailure { tool_name: "t".to_string() };
        assert_ne!(
            custom.variant_name(),
            real.variant_name(),
            "Custom and built-in ToolFailure must have different variant names"
        );
    }

    // ── Debouncer ─────────────────────────────────────────────────────────────

    #[test]
    fn test_debouncer_first_trigger_allowed() {
        let mut d = Debouncer::new();
        assert!(d.should_trigger("routine_a"));
    }

    #[test]
    fn test_debouncer_second_trigger_within_window_blocked() {
        let mut d = Debouncer::new();
        assert!(d.should_trigger("routine_a"));
        // Immediately again — still within the 60-second window.
        assert!(!d.should_trigger("routine_a"));
    }

    #[test]
    fn test_debouncer_different_keys_independent() {
        let mut d = Debouncer::new();
        assert!(d.should_trigger("a"));
        assert!(d.should_trigger("b")); // different key → allowed
        assert!(!d.should_trigger("a")); // same key, within window → blocked
    }

    #[test]
    fn test_debouncer_expired_window_allows_retrigger() {
        // Use a 0-duration window so the entry immediately expires.
        let mut d = Debouncer::with_window(Duration::from_millis(0));
        assert!(d.should_trigger("key"));
        // Any subsequent call should also be allowed because the window is 0.
        assert!(d.should_trigger("key"));
    }
}
