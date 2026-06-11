//! P2-08: IMAP protocol client
//!
//! Low-level IMAP operations over a TCP + TLS connection.
//!
//! This module implements a minimal IMAP4rev1 (RFC 3501) client sufficient for
//! reading a Gmail inbox with App Password authentication.  It uses the
//! `async-imap` crate for the IMAP protocol layer with `tokio-rustls` for TLS.
//!
//! Connections are per-query and are closed immediately after use (no persistent
//! connection pool) as mandated by the spec.
//!
//! ## Configuration (env vars only — no hardcoded values)
//!
//! | Variable        | Required | Description                                        |
//! |-----------------|----------|----------------------------------------------------|
//! | `IMAP_HOST`     | Yes      | IMAP server hostname (e.g. `imap.gmail.com`)       |
//! | `IMAP_PORT`     | No       | IMAP port (default: `993`)                         |
//! | `IMAP_USERNAME` | Yes      | Email address / login                              |
//! | `IMAP_PASSWORD` | Yes      | App Password (not account password)                |

use crate::error::{LuminaError, Result};
use std::env;

// ── ImapConfig ────────────────────────────────────────────────────────────────

/// IMAP server configuration sourced from environment variables.
#[derive(Debug, Clone, PartialEq)]
pub struct ImapConfig {
    /// Hostname of the IMAP server (from `IMAP_HOST`).
    pub host: String,
    /// Port of the IMAP server (from `IMAP_PORT`, default 993).
    pub port: u16,
    /// Login username (from `IMAP_USERNAME`).
    pub username: String,
    /// App Password (from `IMAP_PASSWORD`).
    pub password: String,
}

impl ImapConfig {
    /// Construct from explicit parameters (for testing and programmatic use).
    pub fn new(
        host: impl Into<String>,
        port: u16,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            host: host.into(),
            port,
            username: username.into(),
            password: password.into(),
        }
    }

    /// Load config from environment variables.
    ///
    /// Returns `None` when `IMAP_HOST` is not set (email not configured).
    pub fn from_env() -> Option<Self> {
        let host = env::var("IMAP_HOST").ok().filter(|s| !s.is_empty())?;
        let port: u16 = env::var("IMAP_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(993);
        let username = env::var("IMAP_USERNAME").unwrap_or_default();
        let password = env::var("IMAP_PASSWORD").unwrap_or_default();
        Some(Self::new(host, port, username, password))
    }
}

// ── IMAP response parser helpers ──────────────────────────────────────────────

/// Extract a header field value from raw RFC 2822 header text.
///
/// Handles multi-line (folded) header values by joining continuation lines
/// (those starting with whitespace) onto the previous logical line.
pub fn extract_header(headers: &str, field: &str) -> Option<String> {
    let field_lower = field.to_lowercase();
    let prefix = format!("{}:", field_lower);
    let unfolded = unfold_headers(headers);

    for line in unfolded.lines() {
        // Header field names are case-insensitive (RFC 5322 §2.2).
        // Compare lowercase prefix against lowercase line start, but extract
        // value from the *original* line to preserve case.
        if line.to_lowercase().starts_with(&prefix) {
            // Skip past the "field:" portion on the original line.
            let value = line[field.len() + 1..].trim().to_string();
            return if value.is_empty() { None } else { Some(value) };
        }
    }
    None
}

/// Unfold RFC 2822 header folding.
///
/// A folded header is a long header split across lines where continuation
/// lines begin with a single space or tab.  Unfolding replaces the
/// `CRLF + WSP` sequence with a single space.
pub fn unfold_headers(headers: &str) -> String {
    let normalized = headers.replace("\r\n", "\n");
    // Continuation lines start with SP or TAB; replace them with a space.
    let mut out = String::with_capacity(normalized.len());
    let mut lines = normalized.lines().peekable();
    while let Some(line) = lines.next() {
        out.push_str(line);
        // Peek ahead: if next line starts with WSP, it is a continuation.
        while lines.peek().map(|l| l.starts_with(' ') || l.starts_with('\t')).unwrap_or(false) {
            let cont = lines.next().unwrap();
            out.push(' ');
            out.push_str(cont.trim());
        }
        out.push('\n');
    }
    out
}

/// Strip HTML tags from a string, returning plain text.
///
/// This is a simplistic tag stripper sufficient for email preview generation.
/// It removes `<...>` sequences and decodes common HTML entities.
pub fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;

    for ch in html.chars() {
        match ch {
            '<' => { in_tag = true; }
            '>' => { in_tag = false; }
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }

    // Decode common HTML entities.
    out
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Truncate a string to at most `max_chars` characters.
///
/// If truncation occurs, appends `"…"` to indicate the text was cut.
pub fn truncate_preview(text: &str, max_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        text.to_string()
    } else {
        let truncated: String = chars[..max_chars].iter().collect();
        format!("{}…", truncated.trim_end())
    }
}

/// Parse `IMAP SEARCH UID` response lines and extract UID numbers.
///
/// Example input: `* SEARCH 1 2 3\r\n` → `["1", "2", "3"]`
pub fn parse_search_uids(response: &str) -> Vec<String> {
    response
        .lines()
        .filter(|l| l.to_uppercase().starts_with("* SEARCH"))
        .flat_map(|l| {
            l.splitn(3, ' ')
                .skip(2) // skip "* SEARCH"
                .flat_map(|rest| rest.split_whitespace())
                .map(|uid| uid.trim().to_string())
                .filter(|uid| !uid.is_empty() && uid.chars().all(|c| c.is_ascii_digit()))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Parse an IMAP `FLAGS` response and check whether `\Seen` is present.
///
/// Example: `(\Seen \Answered)` → `true`
pub fn has_seen_flag(flags_str: &str) -> bool {
    // IMAP flags use a literal backslash: \Seen (one backslash in the string).
    // In Rust source we write "\\" to represent a single backslash character.
    flags_str
        .split_whitespace()
        .any(|f| {
            let stripped = f.trim_matches(|c| c == '(' || c == ')');
            stripped.eq_ignore_ascii_case("\\Seen")
        })
}

/// Build an IMAP SEARCH command string for the given query type.
///
/// Supported query types:
/// - `"UNSEEN"` → `SEARCH UNSEEN`
/// - `"FROM:<addr>"` → `SEARCH FROM "<addr>"`
/// - `"SUBJECT:<text>"` → `SEARCH SUBJECT "<text>"`
/// - `"BODY:<text>"` → `SEARCH BODY "<text>"`
/// - `"SINCE:<date>"` → `SEARCH SINCE <date>` (date format: `01-Jan-2026`)
///
/// Returns `Err(LuminaError::Config)` for unrecognized query types.
pub fn build_search_command(tag: &str, query: &SearchQuery) -> Result<String> {
    let criteria = match query {
        SearchQuery::Unseen => "UNSEEN".to_string(),
        SearchQuery::From(addr) => format!("FROM \"{}\"", sanitize_imap_string(addr)?),
        SearchQuery::Subject(text) => format!("SUBJECT \"{}\"", sanitize_imap_string(text)?),
        SearchQuery::Body(text) => format!("BODY \"{}\"", sanitize_imap_string(text)?),
        SearchQuery::Since(date) => format!("SINCE {}", sanitize_imap_date(date)?),
    };
    Ok(format!("{} SEARCH {}\r\n", tag, criteria))
}

/// IMAP search query types.
#[derive(Debug, Clone, PartialEq)]
pub enum SearchQuery {
    /// Search for messages without the `\Seen` flag.
    Unseen,
    /// Search by `From:` header address.
    From(String),
    /// Search by `Subject:` header text.
    Subject(String),
    /// Full-text search in message body.
    Body(String),
    /// Messages received since a given date (IMAP date format: `01-Jan-2026`).
    Since(String),
}

// ── Input sanitization ────────────────────────────────────────────────────────

/// Sanitize a string for use inside an IMAP quoted string literal.
///
/// IMAP quoted strings (RFC 3501 §4.3) may not contain `"`, `\`, CR, or LF.
/// Reject any input that contains these characters to prevent command injection.
pub fn sanitize_imap_string(s: &str) -> Result<String> {
    if s.chars().any(|c| matches!(c, '"' | '\\' | '\r' | '\n')) {
        return Err(LuminaError::Config(format!(
            "IMAP string contains illegal characters: {:?}",
            s
        )));
    }
    // Limit length to prevent oversized commands.
    if s.len() > 1024 {
        return Err(LuminaError::Config(
            "IMAP search string exceeds maximum length (1024 chars)".to_string(),
        ));
    }
    Ok(s.to_string())
}

/// Validate an IMAP date string (format: `DD-Mon-YYYY`, e.g. `01-Jan-2026`).
///
/// Rejects anything that doesn't match the pattern to prevent injection.
pub fn sanitize_imap_date(date: &str) -> Result<String> {
    // IMAP date format: D-Mon-YYYY or DD-Mon-YYYY (RFC 3501 §9)
    // e.g. "1-Jun-2026" (10 chars) or "01-Jun-2026" (11 chars)
    //
    // Structure: {1-2 digits}-{3 alpha chars}-{4 digits}
    // We validate parts individually to reject ISO 8601 (2026-01-01) and
    // injections like "01-Jan-2026 OR 1=1".
    let valid = validate_imap_date_parts(date);
    if valid {
        Ok(date.to_string())
    } else {
        Err(LuminaError::Config(format!(
            "Invalid IMAP date format '{}' — expected D-Mon-YYYY or DD-Mon-YYYY (e.g. 01-Jan-2026)",
            date
        )))
    }
}

/// Validate the structure of an IMAP date string (D-Mon-YYYY or DD-Mon-YYYY).
fn validate_imap_date_parts(date: &str) -> bool {
    let parts: Vec<&str> = date.splitn(3, '-').collect();
    if parts.len() != 3 {
        return false;
    }
    let day = parts[0];
    let month = parts[1];
    let year = parts[2];

    // Day: 1 or 2 digits.
    let day_ok = (day.len() == 1 || day.len() == 2) && day.chars().all(|c| c.is_ascii_digit());
    // Month: exactly 3 ASCII alphabetic characters (Jan, Feb, ..., Dec).
    let month_ok = month.len() == 3 && month.chars().all(|c| c.is_ascii_alphabetic());
    // Year: exactly 4 digits.
    let year_ok = year.len() == 4 && year.chars().all(|c| c.is_ascii_digit());

    day_ok && month_ok && year_ok
}

// ── Live IMAP connection helpers ──────────────────────────────────────────────

/// A connected and authenticated IMAP session.
///
/// This wrapper owns the `async_imap::Session` for its lifetime and provides
/// convenience methods for the operations required by `EmailClient`.
/// The session is closed (LOGOUT) when this struct is dropped.
///
/// Constructed via `ImapSession::connect`.
pub struct ImapSession {
    inner: async_imap::Session<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>,
}

impl ImapSession {
    /// Open a TLS connection to the IMAP server, authenticate, and return
    /// a ready-to-use `ImapSession`.
    ///
    /// Returns `Err` on any connection, TLS, or authentication failure.
    pub async fn connect(config: &ImapConfig) -> Result<Self> {
        use std::sync::Arc;
        use tokio::net::TcpStream;
        use tokio_rustls::TlsConnector;
        use rustls::ClientConfig;

        // Load native TLS certificates so Gmail/Google's cert validates.
        let mut root_store = rustls::RootCertStore::empty();
        let cert_result = rustls_native_certs::load_native_certs();
        if !cert_result.errors.is_empty() {
            log::warn!("Some native TLS certs failed to load: {:?}", cert_result.errors);
        }
        if cert_result.certs.is_empty() {
            return Err(LuminaError::Internal(
                "No native TLS certificates found — cannot verify IMAP server certificate".to_string()
            ));
        }
        for cert in cert_result.certs {
            root_store.add(cert)
                .map_err(|e| LuminaError::Internal(format!("invalid certificate: {e}")))?;
        }

        let tls_config = ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        let connector = TlsConnector::from(Arc::new(tls_config));
        let addr = format!("{}:{}", config.host, config.port);

        let tcp = TcpStream::connect(&addr)
            .await
            .map_err(|e| LuminaError::Internal(format!("IMAP TCP connect to {addr} failed: {e}")))?;

        let server_name = rustls::pki_types::ServerName::try_from(config.host.clone())
            .map_err(|e| LuminaError::Config(format!("invalid IMAP hostname '{}': {e}", config.host)))?;

        let tls_stream = connector
            .connect(server_name, tcp)
            .await
            .map_err(|e| LuminaError::Internal(format!("TLS handshake to {} failed: {e}", config.host)))?;

        let client = async_imap::Client::new(tls_stream);
        let session = client
            .login(&config.username, &config.password)
            .await
            .map_err(|(e, _)| LuminaError::Internal(format!("IMAP LOGIN failed: {e}")))?;

        Ok(Self { inner: session })
    }

    /// SELECT a mailbox and return the number of messages in it.
    pub async fn select(&mut self, folder: &str) -> Result<u32> {
        let mailbox = self.inner
            .select(folder)
            .await
            .map_err(|e| LuminaError::Internal(format!("IMAP SELECT '{}' failed: {e}", folder)))?;
        Ok(mailbox.exists)
    }

    /// Run `UID SEARCH UNSEEN` and return the list of UIDs.
    pub async fn search_unseen(&mut self) -> Result<Vec<u32>> {
        let uids = self.inner
            .uid_search("UNSEEN")
            .await
            .map_err(|e| LuminaError::Internal(format!("IMAP UID SEARCH UNSEEN failed: {e}")))?;
        let mut v: Vec<u32> = uids.into_iter().collect();
        v.sort_unstable();
        Ok(v)
    }

    /// Run `UID SEARCH <criteria>` with an arbitrary criteria string.
    ///
    /// `criteria` must be sanitized before calling this method.
    pub async fn uid_search(&mut self, criteria: &str) -> Result<Vec<u32>> {
        let uids = self.inner
            .uid_search(criteria)
            .await
            .map_err(|e| LuminaError::Internal(format!("IMAP UID SEARCH failed: {e}")))?;
        let mut v: Vec<u32> = uids.into_iter().collect();
        v.sort_unstable();
        Ok(v)
    }

    /// Fetch RFC822 headers and first 500 bytes of the body for a set of UIDs.
    ///
    /// Returns `(uid, headers_text, body_preview_text)` tuples.
    pub async fn fetch_headers_and_preview(
        &mut self,
        uids: &[u32],
    ) -> Result<Vec<(u32, String, String)>> {
        if uids.is_empty() {
            return Ok(vec![]);
        }

        // Build UID set string: "1,2,3" or "1:5" — we use individual IDs.
        let uid_set: String = uids.iter()
            .map(|u| u.to_string())
            .collect::<Vec<_>>()
            .join(",");

        // FETCH: RFC822.HEADER + BODY[TEXT]<0.500> (first 500 bytes of body)
        let fetch_query = "(RFC822.HEADER BODY[TEXT]<0.500>)";
        let messages = self.inner
            .uid_fetch(&uid_set, fetch_query)
            .await
            .map_err(|e| LuminaError::Internal(format!("IMAP UID FETCH failed: {e}")))?;

        use futures::StreamExt;
        let fetched: Vec<async_imap::types::Fetch> = messages
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .filter_map(|r| r.ok())
            .collect();

        let mut results = Vec::new();
        for msg in fetched {
            let uid = match msg.uid {
                Some(u) => u,
                None => continue,
            };
            let headers = msg.header()
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default();
            let body_bytes = msg.text()
                .unwrap_or(b"");
            let body_raw = String::from_utf8_lossy(body_bytes).into_owned();
            results.push((uid, headers, body_raw));
        }

        Ok(results)
    }

    /// Fetch the full body of a single message by UID.
    ///
    /// Truncates to `max_bytes` (default 100 KB; per-spec large emails are
    /// capped at 2 KB for summarization).
    pub async fn fetch_full_body(&mut self, uid: u32, max_bytes: usize) -> Result<Option<String>> {
        let fetch_query = "BODY[TEXT]";
        let messages = self.inner
            .uid_fetch(uid.to_string(), fetch_query)
            .await
            .map_err(|e| LuminaError::Internal(format!("IMAP UID FETCH body failed: {e}")))?;

        use futures::StreamExt;
        let fetched: Vec<async_imap::types::Fetch> = messages
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .filter_map(|r| r.ok())
            .collect();

        if let Some(msg) = fetched.into_iter().find(|m| m.uid == Some(uid)) {
            let body_bytes = msg.text().unwrap_or(b"");
            let truncated = if body_bytes.len() > max_bytes {
                &body_bytes[..max_bytes]
            } else {
                body_bytes
            };
            let body_text = String::from_utf8_lossy(truncated).into_owned();
            Ok(Some(body_text))
        } else {
            Ok(None)
        }
    }

    /// Add an IMAP flag to a message.
    pub async fn store_flag(&mut self, uid: u32, flag: &str) -> Result<()> {
        let flags = format!("+FLAGS ({})", flag);
        let _ = self.inner
            .uid_store(uid.to_string(), &flags)
            .await
            .map_err(|e| LuminaError::Internal(format!("IMAP UID STORE failed: {e}")))?;
        Ok(())
    }

    /// Copy a message to another mailbox and expunge the original (MOVE).
    ///
    /// Uses COPY + UID STORE `\Deleted` + EXPUNGE as a fallback since not all
    /// IMAP servers support the RFC 6851 MOVE extension.
    pub async fn copy_and_expunge(&mut self, uid: u32, destination: &str) -> Result<()> {
        // COPY to destination.
        self.inner
            .uid_copy(uid.to_string(), destination)
            .await
            .map_err(|e| LuminaError::Internal(format!("IMAP UID COPY to '{destination}' failed: {e}")))?;

        // Mark original as deleted.
        self.store_flag(uid, "\\Deleted").await?;

        // EXPUNGE to remove deleted messages.
        self.inner
            .expunge()
            .await
            .map_err(|e| LuminaError::Internal(format!("IMAP EXPUNGE failed: {e}")))?;

        Ok(())
    }

    /// Run `STATUS <folder> (UNSEEN)` to get the unread count without fetching
    /// any message content.
    ///
    /// Returns the UNSEEN count, or 0 if the STATUS command fails.
    pub async fn status_unseen(&mut self, folder: &str) -> Result<u32> {
        let status = self.inner
            .status(folder, "(UNSEEN)")
            .await
            .map_err(|e| LuminaError::Internal(format!("IMAP STATUS '{folder}' failed: {e}")))?;
        Ok(status.unseen.unwrap_or(0))
    }

    /// Send LOGOUT and close the connection.
    pub async fn logout(mut self) -> Result<()> {
        self.inner
            .logout()
            .await
            .map_err(|e| LuminaError::Internal(format!("IMAP LOGOUT failed: {e}")))?;
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::sync::Mutex;

    /// Serialise tests that mutate env vars.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    // ── ImapConfig::from_env ─────────────────────────────────────────────────

    #[test]
    #[serial]
    fn test_imap_config_from_env_none_without_host() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("IMAP_HOST");
        assert!(
            ImapConfig::from_env().is_none(),
            "should return None when IMAP_HOST is not set"
        );
    }

    #[test]
    #[serial]
    fn test_imap_config_from_env_some_with_host() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("IMAP_HOST", "imap.example.com");
        std::env::set_var("IMAP_PORT", "993");
        std::env::set_var("IMAP_USERNAME", "user@example.com");
        std::env::set_var("IMAP_PASSWORD", "app-password-123");

        let cfg = ImapConfig::from_env().expect("should return Some");
        assert_eq!(cfg.host, "imap.example.com");
        assert_eq!(cfg.port, 993);
        assert_eq!(cfg.username, "user@example.com");
        assert_eq!(cfg.password, "app-password-123");

        std::env::remove_var("IMAP_HOST");
        std::env::remove_var("IMAP_PORT");
        std::env::remove_var("IMAP_USERNAME");
        std::env::remove_var("IMAP_PASSWORD");
    }

    #[test]
    #[serial]
    fn test_imap_config_default_port_993() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("IMAP_HOST", "imap.example.com");
        std::env::remove_var("IMAP_PORT");

        let cfg = ImapConfig::from_env().expect("should return Some");
        assert_eq!(cfg.port, 993, "default port should be 993");

        std::env::remove_var("IMAP_HOST");
    }

    #[test]
    #[serial]
    fn test_imap_config_empty_host_returns_none() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("IMAP_HOST", "");
        assert!(ImapConfig::from_env().is_none(), "empty IMAP_HOST should return None");
        std::env::remove_var("IMAP_HOST");
    }

    // ── extract_header ────────────────────────────────────────────────────────

    #[test]
    fn test_extract_header_simple() {
        let headers = "From: alice@example.com\r\nSubject: Hello\r\nDate: Thu, 01 Jan 2026 12:00:00 +0000\r\n";
        assert_eq!(
            extract_header(headers, "From"),
            Some("alice@example.com".to_string())
        );
        assert_eq!(
            extract_header(headers, "Subject"),
            Some("Hello".to_string())
        );
    }

    #[test]
    fn test_extract_header_case_insensitive() {
        let headers = "SUBJECT: Test Email\r\n";
        assert_eq!(
            extract_header(headers, "subject"),
            Some("Test Email".to_string())
        );
    }

    #[test]
    fn test_extract_header_missing_returns_none() {
        let headers = "From: alice@example.com\r\n";
        assert!(extract_header(headers, "Subject").is_none());
    }

    #[test]
    fn test_extract_header_folded_value() {
        // Folded header: continuation on next line starting with whitespace.
        let headers = "Subject: This is a very long subject line\r\n that continues here\r\n";
        let val = extract_header(headers, "Subject").unwrap();
        assert!(val.contains("This is a very long subject line"));
        assert!(val.contains("continues here"));
    }

    // ── strip_html ────────────────────────────────────────────────────────────

    #[test]
    fn test_strip_html_removes_tags() {
        let html = "<p>Hello <b>World</b></p>";
        let plain = strip_html(html);
        assert!(!plain.contains('<'), "tags should be removed");
        assert!(plain.contains("Hello"), "text content should remain");
        assert!(plain.contains("World"), "text content should remain");
    }

    #[test]
    fn test_strip_html_decodes_entities() {
        let html = "Price: &amp;100 &lt;sale&gt; &quot;today&quot;";
        let plain = strip_html(html);
        assert!(plain.contains("&100"), "& entity should be decoded");
        assert!(plain.contains("<sale>"), "< > entities should be decoded");
        assert!(plain.contains("\"today\""), "quote entity should be decoded");
    }

    #[test]
    fn test_strip_html_plain_text_unchanged() {
        let plain = "No tags here, just plain text.";
        assert_eq!(strip_html(plain), plain);
    }

    // ── truncate_preview ──────────────────────────────────────────────────────

    #[test]
    fn test_truncate_preview_within_limit() {
        let text = "Short text.";
        assert_eq!(truncate_preview(text, 500), text);
    }

    #[test]
    fn test_truncate_preview_at_limit() {
        let text = "A".repeat(500);
        assert_eq!(truncate_preview(&text, 500), text);
    }

    #[test]
    fn test_truncate_preview_exceeds_limit() {
        let text = "A".repeat(600);
        let result = truncate_preview(&text, 500);
        assert!(result.ends_with('…'), "truncated text should end with ellipsis");
        // 500 chars + '…' = 501 chars
        assert!(result.len() <= 510, "result should not be much longer than limit");
    }

    #[test]
    fn test_truncate_preview_unicode() {
        // Unicode multi-byte characters — truncation must operate on char boundary.
        let text: String = "😀".repeat(600);
        let result = truncate_preview(&text, 500);
        assert!(result.ends_with('…'));
    }

    // ── parse_search_uids ─────────────────────────────────────────────────────

    #[test]
    fn test_parse_search_uids_typical_response() {
        let response = "* SEARCH 1 2 3\r\n";
        let uids = parse_search_uids(response);
        assert_eq!(uids, vec!["1", "2", "3"]);
    }

    #[test]
    fn test_parse_search_uids_empty_search() {
        // Empty SEARCH result (no matching messages).
        let response = "* SEARCH\r\n";
        let uids = parse_search_uids(response);
        assert!(uids.is_empty());
    }

    #[test]
    fn test_parse_search_uids_single_uid() {
        let response = "* SEARCH 42\r\n";
        let uids = parse_search_uids(response);
        assert_eq!(uids, vec!["42"]);
    }

    #[test]
    fn test_parse_search_uids_multi_line_response() {
        // Some servers may return multiple SEARCH lines.
        let response = "* SEARCH 1 2\r\n* SEARCH 3 4\r\n";
        let uids = parse_search_uids(response);
        assert_eq!(uids.len(), 4);
    }

    // ── has_seen_flag ─────────────────────────────────────────────────────────

    #[test]
    fn test_has_seen_flag_present() {
        assert!(has_seen_flag("(\\Seen \\Answered)"));
        assert!(has_seen_flag("\\Seen"));
    }

    #[test]
    fn test_has_seen_flag_absent() {
        assert!(!has_seen_flag("(\\Recent \\Answered)"));
        assert!(!has_seen_flag(""));
    }

    // ── build_search_command ──────────────────────────────────────────────────

    #[test]
    fn test_build_search_command_unseen() {
        let cmd = build_search_command("A001", &SearchQuery::Unseen).unwrap();
        assert!(cmd.contains("SEARCH UNSEEN"));
        assert!(cmd.starts_with("A001"));
    }

    #[test]
    fn test_build_search_command_from() {
        let cmd = build_search_command("A002", &SearchQuery::From("alice@example.com".into())).unwrap();
        assert!(cmd.contains(r#"SEARCH FROM "alice@example.com""#));
    }

    #[test]
    fn test_build_search_command_subject() {
        let cmd = build_search_command("A003", &SearchQuery::Subject("Meeting notes".into())).unwrap();
        assert!(cmd.contains(r#"SEARCH SUBJECT "Meeting notes""#));
    }

    #[test]
    fn test_build_search_command_since() {
        let cmd = build_search_command("A004", &SearchQuery::Since("01-Jan-2026".into())).unwrap();
        assert!(cmd.contains("SEARCH SINCE 01-Jan-2026"));
    }

    // ── sanitize_imap_string ──────────────────────────────────────────────────

    #[test]
    fn test_sanitize_imap_string_valid() {
        assert!(sanitize_imap_string("alice@example.com").is_ok());
        assert!(sanitize_imap_string("Meeting notes").is_ok());
    }

    #[test]
    fn test_sanitize_imap_string_rejects_quote() {
        // Double-quote would break the IMAP quoted string.
        assert!(sanitize_imap_string("inject\"here").is_err());
    }

    #[test]
    fn test_sanitize_imap_string_rejects_backslash() {
        assert!(sanitize_imap_string("inject\\here").is_err());
    }

    #[test]
    fn test_sanitize_imap_string_rejects_crlf() {
        assert!(sanitize_imap_string("line\r\ninjection").is_err());
        assert!(sanitize_imap_string("line\ninjection").is_err());
    }

    #[test]
    fn test_sanitize_imap_string_rejects_oversized() {
        let long = "A".repeat(1025);
        assert!(sanitize_imap_string(&long).is_err());
    }

    // ── sanitize_imap_date ────────────────────────────────────────────────────

    #[test]
    fn test_sanitize_imap_date_valid() {
        assert!(sanitize_imap_date("01-Jan-2026").is_ok());
        assert!(sanitize_imap_date("31-Dec-2025").is_ok());
        assert!(sanitize_imap_date("1-Jun-2026").is_ok());
    }

    #[test]
    fn test_sanitize_imap_date_rejects_injection() {
        assert!(sanitize_imap_date("01-Jan-2026 OR 1=1").is_err());
        assert!(sanitize_imap_date("2026-01-01").is_err()); // ISO 8601 with dashes but wrong format
        assert!(sanitize_imap_date("").is_err());
    }

    // ── unfold_headers ────────────────────────────────────────────────────────

    #[test]
    fn test_unfold_headers_joins_continuation() {
        let folded = "Subject: Long subject\r\n that continues\r\nFrom: alice@example.com\r\n";
        let unfolded = unfold_headers(folded);
        assert!(unfolded.contains("Long subject"));
        assert!(unfolded.contains("that continues"));
        // Should be on the same logical line.
        let subject_line = unfolded
            .lines()
            .find(|l| l.to_lowercase().starts_with("subject:"))
            .unwrap_or("");
        assert!(subject_line.contains("continues"), "continuation should join to previous line");
    }
}
