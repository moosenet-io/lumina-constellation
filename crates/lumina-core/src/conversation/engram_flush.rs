//! CONV-04: flush a closed conversation session to Engram as a raw episodic
//! record (Tier-1 working memory → Tier-2 episodic storage).
//!
//! Design (per S78 spec):
//! - On session close (inactivity sweep or explicit "new conversation"), the
//!   session's raw turn-pairs are rendered into a single Episodic memory and
//!   stored verbatim — NOT summarised (summary is a sleep-time concern).
//! - Stored without an embedding: raw archival, consistent with the S75
//!   principle "reconstruct from raw archive during sleep-time". Avoids an
//!   Ollama round-trip on the flush path. (Sleep-time consolidation can embed.)
//! - Persistence failures are retained in a bounded retry queue and re-attempted
//!   on the next cleanup cycle, so no conversation is lost on a transient error.
//! - Content is sanitised (S6 `filter_output`) before storage.

use crate::conversation::buffer::SessionState;
use crate::engram::types::{Memory, MemoryType, SensitivityCategory};
use crate::error::Result;
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

/// Max number of failed flushes retained for retry before the oldest is dropped.
const MAX_PENDING_FLUSHES: usize = 100;

/// A ready-to-persist conversation episode (rendered + sanitised).
#[derive(Debug, Clone, PartialEq)]
pub struct Episode {
    pub user_id: String,
    pub session_id: String,
    pub session_start: i64,
    pub session_end: i64,
    pub turn_count: usize,
    /// Sanitised, human-readable transcript (the memory content).
    pub content: String,
}

impl Episode {
    /// Build an episode from a closed session's raw turns. Pure: renders the
    /// transcript, sanitises it (S6), and captures metadata. Returns `None` for
    /// an empty session (nothing to flush).
    pub fn from_session(user_id: &str, session: &SessionState) -> Option<Episode> {
        if session.entries.is_empty() && session.summaries.is_empty() {
            return None;
        }
        let start = session
            .entries
            .front()
            .map(|e| e.timestamp)
            .unwrap_or(session.last_activity);
        let end = session
            .entries
            .back()
            .map(|e| e.timestamp)
            .unwrap_or(session.last_activity);
        // turn_count includes turns already compressed into summaries (CONV-05).
        let summarized: usize = session.summaries.iter().map(|s| s.turns_covered).sum();
        let turn_count = session.entries.len() + summarized;

        let mut body = String::new();
        // CONV-05: prepend any summary blocks (compressed older turns) before the
        // verbatim recent turns, so the episode preserves the whole session.
        for block in &session.summaries {
            let s = crate::security::output_filter::filter_output(&block.text);
            body.push_str("[Earlier summary] ");
            body.push_str(s.as_str());
            body.push('\n');
        }
        for entry in &session.entries {
            // Sanitise each side (S6) — scrub any secrets that slipped into text.
            let u = crate::security::output_filter::filter_output(&entry.user_message);
            let a = crate::security::output_filter::filter_output(&entry.assistant_response);
            body.push_str("User: ");
            body.push_str(u.as_str());
            body.push('\n');
            body.push_str("Lumina: ");
            body.push_str(a.as_str());
            body.push('\n');
        }
        let content = format!(
            "Conversation session ({turn_count} turn{}):\n{body}",
            if turn_count == 1 { "" } else { "s" }
        );

        Some(Episode {
            user_id: user_id.to_string(),
            session_id: session.session_id.clone(),
            session_start: start,
            session_end: end,
            turn_count,
            content,
        })
    }

    /// Convert to an Episodic [`Memory`] for storage. Pure.
    pub fn to_memory(&self) -> Memory {
        let mut mem = Memory::new(
            &self.user_id,
            MemoryType::Episodic,
            SensitivityCategory::General,
            &self.content,
        );
        mem.source_conversation_id = Some(self.session_id.clone());
        mem.confidence = 1.0; // raw record, not an inference
        mem.tags = vec![
            "conversation".to_string(),
            "session-flush".to_string(),
            format!("turns:{}", self.turn_count),
            format!("session_start:{}", self.session_start),
            format!("session_end:{}", self.session_end),
        ];
        mem
    }
}

// ── Pending-flush retry queue ───────────────────────────────────────────────

/// Bounded FIFO of episodes whose persistence failed, awaiting retry.
#[derive(Default)]
pub struct PendingFlushes {
    queue: VecDeque<Episode>,
    /// Count of episodes dropped because the queue was full (for diagnostics).
    pub dropped: usize,
}

impl PendingFlushes {
    pub fn new() -> Self {
        Self { queue: VecDeque::new(), dropped: 0 }
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Enqueue a failed episode, dropping the oldest if at capacity.
    pub fn enqueue(&mut self, ep: Episode) {
        if self.queue.len() >= MAX_PENDING_FLUSHES {
            self.queue.pop_front();
            self.dropped += 1;
            eprintln!(
                "conversation/flush: pending queue full ({MAX_PENDING_FLUSHES}); dropped oldest episode"
            );
        }
        self.queue.push_back(ep);
    }

    /// Attempt to persist every queued episode via `persist`. Episodes that fail
    /// again are retained (re-queued) in their original order.
    pub fn retry_all<F>(&mut self, mut persist: F)
    where
        F: FnMut(&Episode) -> Result<()>,
    {
        let mut still_pending = VecDeque::new();
        while let Some(ep) = self.queue.pop_front() {
            if persist(&ep).is_err() {
                still_pending.push_back(ep);
            }
        }
        self.queue = still_pending;
    }
}

/// Process-global pending-flush queue.
static PENDING: OnceLock<Mutex<PendingFlushes>> = OnceLock::new();

fn pending() -> &'static Mutex<PendingFlushes> {
    PENDING.get_or_init(|| Mutex::new(PendingFlushes::new()))
}

/// Persist a single episode to the per-user Engram store (real IO). Synchronous;
/// stores without an embedding (raw archival). No `Config` needed — uses the
/// vault key + per-user path via `EngramStore::open_for_user`.
pub fn persist_episode(ep: &Episode) -> Result<()> {
    let store = crate::engram::EngramStore::open_for_user(&ep.user_id)?;
    store.insert_memory(&ep.to_memory())
}

/// Flush a closed session now. On persistence failure, enqueue for retry.
/// `user_id` must already be storage-safe (sanitised at the channel boundary).
pub fn flush_session(user_id: &str, session: &SessionState) {
    let Some(ep) = Episode::from_session(user_id, session) else {
        return; // empty session — nothing to flush
    };
    match persist_episode(&ep) {
        Ok(()) => {
            eprintln!(
                "conversation: session_flushed user={} session={} turns={}",
                ep.user_id, ep.session_id, ep.turn_count
            );
        }
        Err(e) => {
            eprintln!(
                "conversation: session flush failed (queued for retry) user={} session={}: {e}",
                ep.user_id, ep.session_id
            );
            pending().lock().map(|mut q| q.enqueue(ep)).ok();
        }
    }
}

/// Retry any episodes whose earlier flush failed. Call on each cleanup cycle.
pub fn retry_pending() {
    if let Ok(mut q) = pending().lock() {
        if q.is_empty() {
            return;
        }
        let before = q.len();
        q.retry_all(|ep| persist_episode(ep));
        let after = q.len();
        if before != after {
            eprintln!("conversation: retried pending flushes, {} -> {} remaining", before, after);
        }
    }
}

/// Test-only: number of episodes currently awaiting retry.
#[cfg(test)]
pub fn pending_len() -> usize {
    pending().lock().map(|q| q.len()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::buffer::ConversationBuffer;
    use std::cell::Cell;

    const T: i64 = 1_700_000_000;

    fn session_with(turns: &[(&str, &str)]) -> SessionState {
        let mut b = ConversationBuffer::new(100, 1_000_000, 1800);
        for (u, a) in turns {
            b.push("u-test", *u, *a, T);
        }
        b.close_session("u-test").expect("session exists")
    }

    #[test]
    fn episode_from_session_renders_transcript_and_metadata() {
        let s = session_with(&[("hi there", "hello!"), ("how are you", "great")]);
        let ep = Episode::from_session("alice", &s).expect("non-empty");
        assert_eq!(ep.user_id, "alice");
        assert_eq!(ep.turn_count, 2);
        assert_eq!(ep.session_start, T);
        assert_eq!(ep.session_end, T);
        assert!(!ep.session_id.is_empty());
        assert!(ep.content.contains("User: hi there"));
        assert!(ep.content.contains("Lumina: hello!"));
        assert!(ep.content.contains("how are you"));
        assert!(ep.content.starts_with("Conversation session (2 turns):"));
    }

    #[test]
    fn empty_session_yields_no_episode() {
        let s = ConversationBuffer::new(10, 1000, 1800)
            .close_session("nobody")
            .unwrap_or(SessionState {
                entries: Default::default(),
                summaries: Default::default(),
                last_activity: T,
                total_tokens: 0,
                session_id: "empty".into(),
            });
        assert!(Episode::from_session("x", &s).is_none());
    }

    #[test]
    fn to_memory_is_episodic_with_provenance_and_tags() {
        let s = session_with(&[("a", "b")]);
        let ep = Episode::from_session("bob", &s).unwrap();
        let mem = ep.to_memory();
        assert!(matches!(mem.memory_type, MemoryType::Episodic));
        assert_eq!(mem.source_conversation_id.as_deref(), Some(ep.session_id.as_str()));
        assert!(mem.tags.iter().any(|t| t == "conversation"));
        assert!(mem.tags.iter().any(|t| t == "turns:1"));
        assert_eq!(mem.user_id, "bob");
    }

    #[test]
    fn single_turn_uses_singular_label() {
        let s = session_with(&[("only", "one")]);
        let ep = Episode::from_session("u", &s).unwrap();
        assert!(ep.content.starts_with("Conversation session (1 turn):"));
    }

    #[test]
    fn retry_queue_retains_failures_then_drains_on_success() {
        let mut q = PendingFlushes::new();
        let s = session_with(&[("x", "y")]);
        let ep = Episode::from_session("u", &s).unwrap();
        q.enqueue(ep);
        assert_eq!(q.len(), 1);

        // First retry: persist fails → still pending.
        q.retry_all(|_| Err(crate::error::LuminaError::Config("engram down".into())));
        assert_eq!(q.len(), 1, "failed flush retained");

        // Second retry: persist succeeds → drained.
        let calls = Cell::new(0);
        q.retry_all(|_| {
            calls.set(calls.get() + 1);
            Ok(())
        });
        assert_eq!(calls.get(), 1);
        assert_eq!(q.len(), 0, "successful flush removed from queue");
    }

    #[test]
    fn retry_queue_is_bounded_and_drops_oldest() {
        let mut q = PendingFlushes::new();
        let s = session_with(&[("x", "y")]);
        for _ in 0..(MAX_PENDING_FLUSHES + 5) {
            q.enqueue(Episode::from_session("u", &s).unwrap());
        }
        assert_eq!(q.len(), MAX_PENDING_FLUSHES, "queue capped");
        assert_eq!(q.dropped, 5, "five oldest dropped");
    }
}
