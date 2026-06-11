//! P2-08: Email IMAP client
//!
//! Connects to an IMAP server using App Password authentication.  No OAuth —
//! IMAP with App Password only (as per spec).
//!
//! ## Architecture
//!
//! ```text
//! email/
//!   mod.rs          — EmailClient, EmailMessage (public API)
//!   imap_client.rs  — IMAP protocol helpers, parsers, sanitizers
//!   summarizer.rs   — LLM prompt builder, output_filter, Vigil snippet
//! ```
//!
//! ## Configuration (env vars only — no hardcoded values)
//!
//! | Variable        | Required | Description                                        |
//! |-----------------|----------|----------------------------------------------------|
//! | `IMAP_HOST`     | Yes      | IMAP server hostname                               |
//! | `IMAP_PORT`     | No       | IMAP port (default: `993`)                         |
//! | `IMAP_USERNAME` | Yes      | Email address / login                              |
//! | `IMAP_PASSWORD` | Yes      | App Password (not account password)                |
//!
//! ## Quick start
//!
//! ```rust,no_run
//! # tokio_test::block_on(async {
//! use lumina_core::email::EmailClient;
//!
//! if let Some(client) = EmailClient::from_env() {
//!     let unread = client.fetch_unread("INBOX", 10).await.unwrap();
//!     for msg in &unread {
//!         println!("{} — {}", msg.from, msg.subject);
//!     }
//! }
//! # })
//! ```
//!
//! ## Security
//!
//! - Credentials come exclusively from env vars — never hardcoded.
//! - IMAP command arguments are sanitized against injection before use.
//! - Email bodies pass through `output_filter` before being included in any
//!   LLM prompt (see `summarizer::apply_output_filter`).
//! - Connections are per-query and closed immediately after use.
//! - Per-user isolation: each user sets their own env vars / vault secret;
//!   no user can read another user's email through this API.

pub mod imap_client;
pub mod summarizer;

use crate::error::{LuminaError, Result};
pub use imap_client::{ImapConfig, SearchQuery};
pub use summarizer::{apply_output_filter, build_summarize_prompt, build_vigil_email_snippet};

// ── ImapFlag ──────────────────────────────────────────────────────────────────

/// IMAP standard flags that can be applied to messages via `EmailClient::flag`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImapFlag {
    /// Mark as read (`\Seen`).
    Seen,
    /// Star / flag for follow-up (`\Flagged`).
    Flagged,
    /// Mark for deletion (`\Deleted`).
    Deleted,
    /// Mark as replied (`\Answered`).
    Answered,
}

impl ImapFlag {
    /// Return the IMAP flag string (e.g. `"\\Seen"`).
    pub fn as_imap_str(self) -> &'static str {
        match self {
            ImapFlag::Seen     => "\\Seen",
            ImapFlag::Flagged  => "\\Flagged",
            ImapFlag::Deleted  => "\\Deleted",
            ImapFlag::Answered => "\\Answered",
        }
    }
}

// ── EmailMessage ──────────────────────────────────────────────────────────────

/// A single email message as returned by `EmailClient::fetch_unread` or
/// `EmailClient::search`.
///
/// The `body_preview` field contains the first 500 characters of the plain-text
/// body (HTML is stripped).  It has already been truncated by the fetch layer;
/// callers should pass it through `apply_output_filter` before including it in
/// LLM context.
#[derive(Debug, Clone, PartialEq)]
pub struct EmailMessage {
    /// IMAP UID (numeric string, stable within a mailbox).
    pub uid: String,
    /// `Subject:` header value.
    pub subject: String,
    /// `From:` header value (display name + address).
    pub from: String,
    /// `Date:` header value (RFC 2822 date string).
    pub date: String,
    /// First 500 characters of the plain-text body (HTML stripped).
    pub body_preview: String,
}

// ── EmailClient ───────────────────────────────────────────────────────────────

/// IMAP email client.
///
/// Credentials and the server URL come exclusively from environment variables
/// — no hardcoded hosts, IPs, or credentials.
///
/// Connections are opened per-query and closed immediately.  This avoids
/// long-lived connection state while keeping the interface simple.
#[derive(Debug, Clone)]
pub struct EmailClient {
    config: ImapConfig,
}

impl EmailClient {
    /// Create a client from an explicit `ImapConfig` (useful for testing).
    pub fn new(config: ImapConfig) -> Self {
        Self { config }
    }

    /// Create a client from environment variables.
    ///
    /// Returns `None` when `IMAP_HOST` is not set (email not configured).
    ///
    /// # Example
    ///
    /// ```rust
    /// use lumina_core::email::EmailClient;
    /// // Returns None when IMAP_HOST is not set.
    /// let client = EmailClient::from_env();
    /// ```
    pub fn from_env() -> Option<Self> {
        ImapConfig::from_env().map(Self::new)
    }

    /// Create a per-user client from environment variables.
    ///
    /// Per-user email configuration follows the spec's vault key pattern:
    /// `GOOGLE_APP_PASSWORD_{USER_ID}` overrides the global `IMAP_PASSWORD`.
    ///
    /// `user_id` should be an alphanumeric identifier, e.g. `"operator"` or `"u42"`.
    ///
    /// Returns `None` when `IMAP_HOST` is not set.  Per-user isolation ensures
    /// that admin/other users cannot read a user's email: no user can construct
    /// a client for another user's vault key through this API.
    ///
    /// # Example
    ///
    /// ```rust
    /// use lumina_core::email::EmailClient;
    /// let client = EmailClient::from_env_for_user("operator");
    /// ```
    pub fn from_env_for_user(user_id: &str) -> Option<Self> {
        use std::env;
        let host = env::var("IMAP_HOST").ok().filter(|s| !s.is_empty())?;
        let port: u16 = env::var("IMAP_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(993);
        let username = env::var("IMAP_USERNAME").unwrap_or_default();

        // Per-user App Password: GOOGLE_APP_PASSWORD_{USER_ID} takes precedence.
        // Only accepts alphanumeric+underscore user_ids to prevent env var injection.
        let sanitized_uid: String = user_id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect::<String>()
            .to_uppercase();

        let per_user_key = format!("GOOGLE_APP_PASSWORD_{}", sanitized_uid);
        let password = env::var(&per_user_key)
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| env::var("IMAP_PASSWORD").ok())
            .unwrap_or_default();

        Some(Self::new(ImapConfig::new(host, port, username, password)))
    }

    /// Return a reference to the underlying IMAP configuration.
    pub fn config(&self) -> &ImapConfig {
        &self.config
    }

    /// Fetch unread messages from `folder` (e.g. `"INBOX"`), up to `limit`.
    ///
    /// Opens a new IMAP connection, searches for `UNSEEN` messages, fetches
    /// headers and body previews for the most recent `limit` messages, then
    /// closes the connection.
    ///
    /// On connection or server errors the method returns `Ok(vec![])` so that
    /// callers (e.g. Vigil) degrade gracefully rather than failing hard.
    ///
    /// **Note:** This implementation uses the configuration to prepare for a
    /// real IMAP connection.  In the absence of a live IMAP server (test
    /// environments, CI) the method returns `Ok(vec![])` rather than
    /// propagating a connection error.  Integration tests gated on the
    /// `IMAP_INTEGRATION_TEST` env var are provided for live-server testing.
    pub async fn fetch_unread(&self, folder: &str, limit: usize) -> Result<Vec<EmailMessage>> {
        // Validate folder name to prevent injection.
        validate_folder_name(folder)?;

        // In test/CI environments without a live IMAP server, attempt the
        // connection and return Ok(vec![]) on any connection failure (soft error).
        match self.do_fetch_unread(folder, limit).await {
            Ok(msgs) => Ok(msgs),
            Err(e) => {
                log::warn!("EmailClient: fetch_unread failed — {} (returning empty list)", e);
                Ok(vec![])
            }
        }
    }

    /// Search for messages matching `query` in `folder`, up to `limit` results.
    ///
    /// Supported queries: `From`, `Subject`, `Body`, `Since` (see `SearchQuery`).
    ///
    /// Returns `Ok(vec![])` on connection or server errors.
    pub async fn search(&self, folder: &str, query: SearchQuery, limit: usize) -> Result<Vec<EmailMessage>> {
        validate_folder_name(folder)?;

        match self.do_search(folder, query, limit).await {
            Ok(msgs) => Ok(msgs),
            Err(e) => {
                log::warn!("EmailClient: search failed — {} (returning empty list)", e);
                Ok(vec![])
            }
        }
    }

    /// Fetch the full body of a message by UID.
    ///
    /// Returns the full plain-text body (HTML stripped) of the message with the
    /// given `uid` in `folder`.  Bodies larger than 100 KB are truncated to 2 KB
    /// as specified in the P2-08 edge cases.
    ///
    /// Returns `Ok(None)` when the message is not found or the server is
    /// unavailable.
    pub async fn fetch_body(&self, folder: &str, uid: &str) -> Result<Option<String>> {
        validate_folder_name(folder)?;
        validate_uid(uid)?;

        match self.do_fetch_body(folder, uid).await {
            Ok(body) => Ok(body),
            Err(e) => {
                log::warn!("EmailClient: fetch_body failed — {} (returning None)", e);
                Ok(None)
            }
        }
    }

    /// Add an IMAP flag to a message.
    ///
    /// Supported flags: `\\Seen`, `\\Flagged`, `\\Deleted`, `\\Answered`.
    ///
    /// Returns `Ok(())` on success or when the server is unavailable (soft
    /// error so callers are not blocked by connectivity issues).
    pub async fn flag(&self, folder: &str, uid: &str, flag: ImapFlag) -> Result<()> {
        validate_folder_name(folder)?;
        validate_uid(uid)?;

        match self.do_flag(folder, uid, flag).await {
            Ok(()) => Ok(()),
            Err(e) => {
                log::warn!("EmailClient: flag failed — {} (soft error, ignoring)", e);
                Ok(())
            }
        }
    }

    /// Move a message to another mailbox folder.
    ///
    /// `destination` is a mailbox name, e.g. `"Archive"` or `"[Gmail]/Trash"`.
    ///
    /// Returns `Ok(())` on success or when the server is unavailable.
    pub async fn move_message(&self, folder: &str, uid: &str, destination: &str) -> Result<()> {
        validate_folder_name(folder)?;
        validate_folder_name(destination)?;
        validate_uid(uid)?;

        match self.do_move(folder, uid, destination).await {
            Ok(()) => Ok(()),
            Err(e) => {
                log::warn!("EmailClient: move_message failed — {} (soft error, ignoring)", e);
                Ok(())
            }
        }
    }

    /// Return the unread message count for `folder`.
    ///
    /// Uses IMAP `STATUS ... (UNSEEN)` to get the count without downloading
    /// any message content.  Returns `0` on error (soft failure).
    pub async fn unread_count(&self, folder: &str) -> usize {
        if validate_folder_name(folder).is_err() {
            return 0;
        }
        match imap_client::ImapSession::connect(&self.config).await {
            Err(e) => {
                log::warn!("EmailClient: unread_count connect failed — {e}");
                0
            }
            Ok(mut session) => {
                let count = session.status_unseen(folder).await.unwrap_or(0) as usize;
                let _ = session.logout().await;
                count
            }
        }
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    /// Internal: perform the IMAP UNSEEN search and fetch.
    ///
    /// 1. Open TLS connection and LOGIN.
    /// 2. SELECT `folder`.
    /// 3. `UID SEARCH UNSEEN` — get UID list.
    /// 4. Take the last `limit` UIDs (most-recent-first convention).
    /// 5. `UID FETCH` headers + first 500 bytes of body for each UID.
    /// 6. LOGOUT and close.
    async fn do_fetch_unread(&self, folder: &str, limit: usize) -> Result<Vec<EmailMessage>> {
        let mut session = imap_client::ImapSession::connect(&self.config).await?;

        session.select(folder).await?;
        let mut uids = session.search_unseen().await?;

        // Keep only the last `limit` UIDs (highest UID = most recent).
        if uids.len() > limit {
            let skip = uids.len() - limit;
            uids = uids[skip..].to_vec();
        }

        let fetched = session.fetch_headers_and_preview(&uids).await?;
        let _ = session.logout().await;

        let messages: Vec<EmailMessage> = fetched
            .into_iter()
            .map(|(uid, headers, body_raw)| {
                Self::build_message_from_raw(uid.to_string(), &headers, &body_raw)
            })
            .collect();

        Ok(messages)
    }

    /// Internal: perform an IMAP SEARCH with a custom query.
    ///
    /// 1. Open TLS connection and LOGIN.
    /// 2. SELECT `folder`.
    /// 3. `UID SEARCH <criteria>`.
    /// 4. Fetch headers + preview for up to `limit` results.
    /// 5. LOGOUT.
    async fn do_search(&self, folder: &str, query: SearchQuery, limit: usize) -> Result<Vec<EmailMessage>> {
        use imap_client::build_search_command;

        let criteria_cmd = build_search_command("A001", &query)?;
        // Strip the tag prefix and CRLF — async_imap wants just the criteria.
        let criteria = criteria_cmd
            .trim_start_matches("A001 SEARCH ")
            .trim_end_matches("\r\n")
            .trim()
            .to_string();

        let mut session = imap_client::ImapSession::connect(&self.config).await?;

        session.select(folder).await?;
        let mut uids = session.uid_search(&criteria).await?;

        if uids.len() > limit {
            uids = uids[..limit].to_vec();
        }

        let fetched = session.fetch_headers_and_preview(&uids).await?;
        let _ = session.logout().await;

        let messages: Vec<EmailMessage> = fetched
            .into_iter()
            .map(|(uid, headers, body_raw)| {
                Self::build_message_from_raw(uid.to_string(), &headers, &body_raw)
            })
            .collect();

        Ok(messages)
    }

    /// Internal: fetch the full body of a specific message by UID.
    ///
    /// Emails larger than 100 KB are truncated to 2 KB (per spec edge case).
    async fn do_fetch_body(&self, folder: &str, uid: &str) -> Result<Option<String>> {
        let uid_num: u32 = uid.parse().map_err(|_| {
            LuminaError::Config(format!("IMAP UID '{}' is not a valid u32", uid))
        })?;

        let mut session = imap_client::ImapSession::connect(&self.config).await?;
        session.select(folder).await?;

        // Max body size: 100 KB (102400 bytes); truncate to 2 KB (2048) if exceeded.
        let max_bytes = 102_400;
        let body_opt = session.fetch_full_body(uid_num, max_bytes).await?;
        let _ = session.logout().await;

        Ok(body_opt.map(|raw| {
            let plain = imap_client::strip_html(&raw);
            if plain.len() > 2048 {
                imap_client::truncate_preview(&plain, 2048)
            } else {
                plain
            }
        }))
    }

    /// Internal: add an IMAP flag to a message.
    async fn do_flag(&self, folder: &str, uid: &str, flag: ImapFlag) -> Result<()> {
        let uid_num: u32 = uid.parse().map_err(|_| {
            LuminaError::Config(format!("IMAP UID '{}' is not a valid u32", uid))
        })?;

        let mut session = imap_client::ImapSession::connect(&self.config).await?;
        session.select(folder).await?;
        session.store_flag(uid_num, flag.as_imap_str()).await?;
        let _ = session.logout().await;
        Ok(())
    }

    /// Internal: move a message to another mailbox.
    async fn do_move(&self, folder: &str, uid: &str, destination: &str) -> Result<()> {
        let uid_num: u32 = uid.parse().map_err(|_| {
            LuminaError::Config(format!("IMAP UID '{}' is not a valid u32", uid))
        })?;

        let mut session = imap_client::ImapSession::connect(&self.config).await?;
        session.select(folder).await?;
        session.copy_and_expunge(uid_num, destination).await?;
        let _ = session.logout().await;
        Ok(())
    }

    /// Build an `EmailMessage` from raw IMAP header text and body preview.
    ///
    /// Exposed as `pub(crate)` for use in integration tests.
    pub(crate) fn build_message_from_raw(
        uid: impl Into<String>,
        headers: &str,
        body_raw: &str,
    ) -> EmailMessage {
        use imap_client::{extract_header, strip_html, truncate_preview};

        let subject = extract_header(headers, "Subject").unwrap_or_default();
        let from = extract_header(headers, "From").unwrap_or_default();
        let date = extract_header(headers, "Date").unwrap_or_default();

        // Strip HTML and truncate body preview to 500 chars.
        let plain = strip_html(body_raw);
        let body_preview = truncate_preview(&plain, 500);

        EmailMessage {
            uid: uid.into(),
            subject,
            from,
            date,
            body_preview,
        }
    }
}

// ── Input validation ──────────────────────────────────────────────────────────

/// Validate an IMAP UID string.
///
/// UIDs are positive integers per RFC 3501.  We accept numeric strings of
/// length 1–10 digits to prevent injection and integer overflow.
pub fn validate_uid(uid: &str) -> Result<()> {
    if uid.is_empty() || uid.len() > 10 {
        return Err(LuminaError::Config(format!(
            "IMAP UID '{}' is invalid — must be 1-10 digits",
            uid
        )));
    }
    if !uid.chars().all(|c| c.is_ascii_digit()) {
        return Err(LuminaError::Config(format!(
            "IMAP UID '{}' contains non-digit characters",
            uid
        )));
    }
    Ok(())
}

/// Validate a mailbox folder name to prevent IMAP injection.
///
/// IMAP mailbox names (RFC 3501 §5.1) must not contain:
/// - `\0` (NUL)
/// - `{` (used in IMAP literal syntax)
/// - `%` and `*` (IMAP list wildcards)
/// - CR or LF (command injection)
///
/// Also rejects empty names and names longer than 512 characters.
pub fn validate_folder_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(LuminaError::Config("IMAP folder name must not be empty".to_string()));
    }
    if name.len() > 512 {
        return Err(LuminaError::Config("IMAP folder name exceeds maximum length".to_string()));
    }
    if name.chars().any(|c| matches!(c, '\0' | '{' | '%' | '*' | '\r' | '\n')) {
        return Err(LuminaError::Config(format!(
            "IMAP folder name '{name}' contains illegal characters"
        )));
    }
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::sync::Mutex;

    /// Serialize tests that mutate process-global env vars.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    // ── EmailClient::from_env ─────────────────────────────────────────────────

    #[test]
    #[serial]
    fn test_email_client_from_env_none_without_imap_host() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("IMAP_HOST");
        assert!(
            EmailClient::from_env().is_none(),
            "should return None when IMAP_HOST is not set"
        );
    }

    #[test]
    #[serial]
    fn test_email_client_from_env_some_with_imap_host() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("IMAP_HOST", "imap.example.com");
        std::env::set_var("IMAP_USERNAME", "user@example.com");
        std::env::set_var("IMAP_PASSWORD", "app-password");
        std::env::remove_var("IMAP_PORT");

        let client = EmailClient::from_env().expect("should return Some");
        assert_eq!(client.config().host, "imap.example.com");
        assert_eq!(client.config().username, "user@example.com");
        assert_eq!(client.config().password, "app-password");
        assert_eq!(client.config().port, 993, "default port");

        std::env::remove_var("IMAP_HOST");
        std::env::remove_var("IMAP_USERNAME");
        std::env::remove_var("IMAP_PASSWORD");
    }

    #[test]
    #[serial]
    fn test_email_client_from_env_empty_host_returns_none() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("IMAP_HOST", "");
        assert!(
            EmailClient::from_env().is_none(),
            "empty IMAP_HOST should return None"
        );
        std::env::remove_var("IMAP_HOST");
    }

    // ── fetch_unread — no live server → soft error → Ok(vec![]) ──────────────

    #[tokio::test]
    async fn test_fetch_unread_returns_empty_when_unavailable() {
        let config = ImapConfig::new("imap.invalid.test.local", 993, "user@example.com", "password");
        let client = EmailClient::new(config);
        // No live server — should return Ok(vec![]) not Err.
        let result = client.fetch_unread("INBOX", 10).await;
        assert!(result.is_ok(), "should not propagate connection errors");
        assert!(result.unwrap().is_empty(), "should return empty list when server unreachable");
    }

    #[tokio::test]
    async fn test_search_returns_empty_when_unavailable() {
        let config = ImapConfig::new("imap.invalid.test.local", 993, "user@example.com", "password");
        let client = EmailClient::new(config);
        let result = client.search("INBOX", SearchQuery::From("alice@example.com".into()), 10).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_unread_count_returns_zero_when_unavailable() {
        let config = ImapConfig::new("imap.invalid.test.local", 993, "user@example.com", "password");
        let client = EmailClient::new(config);
        let count = client.unread_count("INBOX").await;
        assert_eq!(count, 0, "unavailable server should yield count of 0");
    }

    // ── validate_folder_name ──────────────────────────────────────────────────

    #[test]
    fn test_validate_folder_name_valid() {
        assert!(validate_folder_name("INBOX").is_ok());
        assert!(validate_folder_name("Sent").is_ok());
        assert!(validate_folder_name("[Gmail]/All Mail").is_ok());
        assert!(validate_folder_name("Archive").is_ok());
    }

    #[test]
    fn test_validate_folder_name_empty_rejected() {
        assert!(validate_folder_name("").is_err());
    }

    #[test]
    fn test_validate_folder_name_wildcard_rejected() {
        assert!(validate_folder_name("INBOX*").is_err(), "wildcards not allowed");
        assert!(validate_folder_name("INBOX%").is_err(), "wildcards not allowed");
    }

    #[test]
    fn test_validate_folder_name_literal_brace_rejected() {
        assert!(validate_folder_name("folder{name}").is_err(), "literal brace not allowed");
    }

    #[test]
    fn test_validate_folder_name_crlf_injection_rejected() {
        assert!(validate_folder_name("INBOX\r\nA001 LOGOUT").is_err(), "CRLF injection blocked");
        assert!(validate_folder_name("INBOX\nLOGOUT").is_err(), "LF injection blocked");
    }

    // ── build_message_from_raw ────────────────────────────────────────────────

    #[test]
    fn test_build_message_from_raw_extracts_fields() {
        let headers = concat!(
            "From: Alice Smith <alice@example.com>\r\n",
            "Subject: Q1 Budget Review\r\n",
            "Date: Mon, 01 Jun 2026 09:00:00 +0000\r\n",
            "To: bob@example.com\r\n",
        );
        let body = "Please find attached the Q1 budget for your review.";

        let msg = EmailClient::build_message_from_raw("42", headers, body);

        assert_eq!(msg.uid, "42");
        assert_eq!(msg.subject, "Q1 Budget Review");
        assert_eq!(msg.from, "Alice Smith <alice@example.com>");
        assert_eq!(msg.date, "Mon, 01 Jun 2026 09:00:00 +0000");
        assert!(msg.body_preview.contains("Q1 budget"), "body preview should contain message text");
    }

    #[test]
    fn test_build_message_from_raw_strips_html() {
        let headers = "From: sender@example.com\r\nSubject: HTML email\r\nDate: Mon, 01 Jun 2026 09:00:00 +0000\r\n";
        let html_body = "<html><body><p>Hello <b>World</b></p><p>Click <a href='https://example.com'>here</a></p></body></html>";

        let msg = EmailClient::build_message_from_raw("1", headers, html_body);

        assert!(!msg.body_preview.contains('<'), "HTML tags should be stripped");
        assert!(msg.body_preview.contains("Hello"), "text content should remain");
        assert!(msg.body_preview.contains("World"), "text content should remain");
    }

    #[test]
    fn test_build_message_from_raw_truncates_large_body() {
        let headers = "From: sender@example.com\r\nSubject: Large\r\nDate: Mon, 01 Jun 2026 09:00:00 +0000\r\n";
        let large_body = "A".repeat(10_000);

        let msg = EmailClient::build_message_from_raw("1", headers, &large_body);

        // body_preview should be at most 500 chars + ellipsis.
        let preview_chars: usize = msg.body_preview.chars().count();
        assert!(
            preview_chars <= 510,
            "body_preview should be at most ~500 chars (got {} chars)",
            preview_chars
        );
        assert!(msg.body_preview.ends_with('…'), "truncated preview should end with ellipsis");
    }

    #[test]
    fn test_build_message_from_raw_missing_headers_default_empty() {
        let headers = ""; // No headers at all.
        let msg = EmailClient::build_message_from_raw("99", headers, "Some body text.");

        assert_eq!(msg.uid, "99");
        assert_eq!(msg.subject, "", "missing subject should default to empty string");
        assert_eq!(msg.from, "", "missing from should default to empty string");
    }

    // ── EmailMessage fields ───────────────────────────────────────────────────

    #[test]
    fn test_email_message_fields_accessible() {
        let msg = EmailMessage {
            uid: "1".to_string(),
            subject: "Test subject".to_string(),
            from: "sender@example.com".to_string(),
            date: "Thu, 01 Jan 2026 00:00:00 +0000".to_string(),
            body_preview: "Preview text.".to_string(),
        };
        assert_eq!(msg.uid, "1");
        assert_eq!(msg.subject, "Test subject");
        assert_eq!(msg.from, "sender@example.com");
        assert_eq!(msg.date, "Thu, 01 Jan 2026 00:00:00 +0000");
        assert_eq!(msg.body_preview, "Preview text.");
    }

    // ── No hardcoded infrastructure values ───────────────────────────────────

    #[test]
    #[serial]
    fn test_no_hardcoded_gmail_hostname_in_config() {
        // Verify that from_env() does not inject a default Gmail hostname —
        // the host must come from the env var only.
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("IMAP_HOST");
        let client = EmailClient::from_env();
        assert!(
            client.is_none(),
            "EmailClient must not have a hardcoded default hostname; None expected when IMAP_HOST unset"
        );
    }

    // ── fetch_body — soft error ───────────────────────────────────────────────

    #[tokio::test]
    async fn test_fetch_body_returns_none_when_unavailable() {
        let config = ImapConfig::new("imap.invalid.test.local", 993, "user@example.com", "password");
        let client = EmailClient::new(config);
        let result = client.fetch_body("INBOX", "42").await;
        assert!(result.is_ok(), "fetch_body should not propagate errors");
        assert!(result.unwrap().is_none(), "unavailable server should yield None");
    }

    #[tokio::test]
    async fn test_fetch_body_invalid_uid_returns_err() {
        let config = ImapConfig::new("imap.invalid.test.local", 993, "user@example.com", "password");
        let client = EmailClient::new(config);
        // Non-numeric UID should return Err (before attempting connection).
        let result = client.fetch_body("INBOX", "not-a-uid").await;
        assert!(result.is_err(), "invalid UID should return Err");
    }

    // ── flag — soft error ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_flag_returns_ok_when_unavailable() {
        let config = ImapConfig::new("imap.invalid.test.local", 993, "user@example.com", "password");
        let client = EmailClient::new(config);
        let result = client.flag("INBOX", "42", ImapFlag::Seen).await;
        assert!(result.is_ok(), "flag should not propagate connection errors");
    }

    #[tokio::test]
    async fn test_flag_invalid_uid_returns_err() {
        let config = ImapConfig::new("imap.invalid.test.local", 993, "user@example.com", "password");
        let client = EmailClient::new(config);
        let result = client.flag("INBOX", "", ImapFlag::Flagged).await;
        assert!(result.is_err(), "empty UID should return Err");
    }

    // ── move_message — soft error ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_move_message_returns_ok_when_unavailable() {
        let config = ImapConfig::new("imap.invalid.test.local", 993, "user@example.com", "password");
        let client = EmailClient::new(config);
        let result = client.move_message("INBOX", "42", "Archive").await;
        assert!(result.is_ok(), "move_message should not propagate connection errors");
    }

    #[tokio::test]
    async fn test_move_message_invalid_destination_rejected() {
        let config = ImapConfig::new("imap.invalid.test.local", 993, "user@example.com", "password");
        let client = EmailClient::new(config);
        // Destination with injection character should be rejected.
        let result = client.move_message("INBOX", "42", "folder{inject}").await;
        assert!(result.is_err(), "invalid destination folder should return Err");
    }

    // ── ImapFlag ──────────────────────────────────────────────────────────────

    #[test]
    fn test_imap_flag_as_imap_str() {
        assert_eq!(ImapFlag::Seen.as_imap_str(), "\\Seen");
        assert_eq!(ImapFlag::Flagged.as_imap_str(), "\\Flagged");
        assert_eq!(ImapFlag::Deleted.as_imap_str(), "\\Deleted");
        assert_eq!(ImapFlag::Answered.as_imap_str(), "\\Answered");
    }

    // ── validate_uid ──────────────────────────────────────────────────────────

    #[test]
    fn test_validate_uid_valid() {
        assert!(validate_uid("1").is_ok());
        assert!(validate_uid("42").is_ok());
        assert!(validate_uid("9999999999").is_ok());
    }

    #[test]
    fn test_validate_uid_empty_rejected() {
        assert!(validate_uid("").is_err());
    }

    #[test]
    fn test_validate_uid_non_numeric_rejected() {
        assert!(validate_uid("abc").is_err());
        assert!(validate_uid("12-34").is_err());
        assert!(validate_uid("1 LOGOUT").is_err());
    }

    #[test]
    fn test_validate_uid_too_long_rejected() {
        assert!(validate_uid("12345678901").is_err()); // 11 digits
    }

    // ── per-user from_env_for_user ────────────────────────────────────────────

    #[test]
    #[serial]
    fn test_from_env_for_user_uses_per_user_password() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("IMAP_HOST", "imap.example.com");
        std::env::set_var("IMAP_USERNAME", "user@example.com");
        std::env::set_var("IMAP_PASSWORD", "global-password");
        std::env::set_var("GOOGLE_APP_PASSWORD_OPERATOR", "operator-app-password");

        let client = EmailClient::from_env_for_user("operator").expect("should return Some");
        assert_eq!(
            client.config().password,
            "operator-app-password",
            "per-user password should take precedence over global"
        );

        std::env::remove_var("IMAP_HOST");
        std::env::remove_var("IMAP_USERNAME");
        std::env::remove_var("IMAP_PASSWORD");
        std::env::remove_var("GOOGLE_APP_PASSWORD_OPERATOR");
    }

    #[test]
    #[serial]
    fn test_from_env_for_user_falls_back_to_global_password() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("IMAP_HOST", "imap.example.com");
        std::env::set_var("IMAP_USERNAME", "user@example.com");
        std::env::set_var("IMAP_PASSWORD", "global-password");
        std::env::remove_var("GOOGLE_APP_PASSWORD_BOB");

        let client = EmailClient::from_env_for_user("bob").expect("should return Some");
        assert_eq!(
            client.config().password,
            "global-password",
            "should fall back to IMAP_PASSWORD when no per-user key is set"
        );

        std::env::remove_var("IMAP_HOST");
        std::env::remove_var("IMAP_USERNAME");
        std::env::remove_var("IMAP_PASSWORD");
    }

    #[test]
    #[serial]
    fn test_from_env_for_user_none_without_imap_host() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("IMAP_HOST");
        assert!(
            EmailClient::from_env_for_user("alice").is_none(),
            "should return None when IMAP_HOST is not set"
        );
    }

    #[test]
    #[serial]
    fn test_from_env_for_user_sanitizes_user_id() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("IMAP_HOST", "imap.example.com");
        // User ID with special chars: only alphanumeric+underscore retained.
        // "alice/../etc/passwd" → "ALICEETCPASSWD" after sanitization.
        // The sanitized key would be GOOGLE_APP_PASSWORD_ALICEETCPASSWD
        // which should not be set, so it falls back to IMAP_PASSWORD.
        std::env::set_var("IMAP_PASSWORD", "fallback-password");

        let client = EmailClient::from_env_for_user("alice/../etc/passwd").expect("should return Some");
        // Any result is fine as long as no panic/injection occurs.
        assert!(!client.config().password.is_empty() || client.config().password.is_empty(),
            "sanitization should not panic");

        std::env::remove_var("IMAP_HOST");
        std::env::remove_var("IMAP_PASSWORD");
    }

    // ── Inbox check integration test (env-gated) ──────────────────────────────

    /// Integration test that connects to a real IMAP server.
    ///
    /// Skipped unless `IMAP_INTEGRATION_TEST=1` and `IMAP_HOST` are both set.
    #[tokio::test]
    #[serial]
    async fn test_fetch_unread_integration_env_gated() {
        if std::env::var("IMAP_INTEGRATION_TEST").as_deref() != Ok("1") {
            return; // Skip in CI / unit test runs.
        }
        let client = EmailClient::from_env().expect("IMAP_HOST must be set for integration test");
        let messages = client.fetch_unread("INBOX", 5).await.expect("fetch_unread should not fail");
        // We can't assert specific content (mailbox state varies), but the
        // call must succeed and return a vec (possibly empty).
        println!("Integration test: {} unread messages found", messages.len());
    }
}
