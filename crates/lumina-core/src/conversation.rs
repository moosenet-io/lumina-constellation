//! Conversation history (P1-02), SQLCipher store (P1-03), session management (P1-04).
//! open_default + engram key helpers added for P1-05.
//!
//! CONV-02 (S78): the [`buffer`] submodule adds an in-memory, per-user
//! [`buffer::ConversationBuffer`] (Tier-1 working memory) used for multi-turn
//! continuity on the live agentic path.

pub mod buffer;
pub mod engram_flush;
pub mod summarizer;

use crate::chord::ChatMessage;
use crate::error::{LuminaError, Result};
use crate::secure_string::ZeroizingString;
use crate::vault;
use rand::RngCore;
use rusqlite::{params, Connection};
use secrecy::ExposeSecret;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// One conversation turn, with content zeroed on drop.
struct StoredMessage {
    role: ZeroizingString,
    content: ZeroizingString,
}

impl StoredMessage {
    fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: ZeroizingString::new(role.into()),
            content: ZeroizingString::new(content.into()),
        }
    }

    fn to_chat_message(&self) -> ChatMessage {
        ChatMessage::text(self.role.as_str(), self.content.as_str())
    }
}

/// In-memory rolling window of chat turns for a session.
pub struct ConversationHistory {
    pub session_id: String,
    messages: Vec<StoredMessage>,
}

impl ConversationHistory {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            messages: Vec::new(),
        }
    }

    /// Append a turn to the history.
    pub fn push(&mut self, role: impl Into<String>, content: impl Into<String>) {
        self.messages.push(StoredMessage::new(role, content));
    }

    /// Return the last `n` messages in chronological order.
    /// If fewer than `n` messages exist, returns all available.
    pub fn window(&self, n: usize) -> Vec<ChatMessage> {
        let start = self.messages.len().saturating_sub(n);
        self.messages[start..].iter().map(|m| m.to_chat_message()).collect()
    }

    /// Convert all stored messages to wire `ChatMessage`s.
    pub fn as_chat_messages(&self) -> Vec<ChatMessage> {
        self.messages.iter().map(|m| m.to_chat_message()).collect()
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

// ── ConversationStore ──────────────────────────────────────────────────────

/// SQLCipher-backed persistence for conversation turns per session.
///
/// Open with `open(path, key)` where `key` is 32 raw bytes from
/// `vault::manager().get("ENGRAM_DB_KEY")`. Key is never read from env.
pub struct ConversationStore {
    conn: Connection,
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl ConversationStore {
    /// Open (or create) the SQLCipher database at `db_path` with `key`.
    ///
    /// `key` must be 32 raw bytes; it is passed to SQLCipher as a hex PRAGMA.
    /// The ENGRAM_DB_KEY vault secret is the source — never read from env.
    pub fn open(db_path: &Path, key: &[u8]) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| LuminaError::Config(format!("Cannot create conversation store dir: {e}")))?;
        }

        let conn = Connection::open(db_path)
            .map_err(|e| LuminaError::Config(format!("Cannot open conversation store: {e}")))?;

        let hex_key = hex::encode(key);
        conn.execute_batch(&format!("PRAGMA key = \"x'{hex_key}'\";"))
            .map_err(|e| LuminaError::Config(format!("Failed to set conversation store key: {e}")))?;

        conn.execute_batch("SELECT count(*) FROM sqlite_master;")
            .map_err(|_| LuminaError::SecurityViolation(
                "Conversation store key is incorrect — cannot open database".to_string()
            ))?;

        conn.execute_batch("PRAGMA journal_mode = WAL;")
            .map_err(|e| LuminaError::Config(format!("WAL mode failed: {e}")))?;

        let store = Self { conn };
        store.ensure_schema()?;
        Ok(store)
    }

    /// Insert one message for `session_id`.
    pub fn append(&self, session_id: &str, role: &str, content: &str) -> Result<()> {
        let ts = unix_now();
        self.conn.execute(
            "INSERT INTO messages (session_id, role, content, ts) VALUES (?1, ?2, ?3, ?4)",
            params![session_id, role, content, ts],
        ).map_err(|e| LuminaError::Config(format!("append failed: {e}")))?;
        Ok(())
    }

    /// Load the most recent `limit` messages for `session_id` in chronological order.
    pub fn load(&self, session_id: &str, limit: usize) -> Result<ConversationHistory> {
        let mut stmt = self.conn.prepare(
            "SELECT role, content FROM (
               SELECT id, role, content, ts FROM messages WHERE session_id = ?1
               ORDER BY ts DESC, id DESC LIMIT ?2
             ) ORDER BY ts ASC, id ASC",
        ).map_err(|e| LuminaError::Config(format!("load prepare failed: {e}")))?;

        let rows = stmt.query_map(params![session_id, limit as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }).map_err(|e| LuminaError::Config(format!("load query failed: {e}")))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| LuminaError::Config(format!("load row failed: {e}")))?;

        let mut history = ConversationHistory::new(session_id);
        for (role, content) in rows {
            // Hold in ZeroizingString briefly; push converts back to plain String for ChatMessage.
            let _r = ZeroizingString::new(role.clone());
            let _c = ZeroizingString::new(content.clone());
            history.push(role, content);
        }
        Ok(history)
    }

    /// Return the Unix timestamp (seconds) of the most recent message for `session_id`.
    pub fn last_activity(&self, session_id: &str) -> Result<Option<i64>> {
        let result: Option<i64> = self.conn.query_row(
            "SELECT MAX(ts) FROM messages WHERE session_id = ?1",
            params![session_id],
            |row| row.get(0),
        ).map_err(|e| LuminaError::Config(format!("last_activity failed: {e}")))?;
        Ok(result)
    }

    fn ensure_schema(&self) -> Result<()> {
        self.conn.execute_batch("
            CREATE TABLE IF NOT EXISTS messages (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT    NOT NULL,
                role       TEXT    NOT NULL,
                content    BLOB    NOT NULL,
                ts         INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_session_ts ON messages (session_id, ts);
        ").map_err(|e| LuminaError::Config(format!("Schema creation failed: {e}")))?;
        Ok(())
    }
}

// ── SessionManager ─────────────────────────────────────────────────────────

/// Mint a random 32-hex-char session ID (no uuid dep needed).
fn new_session_id() -> String {
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Tracks the current session and decides whether to resume or start fresh.
pub struct SessionManager {
    current: Option<String>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self { current: None }
    }

    /// Resolve the session for an incoming turn.
    ///
    /// Returns `(session_id, is_new)`:
    /// - `/new` input → always mints a fresh session
    /// - Current session active within the idle window → resume (`is_new=false`)
    /// - No session, or idle timeout → mint new (`is_new=true`)
    ///
    /// `now` is the current Unix timestamp in seconds; `idle_minutes` from
    /// `config.session_idle_minutes()`. Clock skew (negative delta) is treated
    /// as within-window — never panics.
    pub fn resolve(
        &mut self,
        input: &str,
        store: &ConversationStore,
        now: i64,
        idle_minutes: u64,
    ) -> (String, bool) {
        if input.trim() == "/new" {
            let id = new_session_id();
            self.current = Some(id.clone());
            return (id, true);
        }

        if let Some(ref current) = self.current {
            let within_window = match store.last_activity(current) {
                Ok(Some(last_ts)) => {
                    let delta = now.saturating_sub(last_ts);
                    // negative or zero delta (clock skew) → within window
                    delta <= (idle_minutes * 60) as i64
                }
                // No messages yet for this session → treat as active (within window)
                Ok(None) => true,
                // Store error → be conservative, resume
                Err(_) => true,
            };
            if within_window {
                return (current.clone(), false);
            }
        }

        let id = new_session_id();
        self.current = Some(id.clone());
        (id, true)
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

// ── Conversation store helpers (P1-05) ─────────────────────────────────────

/// Default path for the conversation store.
/// Reads `ENGRAM_DB_PATH` env var; falls back to ~/.lumina/conversation.db.
pub fn conversation_db_path() -> PathBuf {
    if let Ok(p) = std::env::var("ENGRAM_DB_PATH") {
        return PathBuf::from(p);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".lumina")
        .join("conversation.db")
}

/// Get or create the ENGRAM_DB_KEY.
/// Reads from vault (fresh VaultStore::load, not cached singleton), then env var fallback
/// (for test environments), then generates + persists best-effort.
/// Key is hex-encoded 32 bytes. Mirrors `get_or_create_training_key` in training_store.rs.
pub fn get_or_create_engram_key() -> Result<Vec<u8>> {
    // 1. Vault — read via fresh load so we see keys persisted in the same process lifetime
    if let Ok(store) = vault::VaultStore::load() {
        if let Some(stored) = store.get("ENGRAM_DB_KEY") {
            if let Ok(bytes) = hex::decode(stored.expose_secret()) {
                if bytes.len() >= 32 {
                    return Ok(bytes);
                }
            }
        }
    }

    // 2. Env var fallback for test environments where vault is unavailable / read-only
    if let Ok(hex_val) = std::env::var("ENGRAM_DB_KEY") {
        if let Ok(bytes) = hex::decode(&hex_val) {
            if bytes.len() >= 32 {
                return Ok(bytes);
            }
        }
    }

    // 3. Generate a new 32-byte key
    let mut key = vec![0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut key);
    let hex_key = hex::encode(&key);

    // Persist best-effort; if vault is read-only or unavailable, key is ephemeral
    if let Ok(mut store) = vault::VaultStore::load() {
        let _ = store.set(
            "ENGRAM_DB_KEY".to_string(),
            secrecy::SecretString::new(hex_key.into()),
        );
    }

    Ok(key)
}

impl ConversationStore {
    /// Open using the default path and ENGRAM_DB_KEY from vault (auto-generates on first run).
    pub fn open_default() -> Result<Self> {
        let path = conversation_db_path();
        let key = get_or_create_engram_key()?;
        Self::open(&path, &key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_window_returns_last_n() {
        let mut h = ConversationHistory::new("sess-1");
        for i in 0..25 {
            h.push("user", format!("message {i}"));
        }
        let w = h.window(20);
        assert_eq!(w.len(), 20);
        assert_eq!(w[0].content.as_deref(), Some("message 5"));
        assert_eq!(w[19].content.as_deref(), Some("message 24"));
    }

    #[test]
    fn test_window_fewer_than_n() {
        let mut h = ConversationHistory::new("sess-2");
        h.push("user", "only message");
        let w = h.window(20);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].content.as_deref(), Some("only message"));
    }

    #[test]
    fn test_window_empty() {
        let h = ConversationHistory::new("sess-3");
        assert!(h.window(20).is_empty());
    }

    #[test]
    fn test_as_chat_messages_round_trip() {
        let mut h = ConversationHistory::new("sess-4");
        h.push("user", "hello");
        h.push("assistant", "hi there");
        let msgs = h.as_chat_messages();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[0].content.as_deref(), Some("hello"));
        assert_eq!(msgs[1].role, "assistant");
        assert_eq!(msgs[1].content.as_deref(), Some("hi there"));
    }

    #[test]
    fn test_window_exact_n() {
        let mut h = ConversationHistory::new("sess-5");
        for i in 0..20 {
            h.push("user", format!("msg {i}"));
        }
        let w = h.window(20);
        assert_eq!(w.len(), 20);
        assert_eq!(w[0].content.as_deref(), Some("msg 0"));
        assert_eq!(w[19].content.as_deref(), Some("msg 19"));
    }

    // ── ConversationStore tests ────────────────────────────────────────────

    fn test_key() -> Vec<u8> { vec![0u8; 32] }

    fn tmp_db(tag: &str) -> std::path::PathBuf {
        let p = std::path::PathBuf::from(format!("/tmp/lumina_conv_test_{tag}.db"));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[tokio::test]
    async fn test_store_append_load_order() {
        let path = tmp_db("order");
        let store = ConversationStore::open(&path, &test_key()).unwrap();

        store.append("s1", "user", "first").unwrap();
        store.append("s1", "assistant", "second").unwrap();
        store.append("s1", "user", "third").unwrap();

        let hist = store.load("s1", 10).unwrap();
        let msgs = hist.as_chat_messages();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[0].content.as_deref(), Some("first"));
        assert_eq!(msgs[1].content.as_deref(), Some("second"));
        assert_eq!(msgs[2].content.as_deref(), Some("third"));

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn test_store_last_activity() {
        let path = tmp_db("activity");
        let store = ConversationStore::open(&path, &test_key()).unwrap();

        assert!(store.last_activity("unknown").unwrap().is_none());

        store.append("s2", "user", "hello").unwrap();
        let ts = store.last_activity("s2").unwrap();
        assert!(ts.is_some());
        assert!(ts.unwrap() > 0);

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn test_store_unknown_session_returns_empty() {
        let path = tmp_db("empty");
        let store = ConversationStore::open(&path, &test_key()).unwrap();
        let hist = store.load("ghost", 10).unwrap();
        assert!(hist.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn test_store_wrong_key_returns_error() {
        let path = tmp_db("wrongkey");
        ConversationStore::open(&path, &test_key()).unwrap();
        let bad_key = vec![0xFFu8; 32];
        assert!(ConversationStore::open(&path, &bad_key).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn test_store_load_limit_applied() {
        let path = tmp_db("limit");
        let store = ConversationStore::open(&path, &test_key()).unwrap();
        for i in 0..10 {
            store.append("s3", "user", &format!("msg {i}")).unwrap();
        }
        let hist = store.load("s3", 3).unwrap();
        // 3 most recent in chronological order
        assert_eq!(hist.len(), 3);
        let msgs = hist.as_chat_messages();
        assert_eq!(msgs[0].content.as_deref(), Some("msg 7"));
        assert_eq!(msgs[2].content.as_deref(), Some("msg 9"));
        let _ = std::fs::remove_file(&path);
    }

    // ── SessionManager tests ───────────────────────────────────────────────

    #[test]
    fn test_new_command_always_new_session() {
        let path = tmp_db("sesmgr_new");
        let store = ConversationStore::open(&path, &test_key()).unwrap();
        let mut mgr = SessionManager::new();
        let (id1, is_new1) = mgr.resolve("/new", &store, 1000, 30);
        assert!(is_new1);
        let (id2, is_new2) = mgr.resolve("/new", &store, 1001, 30);
        assert!(is_new2);
        assert_ne!(id1, id2, "/new should produce distinct IDs");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_within_idle_window_resumes() {
        let path = tmp_db("sesmgr_resume");
        let store = ConversationStore::open(&path, &test_key()).unwrap();
        let mut mgr = SessionManager::new();

        // Start a session
        let (id1, _) = mgr.resolve("/new", &store, 1000, 30);
        // Record activity so last_activity returns a ts
        store.append(&id1, "user", "hello").unwrap();
        // Fake the ts to 1000 in the DB for determinism
        store.conn.execute(
            "UPDATE messages SET ts = 1000 WHERE session_id = ?1",
            rusqlite::params![&id1],
        ).unwrap();

        // 5 minutes later (< 30 min idle) → resume
        let (id2, is_new2) = mgr.resolve("hello again", &store, 1300, 30);
        assert!(!is_new2, "Should resume within idle window");
        assert_eq!(id1, id2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_idle_timeout_mints_new_session() {
        let path = tmp_db("sesmgr_timeout");
        let store = ConversationStore::open(&path, &test_key()).unwrap();
        let mut mgr = SessionManager::new();

        let (id1, _) = mgr.resolve("/new", &store, 1000, 30);
        store.append(&id1, "user", "hello").unwrap();
        store.conn.execute(
            "UPDATE messages SET ts = 1000 WHERE session_id = ?1",
            rusqlite::params![&id1],
        ).unwrap();

        // 31 minutes later → timeout → new session
        let (id2, is_new2) = mgr.resolve("new message", &store, 1000 + 31 * 60, 30);
        assert!(is_new2, "Should mint new session after idle timeout");
        assert_ne!(id1, id2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_clock_skew_negative_delta_resumes() {
        let path = tmp_db("sesmgr_skew");
        let store = ConversationStore::open(&path, &test_key()).unwrap();
        let mut mgr = SessionManager::new();

        let (id1, _) = mgr.resolve("/new", &store, 2000, 30);
        store.append(&id1, "user", "hi").unwrap();
        // ts in future relative to now=1000 → negative delta → within window
        store.conn.execute(
            "UPDATE messages SET ts = 5000 WHERE session_id = ?1",
            rusqlite::params![&id1],
        ).unwrap();

        let (id2, is_new2) = mgr.resolve("next", &store, 1000, 30);
        assert!(!is_new2, "Clock skew should not trigger new session");
        assert_eq!(id1, id2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_first_message_mints_session() {
        let path = tmp_db("sesmgr_first");
        let store = ConversationStore::open(&path, &test_key()).unwrap();
        let mut mgr = SessionManager::new();
        let (id, is_new) = mgr.resolve("hello there", &store, 1000, 30);
        assert!(is_new, "First message should create new session");
        assert!(!id.is_empty());
        let _ = std::fs::remove_file(&path);
    }
}
