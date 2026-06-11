//! EDGE-09: Telegram channel adapter.
//!
//! Implements the [`Channel`] trait using the `teloxide` crate to connect a
//! Telegram bot to the Lumina core loop.
//!
//! # Configuration (all from environment / vault)
//! - `TELEGRAM_BOT_TOKEN` — Telegram Bot API token (required).
//! - `TELEGRAM_ALLOWED_USERS` — comma-separated Telegram numeric user IDs that
//!   are permitted to send messages (required; empty = no one is allowed).
//!
//! # Feature flag
//! This entire module is compiled only when the `telegram` Cargo feature is
//! enabled.  Nothing in this file may be referenced unless `--features telegram`
//! is active.
//!
//! # Message chunking
//! Telegram enforces a 4 096-character limit per message.  Long responses are
//! split on newline boundaries via [`chunk_message`], preserving paragraph
//! structure wherever possible.

#![cfg(feature = "telegram")]

use async_trait::async_trait;
use std::env;
use teloxide::prelude::*;
use teloxide::types::{ChatAction, MessageId, ReplyParameters};
use tokio::sync::mpsc;

use super::{Channel, ChannelMessage, MessageContext};
use crate::error::{LuminaError, Result};

/// Maximum characters allowed in a single Telegram message.
const TELEGRAM_MAX_CHARS: usize = 4096;

// ── TelegramChannel ─────────────────────────────────────────────────────────

/// Channel adapter that connects a Telegram bot to the Lumina core loop.
///
/// Messages from users whose numeric ID appears in `allowed_user_ids` are
/// forwarded to the core loop.  All other messages are silently dropped.
pub struct TelegramChannel {
    /// The environment-variable name that holds the Telegram Bot API token.
    ///
    /// At runtime the token is read from this env var (or from the vault key
    /// with the same name).  Storing the *name*, not the value, keeps the
    /// token out of memory until it is actually needed.
    token_env_key: String,
    /// Numeric Telegram user IDs whose messages are processed.
    ///
    /// `teloxide::types::UserId` wraps `u64`; we store as `u64` to avoid
    /// silent lossy casts when the ID exceeds `i64::MAX`.
    allowed_user_ids: Vec<u64>,
    /// Initialized `Bot` instance, created once in [`start`] and reused in
    /// [`send_response`] to share the underlying `reqwest::Client` and its
    /// connection pool across all API calls.
    bot: Option<Bot>,
    /// Handle to the background polling task (held for cancellation on stop).
    poll_handle: Option<tokio::task::JoinHandle<()>>,
}

impl TelegramChannel {
    /// Create a new `TelegramChannel`.
    ///
    /// # Parameters
    /// - `token_env_key` — name of the environment variable (or vault key)
    ///   that holds the bot token.  E.g. `"TELEGRAM_BOT_TOKEN"`.
    /// - `allowed_users` — list of numeric Telegram user IDs that are
    ///   permitted to interact with the bot.
    pub fn new(token_env_key: impl Into<String>, allowed_users: Vec<u64>) -> Self {
        Self {
            token_env_key: token_env_key.into(),
            allowed_user_ids: allowed_users,
            bot: None,
            poll_handle: None,
        }
    }

    /// Attempt to construct a `TelegramChannel` from the environment.
    ///
    /// Returns `None` when `TELEGRAM_BOT_TOKEN` is not set or is empty,
    /// allowing callers to skip Telegram registration without an error.
    ///
    /// `TELEGRAM_ALLOWED_USERS` is parsed as a comma-separated list of numeric
    /// IDs (e.g. `"123456,789012"`).  Non-numeric tokens are skipped with a
    /// warning printed to stderr.
    pub fn from_env() -> Option<Self> {
        let token = env::var("TELEGRAM_BOT_TOKEN").ok().filter(|t| !t.is_empty())?;
        // We have a token — the key name is fixed.
        let _ = token; // consumed only to confirm presence; stored by key name

        let allowed: Vec<u64> = env::var("TELEGRAM_ALLOWED_USERS")
            .unwrap_or_default()
            .split(',')
            .filter_map(|s| {
                let s = s.trim();
                if s.is_empty() {
                    return None;
                }
                match s.parse::<u64>() {
                    Ok(id) => Some(id),
                    Err(_) => {
                        eprintln!(
                            "telegram: ignoring non-numeric user ID in \
                             TELEGRAM_ALLOWED_USERS: {:?}",
                            s
                        );
                        None
                    }
                }
            })
            .collect();

        Some(Self::new("TELEGRAM_BOT_TOKEN", allowed))
    }
}

// ── Channel trait ─────────────────────────────────────────────────────────

#[async_trait]
impl Channel for TelegramChannel {
    fn name(&self) -> &str {
        "telegram"
    }

    /// Start the Telegram polling loop.
    ///
    /// Reads the bot token from the configured environment variable, creates a
    /// `teloxide::Bot` (shared via `Clone` with the background poller), stores
    /// it on `self` for reuse in [`send_response`], and spawns a background
    /// task that polls the Telegram API and forwards messages from allowed users
    /// to `sender`.
    ///
    /// Returns immediately after spawning the task.
    async fn start(&mut self, sender: mpsc::Sender<ChannelMessage>) -> Result<()> {
        let token = env::var(&self.token_env_key)
            .ok()
            .filter(|t| !t.is_empty())
            .ok_or_else(|| {
                LuminaError::Config(format!(
                    "telegram: environment variable '{}' is not set or empty — \
                     set it to your Telegram Bot API token",
                    self.token_env_key
                ))
            })?;

        // Create the Bot once. `Bot` is `Clone` (shares the underlying
        // `reqwest::Client` and its connection pool), so we keep one copy on
        // the struct for `send_response` and give one copy to the poll task.
        let bot = Bot::new(token);
        let poll_bot = bot.clone();
        self.bot = Some(bot);

        let allowed = self.allowed_user_ids.clone();

        let handle = tokio::spawn(async move {
            teloxide::repl(poll_bot, move |bot: Bot, msg: Message| {
                let sender = sender.clone();
                let allowed = allowed.clone();
                async move {
                    // Security: silently discard messages from non-allowed users.
                    // UserId wraps u64; store and compare as u64 to avoid lossy casts.
                    let user_id: u64 = msg.from().map(|u| u.id.0).unwrap_or(0);

                    if !allowed.contains(&user_id) {
                        return Ok(());
                    }

                    let text = match msg.text() {
                        Some(t) => t.to_string(),
                        None => {
                            // Non-text message (photo, file, etc.)
                            let _ = bot
                                .send_message(
                                    msg.chat.id,
                                    "I can only process text messages for now.",
                                )
                                .await;
                            return Ok(());
                        }
                    };

                    // Send typing indicator — shows the bot is working on a response.
                    // Errors are ignored: a missing typing indicator is cosmetic.
                    let _ = bot.send_chat_action(msg.chat.id, ChatAction::Typing).await;

                    let chat_id_str = msg.chat.id.0.to_string();
                    // Store the originating message ID in thread_id so
                    // send_response can use it as a reply-to reference.
                    let origin_msg_id = msg.id.0.to_string();
                    let channel_msg = ChannelMessage {
                        source_channel: "telegram".to_string(),
                        sender_id: user_id.to_string(), // u64 → decimal string
                        content: text,
                        context: MessageContext {
                            // room_id holds the chat ID so send_response can parse it.
                            room_id: Some(chat_id_str),
                            request_id: None,
                            // thread_id carries the originating message ID for reply threading.
                            thread_id: Some(origin_msg_id),
                        },
                        timestamp: std::time::SystemTime::now(),
                    };

                    if sender.send(channel_msg).await.is_err() {
                        // Core loop dropped the receiver — stop processing.
                        eprintln!("telegram: channel sender dropped; stopping poll loop");
                    }

                    Ok(())
                }
            })
            .await;
        });

        self.poll_handle = Some(handle);
        Ok(())
    }

    /// Send a response back to the Telegram chat identified by `context.room_id`.
    ///
    /// Long responses are split into multiple messages using [`chunk_message`].
    /// Messages are sent as plain text (no parse mode) so that special
    /// characters such as `<`, `>`, `&`, and `*` are never misinterpreted.
    /// Callers that need rich formatting should pre-process the text before
    /// calling this method.
    async fn send_response(&self, response: &str, context: &MessageContext) -> Result<()> {
        let bot = self.bot.as_ref().ok_or_else(|| {
            LuminaError::Config(
                "telegram: send_response called before start() — bot not initialised".to_string(),
            )
        })?;

        let chat_id_str = context.room_id.as_deref().ok_or_else(|| {
            LuminaError::Config(
                "telegram: MessageContext.room_id is required to send a response".to_string(),
            )
        })?;

        let chat_id_num: i64 = chat_id_str.parse().map_err(|_| {
            LuminaError::Config(format!(
                "telegram: MessageContext.room_id '{}' is not a valid chat ID",
                chat_id_str
            ))
        })?;

        let chat_id = ChatId(chat_id_num);

        // Parse the originating message ID for reply threading.
        // thread_id is set by the message handler to the original message's ID.
        let reply_to: Option<MessageId> = context
            .thread_id
            .as_deref()
            .and_then(|s| s.parse::<i32>().ok())
            .map(MessageId);

        let chunks = chunk_message(response);
        for (i, chunk) in chunks.into_iter().enumerate() {
            // No ParseMode — plain text avoids parse errors on <, >, &, *.
            let mut request = bot.send_message(chat_id, &chunk);
            // Only the first chunk replies to the originating message, so the
            // user gets a clear reference without every chunk being a reply.
            if i == 0 {
                if let Some(msg_id) = reply_to {
                    request = request.reply_parameters(ReplyParameters::new(msg_id));
                }
            }
            request
                .await
                .map_err(|e| LuminaError::Internal(format!("telegram send error: {}", e)))?;
        }

        Ok(())
    }

    /// Stop the Telegram polling task.
    async fn stop(&mut self) -> Result<()> {
        if let Some(handle) = self.poll_handle.take() {
            handle.abort();
        }
        Ok(())
    }
}

// ── chunk_message ────────────────────────────────────────────────────────────

/// Split `text` into chunks of at most [`TELEGRAM_MAX_CHARS`] **characters**
/// (Unicode scalar values, i.e. `char` count).
///
/// The algorithm prefers to split on newline boundaries so paragraph structure
/// is preserved.  If a single line exceeds the limit, it is hard-split at a
/// character (not byte) boundary to avoid panics on multi-byte UTF-8 sequences.
///
/// # Why char count instead of bytes
/// Telegram's 4 096-character limit counts UTF-16 code units.  Counting
/// Unicode scalar values (`chars`) is a safe approximation: it is exact for
/// BMP characters (no surrogates) and conservative (≤ actual limit) for
/// characters outside the BMP.  This prevents both panics and oversized
/// messages.
///
/// # Guarantees
/// - Returns at least one element (even for empty input, returns `[""]`).
/// - Every returned string contains at most `TELEGRAM_MAX_CHARS` chars.
pub fn chunk_message(text: &str) -> Vec<String> {
    if text.chars().count() <= TELEGRAM_MAX_CHARS {
        return vec![text.to_string()];
    }

    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_chars: usize = 0;

    for line in text.split_inclusive('\n') {
        let line_chars = line.chars().count();

        if current_chars + line_chars <= TELEGRAM_MAX_CHARS {
            current.push_str(line);
            current_chars += line_chars;
        } else {
            // Flush whatever we have accumulated so far.
            if !current.is_empty() {
                chunks.push(current.clone());
                current.clear();
                current_chars = 0;
            }

            // Hard-split the line at character (not byte) boundaries.
            let mut remaining_chars = line.chars();
            loop {
                let taken: String = remaining_chars.by_ref().take(TELEGRAM_MAX_CHARS).collect();
                if taken.is_empty() {
                    break;
                }
                let taken_len = taken.chars().count();
                if taken_len < TELEGRAM_MAX_CHARS {
                    // Last fragment — accumulate rather than flush immediately.
                    current.push_str(&taken);
                    current_chars = taken_len;
                    break;
                }
                chunks.push(taken);
            }
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    if chunks.is_empty() {
        chunks.push(String::new());
    }

    chunks
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    // ── name() ──────────────────────────────────────────────────────────────

    #[test]
    fn name_returns_telegram() {
        let ch = TelegramChannel::new("TELEGRAM_BOT_TOKEN", vec![]);
        assert_eq!(ch.name(), "telegram");
    }

    // ── from_env() ──────────────────────────────────────────────────────────

    #[test]
    #[serial]
    fn from_env_returns_none_without_token() {
        let _g = ENV_LOCK.lock().unwrap();
        env::remove_var("TELEGRAM_BOT_TOKEN");
        env::remove_var("TELEGRAM_ALLOWED_USERS");
        assert!(TelegramChannel::from_env().is_none());
    }

    #[test]
    #[serial]
    fn from_env_returns_none_when_token_is_empty() {
        let _g = ENV_LOCK.lock().unwrap();
        env::set_var("TELEGRAM_BOT_TOKEN", "");
        env::remove_var("TELEGRAM_ALLOWED_USERS");
        assert!(TelegramChannel::from_env().is_none());
        env::remove_var("TELEGRAM_BOT_TOKEN");
    }

    #[test]
    #[serial]
    fn from_env_returns_some_with_valid_token() {
        let _g = ENV_LOCK.lock().unwrap();
        env::set_var("TELEGRAM_BOT_TOKEN", "123:abc");
        env::remove_var("TELEGRAM_ALLOWED_USERS");
        let ch = TelegramChannel::from_env();
        assert!(ch.is_some());
        assert_eq!(ch.unwrap().name(), "telegram");
        env::remove_var("TELEGRAM_BOT_TOKEN");
    }

    #[test]
    #[serial]
    fn from_env_parses_allowed_users() {
        let _g = ENV_LOCK.lock().unwrap();
        env::set_var("TELEGRAM_BOT_TOKEN", "123:abc");
        env::set_var("TELEGRAM_ALLOWED_USERS", "123,456");
        let ch = TelegramChannel::from_env().expect("should be Some");
        assert_eq!(ch.allowed_user_ids, vec![123_u64, 456_u64]);
        env::remove_var("TELEGRAM_BOT_TOKEN");
        env::remove_var("TELEGRAM_ALLOWED_USERS");
    }

    #[test]
    #[serial]
    fn from_env_skips_non_numeric_user_ids() {
        let _g = ENV_LOCK.lock().unwrap();
        env::set_var("TELEGRAM_BOT_TOKEN", "123:abc");
        env::set_var("TELEGRAM_ALLOWED_USERS", "123,not-a-number,456");
        let ch = TelegramChannel::from_env().expect("should be Some");
        // "not-a-number" is silently skipped
        assert_eq!(ch.allowed_user_ids, vec![123_u64, 456_u64]);
        env::remove_var("TELEGRAM_BOT_TOKEN");
        env::remove_var("TELEGRAM_ALLOWED_USERS");
    }

    #[test]
    #[serial]
    fn from_env_empty_allowed_users_gives_empty_vec() {
        let _g = ENV_LOCK.lock().unwrap();
        env::set_var("TELEGRAM_BOT_TOKEN", "123:abc");
        env::set_var("TELEGRAM_ALLOWED_USERS", "");
        let ch = TelegramChannel::from_env().expect("should be Some");
        assert!(ch.allowed_user_ids.is_empty());
        env::remove_var("TELEGRAM_BOT_TOKEN");
        env::remove_var("TELEGRAM_ALLOWED_USERS");
    }

    // ── chunk_message() ─────────────────────────────────────────────────────

    #[test]
    fn chunk_short_message_is_single_chunk() {
        let text = "Hello, world!";
        let chunks = chunk_message(text);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], text);
    }

    #[test]
    fn chunk_empty_message_returns_single_empty_chunk() {
        let chunks = chunk_message("");
        // Empty string is short enough to be returned as-is.
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "");
    }

    #[test]
    fn chunk_exactly_max_chars_is_one_chunk() {
        let text = "a".repeat(TELEGRAM_MAX_CHARS);
        let chunks = chunk_message(&text);
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn chunk_over_limit_produces_multiple_chunks() {
        // A single string of 4097 'a' chars — no newlines to split on.
        let text = "a".repeat(TELEGRAM_MAX_CHARS + 1);
        let chunks = chunk_message(&text);
        assert!(chunks.len() >= 2, "expected multiple chunks, got {}", chunks.len());
        for chunk in &chunks {
            assert!(
                chunk.len() <= TELEGRAM_MAX_CHARS,
                "chunk exceeds limit: {} chars",
                chunk.len()
            );
        }
        // Reassembled content must equal the original.
        let reassembled: String = chunks.join("");
        assert_eq!(reassembled, text);
    }

    #[test]
    fn chunk_splits_on_newlines_when_possible() {
        // Two paragraphs, each just under the limit — should stay in separate chunks.
        let para = "x".repeat(TELEGRAM_MAX_CHARS - 1);
        let text = format!("{}\n{}", para, para);
        let chunks = chunk_message(&text);
        assert_eq!(
            chunks.len(),
            2,
            "expected 2 chunks separated at newline, got {}: {:?}",
            chunks.len(),
            chunks.iter().map(|c| c.len()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn chunk_all_chunks_within_limit() {
        // Generate a message made of many short lines totalling >4096 chars.
        let line = "The quick brown fox jumps over the lazy dog.\n";
        let repeats = (TELEGRAM_MAX_CHARS / line.len()) * 3;
        let text = line.repeat(repeats);

        for chunk in chunk_message(&text) {
            assert!(
                chunk.len() <= TELEGRAM_MAX_CHARS,
                "chunk exceeds 4096: {} chars",
                chunk.len()
            );
        }
    }

    #[test]
    fn chunk_reconstruction_preserves_content() {
        // Verify that joining all chunks reproduces the original text exactly.
        let line = "line of text\n";
        let repeats = 500;
        let text = line.repeat(repeats);
        let chunks = chunk_message(&text);
        let reassembled: String = chunks.join("");
        assert_eq!(reassembled, text);
    }

    #[test]
    fn chunk_does_not_panic_on_multibyte_utf8() {
        // Build a string of emoji (4 bytes each in UTF-8) that crosses the 4096-char
        // boundary.  A naive byte-based split_at would panic here.
        let emoji = "\u{1F600}"; // 4 bytes in UTF-8, 1 char
        let text = emoji.repeat(TELEGRAM_MAX_CHARS + 10);
        let chunks = chunk_message(&text);
        for chunk in &chunks {
            assert!(chunk.chars().count() <= TELEGRAM_MAX_CHARS);
        }
        // Content must survive round-trip.
        let reassembled: String = chunks.join("");
        assert_eq!(reassembled, text);
    }

    #[test]
    fn chunk_counts_chars_not_bytes() {
        // A 3-byte UTF-8 char repeated exactly TELEGRAM_MAX_CHARS times
        // should still be a single chunk (chars == limit, bytes >> limit).
        let ch3 = "\u{2603}"; // snowman, 3 bytes in UTF-8, 1 char
        let text = ch3.repeat(TELEGRAM_MAX_CHARS);
        let chunks = chunk_message(&text);
        assert_eq!(chunks.len(), 1, "exactly-limit char count should be one chunk");
    }

    // ── stop() without start() ──────────────────────────────────────────────

    #[tokio::test]
    async fn stop_without_start_is_ok() {
        let mut ch = TelegramChannel::new("TELEGRAM_BOT_TOKEN", vec![]);
        assert!(ch.stop().await.is_ok());
    }

    // ── send_response() without start() ─────────────────────────────────────

    #[tokio::test]
    async fn send_response_returns_error_when_bot_not_initialised() {
        // start() was never called → self.bot is None → Config error.
        let ch = TelegramChannel::new("TELEGRAM_BOT_TOKEN", vec![]);
        let ctx = MessageContext { room_id: Some("123456".to_string()), ..Default::default() };
        let result = ch.send_response("hello", &ctx).await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("start()") || msg.contains("not initialised") || msg.contains("bot"),
            "unexpected error: {}",
            msg
        );
    }

    #[tokio::test]
    async fn send_response_returns_error_when_room_id_missing() {
        // Manually set self.bot to simulate post-start state without a real token.
        // We test the room_id validation path only.
        let mut ch = TelegramChannel::new("TELEGRAM_BOT_TOKEN", vec![]);
        ch.bot = Some(Bot::new("fake-token-for-test"));
        let ctx = MessageContext::default(); // room_id is None
        let result = ch.send_response("hello", &ctx).await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("room_id") || msg.contains("required"), "unexpected: {}", msg);
    }

    #[tokio::test]
    async fn send_response_returns_error_when_room_id_not_numeric() {
        let mut ch = TelegramChannel::new("TELEGRAM_BOT_TOKEN", vec![]);
        ch.bot = Some(Bot::new("fake-token-for-test"));
        let ctx = MessageContext {
            room_id: Some("not-a-number".to_string()),
            ..Default::default()
        };
        let result = ch.send_response("hello", &ctx).await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("not a valid chat ID") || msg.contains("not-a-number"),
            "unexpected: {}",
            msg
        );
    }
}
