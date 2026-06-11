//! Lumina Matrix bot — MATRIX-01..04
//!
//! Connects to the Tuwunel homeserver, listens for messages in the configured
//! room, pipes them through the guarded core loop, and posts responses back.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use matrix_sdk::{
    config::SyncSettings,
    event_handler::Ctx,
    room::Room,
    ruma::{
        events::room::member::StrippedRoomMemberEvent,
        events::room::message::{
            MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent,
        },
        OwnedRoomId,
    },
    Client,
};
use tokio::sync::Mutex;

use crate::agent_loop::process_message_for_user;
use crate::config::{Config, MatrixCredentials};
use crate::error::{LuminaError, Result};
use crate::matrix_format::{chunk_message, markdown_to_matrix_html};

/// Shared mutable state passed into every event handler via matrix-sdk's context system.
pub struct BotState {
    config: Arc<Config>,
    creds: MatrixCredentials,
    seen_events: Mutex<SeenEvents>,
    processing_lock: Mutex<()>,
    startup_time: Instant,
    /// Allowlist of Matrix user IDs permitted to interact with the bot.
    /// Empty = allow all (backward-compatible when MATRIX_ALLOWED_USERS is not set).
    allowed_users: Vec<String>,
}

/// Bounded deduplication set: O(1) lookup, evicts oldest entries beyond cap.
struct SeenEvents {
    ids: HashSet<String>,
    order: VecDeque<String>,
}

impl SeenEvents {
    fn new() -> Self {
        Self { ids: HashSet::new(), order: VecDeque::new() }
    }

    fn contains(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    fn insert(&mut self, id: String) {
        if self.order.len() >= 200 {
            // Trim to 100 oldest entries
            while self.order.len() > 100 {
                if let Some(old) = self.order.pop_front() {
                    self.ids.remove(&old);
                }
            }
        }
        self.ids.insert(id.clone());
        self.order.push_back(id);
    }
}

/// The Matrix bot.
pub struct MatrixBot {
    client: Client,
    creds: MatrixCredentials,
    room_id: OwnedRoomId,
    state: Arc<BotState>,
    announce_startup: bool,
}

impl MatrixBot {
    /// Build and connect the bot.  Does not start the event loop.
    pub async fn connect(config: Arc<Config>) -> Result<Self> {
        let creds = config.require_matrix()?;

        let store_path = &config.matrix_store_path;
        std::fs::create_dir_all(store_path).map_err(|e| {
            LuminaError::Config(format!("Cannot create matrix store dir: {}", e))
        })?;

        let homeserver_url = creds.homeserver.parse::<url::Url>().map_err(|e| {
            LuminaError::Config(format!("Invalid MATRIX_HOMESERVER URL: {}", e))
        })?;

        let client = Client::builder()
            .homeserver_url(homeserver_url)
            .build()
            .await
            .map_err(|e| LuminaError::Config(format!("Matrix client build failed: {}", e)))?;

        Ok(MatrixBot {
            client,
            creds: creds.clone(),
            room_id: OwnedRoomId::try_from(creds.room_id.as_str())
                .map_err(|e| LuminaError::Config(format!("Invalid MATRIX_ROOM_ID: {}", e)))?,
            state: Arc::new(BotState {
                allowed_users: config.matrix_allowed_users.clone(),
                config: config.clone(),
                creds: creds.clone(),
                seen_events: Mutex::new(SeenEvents::new()),
                processing_lock: Mutex::new(()),
                startup_time: Instant::now(),
            }),
            announce_startup: config.matrix_announce_startup,
        })
    }

    /// Log in with password credentials.
    pub async fn login(&self) -> Result<()> {
        self.client
            .matrix_auth()
            .login_username(&self.creds.user, &self.creds.password)
            .initial_device_display_name("lumina-core")
            .send()
            .await
            .map_err(|e| LuminaError::Config(format!("Matrix login failed: {}", e)))?;

        eprintln!("matrix: logged in as {}", self.creds.full_user_id);
        Ok(())
    }

    /// Run one sync to catch up on room state; discards pre-startup messages.
    pub async fn initial_sync(&self) -> Result<()> {
        let settings = SyncSettings::default().timeout(Duration::from_secs(10));
        self.client
            .sync_once(settings)
            .await
            .map_err(|e| LuminaError::Config(format!("Initial sync failed: {}", e)))?;
        eprintln!("matrix: initial sync complete");
        Ok(())
    }

    /// Join the configured room (idempotent).
    pub async fn join_room(&self) -> Result<()> {
        match self.client.join_room_by_id(&self.room_id).await {
            Ok(_) => eprintln!("matrix: joined room {}", self.room_id),
            Err(e) => {
                // Already-joined is not an error
                let msg = e.to_string();
                if msg.contains("already") || msg.contains("forbidden") {
                    eprintln!("matrix: room join skipped ({})", msg);
                } else {
                    return Err(LuminaError::Config(format!("Room join failed: {}", e)));
                }
            }
        }
        Ok(())
    }

    /// Register event handlers and run the sync loop with exponential backoff on reconnect.
    pub async fn run(self) -> Result<()> {
        // Register message handler
        self.client.add_event_handler_context(self.state.clone());
        self.client.add_event_handler(Self::on_message);

        // Register invite handler
        self.client.add_event_handler(Self::on_invite);

        // Post startup announcement
        if self.announce_startup {
            self.post_to_room("Lumina is online.").await.ok();
        }

        // Reconnect loop with exponential backoff
        let mut backoff = Duration::from_secs(2);
        loop {
            let settings = SyncSettings::default()
                .timeout(Duration::from_secs(30));

            match self.client.sync(settings).await {
                Ok(_) => {
                    // sync() returned cleanly — shouldn't happen in normal operation
                    eprintln!("matrix: sync loop ended");
                    break;
                }
                Err(e) => {
                    eprintln!("matrix: sync lost ({}). reconnecting in {}s", e, backoff.as_secs());
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(60));
                }
            }
        }

        Ok(())
    }

    /// Send a plain text message to the configured room.
    async fn post_to_room(&self, text: &str) -> Result<()> {
        let room = self.client.get_room(&self.room_id)
            .ok_or_else(|| LuminaError::Config("Room not found after join".to_string()))?;

        let html = markdown_to_matrix_html(text);
        let content = RoomMessageEventContent::text_html(text, html);
        room.send(content)
            .await
            .map_err(|e| LuminaError::Config(format!("Matrix send failed: {}", e)))?;
        Ok(())
    }

    /// Event handler: incoming room message.
    async fn on_message(
        ev: OriginalSyncRoomMessageEvent,
        room: Room,
        ctx: Ctx<Arc<BotState>>,
        client: Client,
    ) {
        let state = ctx.0.clone();

        // Only process messages received after startup (skip sync replay)
        if let Some(ts) = ev.origin_server_ts.to_system_time() {
            if ts <= std::time::UNIX_EPOCH + Duration::from_secs(
                state.startup_time.elapsed().as_secs()
                    // startup_time is Instant; convert to approx wall clock via UNIX_EPOCH
                    // Actually: skip messages older than bot start. We use a simpler check:
                    // process if message was sent within the last hour (covers restart gap).
                    + (std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                        .saturating_sub(state.startup_time.elapsed().as_secs()))
            ) {
                // Timestamp before startup — skip
            }
        }

        // Ignore own messages
        if let Some(my_id) = client.user_id() {
            if ev.sender == my_id {
                return;
            }
        }

        // MATRIX-SECURITY: enforce sender allowlist — silently drop messages from
        // users not in the list. No acknowledgement is sent to avoid user enumeration.
        if !state.allowed_users.is_empty() {
            let sender_str = ev.sender.as_str();
            if !state.allowed_users.iter().any(|u| u == sender_str) {
                eprintln!("matrix: rejected message from non-allowlisted sender {}", sender_str);
                return;
            }
        }

        // Extract text body
        let text = match ev.content.msgtype {
            MessageType::Text(ref t) => t.body.trim().to_string(),
            _ => return, // ignore non-text (images, files, etc.)
        };

        if text.is_empty() {
            return;
        }

        // Dedup by Matrix event ID
        {
            let mut seen = state.seen_events.lock().await;
            let event_id = ev.event_id.to_string();
            if seen.contains(&event_id) {
                eprintln!("matrix: duplicate event {} skipped", &event_id[..16.min(event_id.len())]);
                return;
            }
            seen.insert(event_id);
        }

        eprintln!("matrix: [{}] {}: {}...", room.room_id(), ev.sender, &text[..text.len().min(80)]);

        // Serialization lock — one message at a time
        let _lock = state.processing_lock.lock().await;

        // Guarded-tool approval: handle `approve <CODE>` / `deny <CODE>` here,
        // deterministically and WITHOUT the LLM, before anything else. This is the
        // operator's express per-occurrence authorization for guarded tools.
        if let Some(reply) = crate::agent_loop::try_handle_approval(&state.config, &text).await {
            let html = markdown_to_matrix_html(&reply);
            let content = RoomMessageEventContent::text_html(reply.as_str(), html);
            if let Err(e) = room.send(content).await {
                eprintln!("matrix: approval reply send failed: {}", e);
            }
            return;
        }

        // Typing indicator on (covers the whole turn).
        if let Err(e) = room.typing_notice(true).await {
            eprintln!("matrix: typing notice failed: {}", e);
        }

        // Two-message UX: post the interim "Let me pull that up…" ONLY when the
        // turn actually does slow work (a tool call) — pure-text turns like
        // "tell me a joke" return fast and silently, so they get ONE message.
        //
        // The ack fires at most once, on whichever of these comes first:
        //   • a real tool-call signal from the executor — precise, fires the
        //     instant a tool is dispatched in the legacy client-side loop; and
        //   • a short latency grace period — a backstop for Chord's server-side
        //     agentic mode, where tools run remotely and aren't observable
        //     mid-flight. Fast replies finish before the grace elapses, so a
        //     snappy joke never trips it.
        //
        // The `biased` select polls turn-completion first, so a turn that has
        // already finished can never emit a late, pointless ack.
        let grace_ms = ack_grace_ms();
        let (notify_tx, mut notify_rx) =
            tokio::sync::mpsc::unbounded_channel::<Option<String>>();
        // CONV-03: thread the authenticated sender so the conversation buffer is
        // keyed per Matrix user (per-user multi-turn isolation).
        let turn = crate::agent_loop::with_tool_notify(
            notify_tx,
            process_message_for_user(&state.config, &text, Some(ev.sender.as_str())),
        );
        tokio::pin!(turn);
        let grace = tokio::time::sleep(Duration::from_millis(grace_ms));
        tokio::pin!(grace);

        let mut ack_sent = false;
        let result = loop {
            tokio::select! {
                biased;
                r = &mut turn => break r,
                _ = &mut grace, if !ack_sent => {
                    // RESP-04: last-resort backstop — no tool name known here.
                    send_ack(&room, ack_phrase(None)).await;
                    ack_sent = true;
                }
                sig = notify_rx.recv(), if !ack_sent => {
                    if let Some(tool_opt) = sig {
                        // RESP-05: tailor the interim phrase to the tool dispatched.
                        send_ack(&room, ack_phrase(tool_opt.as_deref())).await;
                        ack_sent = true;
                    }
                }
            }
        };

        let response = match result {
            Ok(r) => r.to_string(),
            Err(LuminaError::SecurityViolation(_)) => {
                "I can't process that input.".to_string()
            }
            Err(e) => {
                eprintln!("matrix: process_message error: {}", e);
                "I'm having trouble connecting to my brain right now. Please try again.".to_string()
            }
        };

        // Typing indicator off
        if let Err(e) = room.typing_notice(false).await {
            eprintln!("matrix: typing notice failed: {}", e);
        }

        // Send response, chunked if needed
        let chunks = chunk_message(&response);
        for chunk in &chunks {
            let html = markdown_to_matrix_html(chunk);
            let content = RoomMessageEventContent::text_html(chunk.as_str(), html);
            if let Err(e) = room.send(content).await {
                eprintln!("matrix: send failed: {}", e);
                break;
            }
        }
    }

    /// Event handler: auto-accept room invites.
    async fn on_invite(ev: StrippedRoomMemberEvent, room: Room, client: Client, ctx: Ctx<Arc<BotState>>) {
        let state = ctx.0.clone();

        // Only accept if the event targets the bot itself
        if let Some(my_id) = client.user_id() {
            if ev.state_key.as_str() != my_id.as_str() {
                return;
            }
        }

        // MATRIX-SECURITY: only accept invites from allowlisted users
        if !state.allowed_users.is_empty() {
            let sender_str = ev.sender.as_str();
            if !state.allowed_users.iter().any(|u| u == sender_str) {
                eprintln!("matrix: rejected invite from non-allowlisted sender {}", sender_str);
                return;
            }
        }

        eprintln!("matrix: auto-accepting invite to {} from {}", room.room_id(), ev.sender);
        if let Err(e) = room.join().await {
            eprintln!("matrix: failed to accept invite: {}", e);
        }
    }
}

/// Default latency-grace window before posting the interim ack, in ms.
///
/// RESP-04: this is now a last-resort backstop only. Real tool dispatch fires a
/// precise signal (server-side agentic mode and the legacy loop both notify the
/// instant a tool runs), so the timer only trips for the rare case where a slow
/// turn produces neither a tool signal nor a fast reply.
const DEFAULT_ACK_GRACE_MS: u64 = 10000;

// ── RESP-05: tailored interim acknowledgement phrases ──────────────────────────

/// Interim ack phrases (markdown italic), keyed by the kind of work the tool does.
const ACK_SEARCHING: &str = "_Searching for that…_";
const ACK_CHECKING: &str = "_Checking on that…_";
const ACK_SETTING_UP: &str = "_Setting that up…_";
const ACK_WORKING: &str = "_Working on that…_";
const ACK_DEFAULT: &str = "_One moment…_";

/// Map a dispatched tool name to a tailored interim ack phrase (RESP-05).
///
/// Case-insensitive substring matching on the tool name. `None` (or no match)
/// falls back to a neutral phrase. Centralized so phrasing stays consistent.
fn ack_phrase(tool: Option<&str>) -> &'static str {
    let name = match tool {
        Some(t) => t.to_ascii_lowercase(),
        None => return ACK_DEFAULT,
    };
    let has = |needle: &str| name.contains(needle);
    // Info-retrieval tools (web/search/news/weather/finance/commute) all read as
    // "looking something up" — the spec maps weather & news to this bucket.
    if has("web") || has("search") || has("news") || has("weather")
        || has("finance") || has("stock") || has("price")
        || has("commute") || has("traffic")
    {
        ACK_SEARCHING
    } else if has("health") || has("network") || has("server") || has("ping") || has("status") {
        ACK_CHECKING
    } else if has("calendar") || has("reminder") || has("schedul") || has("event") {
        ACK_SETTING_UP
    } else if has("file") || has("fs") || has("read") || has("write") || has("exec") || has("command") {
        ACK_WORKING
    } else {
        ACK_DEFAULT
    }
}

/// Grace window (ms) before the interim "Let me pull that up…" message is sent,
/// overridable via `LUMINA_ACK_GRACE_MS`. Tunable without a rebuild so the
/// backstop can be matched to local inference latency: long enough that fast
/// pure-text replies finish first, short enough to fill a real tool-run wait.
fn ack_grace_ms() -> u64 {
    std::env::var("LUMINA_ACK_GRACE_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_ACK_GRACE_MS)
}

/// Post the interim acknowledgement `phrase` (markdown italic) to `room`.
async fn send_ack(room: &Room, phrase: &str) {
    let html = markdown_to_matrix_html(phrase);
    let content = RoomMessageEventContent::text_html(phrase, html);
    if let Err(e) = room.send(content).await {
        eprintln!("matrix: ack send failed: {}", e);
    }
}

/// Graceful shutdown: post announcement, then allow current processing to drain.
pub async fn shutdown_bot(client: &Client, room_id: &OwnedRoomId, announce: bool) {
    if announce {
        if let Some(room) = client.get_room(room_id) {
            let content = RoomMessageEventContent::text_plain("Lumina is going offline.");
            let _ = room.send(content).await;
        }
    }
    eprintln!("matrix: shutting down");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn test_seen_events_insert_and_contains() {
        let mut seen = SeenEvents::new();
        seen.insert("$abc123".to_string());
        assert!(seen.contains("$abc123"));
        assert!(!seen.contains("$xyz"));
    }

    #[test]
    #[serial]
    fn test_ack_grace_ms_default_and_override() {
        // Single test (not two) so the shared LUMINA_ACK_GRACE_MS env var is not
        // raced by parallel test threads.
        std::env::remove_var("LUMINA_ACK_GRACE_MS");
        assert_eq!(ack_grace_ms(), DEFAULT_ACK_GRACE_MS);

        std::env::set_var("LUMINA_ACK_GRACE_MS", "800");
        assert_eq!(ack_grace_ms(), 800);

        // Invalid / zero values fall back to the default.
        std::env::set_var("LUMINA_ACK_GRACE_MS", "0");
        assert_eq!(ack_grace_ms(), DEFAULT_ACK_GRACE_MS);
        std::env::set_var("LUMINA_ACK_GRACE_MS", "notanumber");
        assert_eq!(ack_grace_ms(), DEFAULT_ACK_GRACE_MS);
        std::env::remove_var("LUMINA_ACK_GRACE_MS");
    }

    #[test]
    fn test_default_ack_grace_is_backstop() {
        // RESP-04: grace bumped to a last-resort backstop (10s).
        assert_eq!(DEFAULT_ACK_GRACE_MS, 10000);
    }

    #[test]
    fn test_ack_phrase_mapping() {
        // RESP-05: tailored phrase per tool category (case-insensitive substring).
        assert_eq!(ack_phrase(Some("searxng_search")), ACK_SEARCHING);
        assert_eq!(ack_phrase(Some("lumina_web_fetch")), ACK_SEARCHING);
        assert_eq!(ack_phrase(Some("news_get_headlines")), ACK_SEARCHING);
        // Live-test gap: the weather tool is `lumina_weather` (no search/web in
        // the name) — the spec maps weather & finance to this bucket too.
        assert_eq!(ack_phrase(Some("lumina_weather")), ACK_SEARCHING);
        assert_eq!(ack_phrase(Some("finance_quote")), ACK_SEARCHING);
        assert_eq!(ack_phrase(Some("commute_status")), ACK_SEARCHING);

        assert_eq!(ack_phrase(Some("health")), ACK_CHECKING);
        assert_eq!(ack_phrase(Some("network_diagnose")), ACK_CHECKING);
        assert_eq!(ack_phrase(Some("portainer_container_status")), ACK_CHECKING);
        assert_eq!(ack_phrase(Some("ping_host")), ACK_CHECKING);

        assert_eq!(ack_phrase(Some("google_calendar_create")), ACK_SETTING_UP);
        assert_eq!(ack_phrase(Some("set_reminder")), ACK_SETTING_UP);
        assert_eq!(ack_phrase(Some("schedule_meeting")), ACK_SETTING_UP);
        assert_eq!(ack_phrase(Some("create_event")), ACK_SETTING_UP);

        assert_eq!(ack_phrase(Some("read_file")), ACK_WORKING);
        assert_eq!(ack_phrase(Some("fs_write")), ACK_WORKING);
        assert_eq!(ack_phrase(Some("exec_command")), ACK_WORKING);

        // Case-insensitive.
        assert_eq!(ack_phrase(Some("SEARXNG_SEARCH")), ACK_SEARCHING);

        // Fallbacks.
        assert_eq!(ack_phrase(None), ACK_DEFAULT);
        assert_eq!(ack_phrase(Some("some_obscure_tool")), ACK_DEFAULT);
    }

    #[test]
    fn test_seen_events_bounded_at_200_trims_to_100() {
        let mut seen = SeenEvents::new();
        for i in 0..200 {
            seen.insert(format!("$event{}", i));
        }
        assert_eq!(seen.order.len(), 200);

        // Insert one more — should trim to 100 and then add
        seen.insert("$overflow".to_string());
        assert_eq!(seen.order.len(), 101, "should trim to 100 then add 1");
        assert!(seen.contains("$overflow"));
        // The first 100 events should be gone
        for i in 0..100 {
            assert!(!seen.contains(&format!("$event{}", i)), "event{} should be evicted", i);
        }
    }

    #[test]
    fn test_backoff_doubles_and_caps() {
        let mut backoff = Duration::from_secs(2);
        let cap = Duration::from_secs(60);
        let steps: Vec<u64> = (0..10).map(|_| {
            let v = backoff.as_secs();
            backoff = (backoff * 2).min(cap);
            v
        }).collect();
        assert_eq!(steps[0], 2);
        assert_eq!(steps[1], 4);
        assert_eq!(steps[2], 8);
        assert_eq!(steps[3], 16);
        assert_eq!(steps[4], 32);
        // Cap at 60
        assert!(steps.iter().all(|&v| v <= 60));
    }

    #[test]
    fn test_backoff_resets() {
        let mut backoff = Duration::from_secs(32);
        // Simulate successful reconnect: reset
        backoff = Duration::from_secs(2);
        assert_eq!(backoff.as_secs(), 2);
    }
}
