//! P2-07: Calendar CalDAV client
//!
//! Connects to a CalDAV server (e.g. Google Calendar via CalDAV protocol) using
//! HTTP Basic authentication with an App Password.  No OAuth, no token refresh.
//!
//! ## Configuration (env vars only — no hardcoded values)
//!
//! | Variable           | Required | Description                                   |
//! |--------------------|----------|-----------------------------------------------|
//! | `CALDAV_URL`       | Yes      | CalDAV collection URL                         |
//! | `CALDAV_USERNAME`  | Yes      | Username / email for Basic auth               |
//! | `CALDAV_PASSWORD`  | Yes      | App Password for Basic auth                   |
//!
//! ## Quick start
//!
//! ```rust,no_run
//! # tokio_test::block_on(async {
//! use lumina_core::caldav::CalDavClient;
//!
//! if let Some(client) = CalDavClient::from_env() {
//!     let events = client.fetch_events("20260601T000000Z", "20260630T235959Z").await.unwrap();
//!     for ev in &events {
//!         println!("{} — {}", ev.dtstart, ev.summary);
//!     }
//! }
//! # })
//! ```

use crate::error::{LuminaError, Result};
use std::env;

// ── CalendarEvent ────────────────────────────────────────────────────────────

/// A single calendar event parsed from an iCalendar VEVENT block.
#[derive(Debug, Clone, PartialEq)]
pub struct CalendarEvent {
    /// Unique identifier (`UID` property).
    pub uid: String,
    /// Event title (`SUMMARY` property).
    pub summary: String,
    /// Start date/time string (`DTSTART` property value, may include timezone).
    pub dtstart: String,
    /// End date/time string (`DTEND` property value, may include timezone).
    pub dtend: String,
    /// Optional description (`DESCRIPTION` property).
    pub description: Option<String>,
    /// Optional location (`LOCATION` property).
    pub location: Option<String>,
}

// ── CalDavClient ─────────────────────────────────────────────────────────────

/// CalDAV HTTP client.
///
/// Credentials and the server URL come exclusively from environment variables
/// — no hardcoded hosts, IPs, or credentials.
#[derive(Debug, Clone)]
pub struct CalDavClient {
    /// Base CalDAV collection URL (from `CALDAV_URL`).
    base_url: String,
    /// Basic-auth username (from `CALDAV_USERNAME`).
    username: String,
    /// Basic-auth password / App Password (from `CALDAV_PASSWORD`).
    password: String,
    /// Underlying HTTP client.
    http: reqwest::Client,
}

impl CalDavClient {
    /// Create a new client from explicit parameters (for testing).
    pub fn new(base_url: impl Into<String>, username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            username: username.into(),
            password: password.into(),
            http: reqwest::Client::new(),
        }
    }

    /// Create a client from environment variables.
    ///
    /// Returns `None` when `CALDAV_URL` is not set (calendar not configured).
    /// `CALDAV_USERNAME` and `CALDAV_PASSWORD` default to empty string if absent,
    /// which will produce a 401 from the server rather than a panic here.
    pub fn from_env() -> Option<Self> {
        let base_url = env::var("CALDAV_URL").ok().filter(|s| !s.is_empty())?;
        let username = env::var("CALDAV_USERNAME").unwrap_or_default();
        let password = env::var("CALDAV_PASSWORD").unwrap_or_default();
        Some(Self::new(base_url, username, password))
    }

    /// Fetch calendar events in the given UTC date/time range.
    ///
    /// `start` and `end` are iCalendar date/time strings, e.g.
    /// `"20260601T000000Z"` (UTC) or `"20260601"` (date-only).
    ///
    /// Uses a CalDAV `calendar-query` REPORT request.  On server errors or
    /// connection failures the method logs a warning and returns `Ok(vec![])`
    /// so callers (e.g. Vigil) degrade gracefully.
    pub async fn fetch_events(&self, start: &str, end: &str) -> Result<Vec<CalendarEvent>> {
        // Validate date-time strings before embedding in XML to prevent injection.
        validate_caldav_datetime(start)?;
        validate_caldav_datetime(end)?;

        let body = build_report_body(start, end);

        let response = self.http
            .request(
                reqwest::Method::from_bytes(b"REPORT").map_err(|e| {
                    LuminaError::Internal(format!("Invalid HTTP method: {}", e))
                })?,
                &self.base_url,
            )
            .basic_auth(&self.username, Some(&self.password))
            .header("Content-Type", "application/xml; charset=utf-8")
            .header("Depth", "1")
            .body(body)
            .send()
            .await;

        match response {
            Err(e) => {
                log::warn!("CalDAV: request failed — {}", e);
                Ok(vec![])
            }
            Ok(resp) => {
                let status = resp.status();
                if !status.is_success() {
                    log::warn!("CalDAV: server returned {} — returning empty event list", status);
                    return Ok(vec![]);
                }
                let text = resp.text().await.unwrap_or_default();
                Ok(parse_ical(&text))
            }
        }
    }
}

// ── Input validation ─────────────────────────────────────────────────────────

/// Validate a CalDAV date/time string to prevent XML injection.
///
/// Accepted formats (RFC 5545 §3.3.4 / §3.3.5 / CalDAV RFC 4791 §9.9):
/// - Date-only: `YYYYMMDD` (8 digits)
/// - Date-time UTC: `YYYYMMDDTHHMMSSZ` (16 chars, ends with Z)
/// - Date-time local: `YYYYMMDDTHHMMSS` (15 chars)
fn validate_caldav_datetime(value: &str) -> Result<()> {
    let valid = match value.len() {
        8  => value.bytes().all(|b| b.is_ascii_digit()),
        15 => value[..8].bytes().all(|b| b.is_ascii_digit())
            && value.as_bytes()[8] == b'T'
            && value[9..].bytes().all(|b| b.is_ascii_digit()),
        16 => value[..8].bytes().all(|b| b.is_ascii_digit())
            && value.as_bytes()[8] == b'T'
            && value[9..15].bytes().all(|b| b.is_ascii_digit())
            && value.as_bytes()[15] == b'Z',
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(LuminaError::Config(format!(
            "invalid CalDAV date/time value '{}' — must be YYYYMMDD, YYYYMMDDTHHMMSS, or YYYYMMDDTHHMMSSZ",
            value
        )))
    }
}

// ── REPORT request body ───────────────────────────────────────────────────────

/// Build a CalDAV `calendar-query` REPORT XML body for the given date range.
fn build_report_body(start: &str, end: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<C:calendar-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:prop>
    <D:getetag/>
    <C:calendar-data/>
  </D:prop>
  <C:filter>
    <C:comp-filter name="VCALENDAR">
      <C:comp-filter name="VEVENT">
        <C:time-range start="{}" end="{}"/>
      </C:comp-filter>
    </C:comp-filter>
  </C:filter>
</C:calendar-query>"#,
        start, end
    )
}

// ── iCalendar parser ─────────────────────────────────────────────────────────

/// Minimal iCalendar parser.
///
/// Scans `body` for `BEGIN:VEVENT`…`END:VEVENT` blocks and extracts
/// `UID`, `SUMMARY`, `DTSTART`, `DTEND`, `DESCRIPTION`, and `LOCATION`.
///
/// Property values may appear as bare `KEY:value` lines or as typed/param
/// variants like `DTSTART;TZID=America/Los_Angeles:20260601T090000`.  The
/// parser strips everything up to and including the first `:` to obtain the
/// raw value.
///
/// Multi-line (folded) iCal values — where continuation lines start with a
/// single space or tab — are unfolded before extraction.
pub fn parse_ical(body: &str) -> Vec<CalendarEvent> {
    // Unfold folded lines (RFC 5545 §3.1): CRLF or LF followed by SPACE or TAB.
    let unfolded = unfold_ical(body);

    let mut events = Vec::new();

    for block in unfolded.split("BEGIN:VEVENT") {
        // Skip content before the first VEVENT
        if !block.contains("END:VEVENT") {
            continue;
        }

        // Trim everything after END:VEVENT
        let block = match block.split("END:VEVENT").next() {
            Some(b) => b,
            None => continue,
        };

        let uid = extract_property(block, "UID").unwrap_or_default();
        let summary = extract_property(block, "SUMMARY").unwrap_or_default();
        let dtstart = extract_property(block, "DTSTART").unwrap_or_default();
        let dtend = extract_property(block, "DTEND").unwrap_or_default();
        let description = extract_property(block, "DESCRIPTION");
        let location = extract_property(block, "LOCATION");

        // Skip VEVENT blocks that are missing the required fields.
        if uid.is_empty() && summary.is_empty() && dtstart.is_empty() {
            continue;
        }

        events.push(CalendarEvent {
            uid,
            summary,
            dtstart,
            dtend,
            description,
            location,
        });
    }

    events
}

/// Unfold RFC 5545 folded lines.
///
/// A folded line is a logical line split across multiple physical lines where
/// continuation lines begin with a single SPACE or TAB character.
fn unfold_ical(s: &str) -> String {
    // Normalize CRLF → LF, then remove fold continuation.
    let normalized = s.replace("\r\n", "\n");
    normalized
        .replace("\n ", "")
        .replace("\n\t", "")
}

/// Extract the value of a named iCalendar property from a VEVENT block.
///
/// Handles both bare `KEY:value` and parameterized `KEY;param=val:value` forms.
/// Returns `None` when the property is absent or its value is empty.
///
/// The returned value is unescaped per RFC 5545 §3.3.11 (TEXT type):
/// `\n`/`\N` → newline, `\,` → `,`, `\;` → `;`, `\\` → `\`.
fn extract_property(block: &str, key: &str) -> Option<String> {
    // Hoist key conversion above the loop — key is invariant over lines.
    let key_upper = key.to_uppercase();
    let colon_match = format!("{}:", key_upper);
    let semi_match  = format!("{};", key_upper);

    for line in block.lines() {
        let upper = line.to_uppercase();
        if upper.starts_with(&colon_match) || upper.starts_with(&semi_match) {
            // Value is everything after the first `:`.
            let raw = line.splitn(2, ':').nth(1).unwrap_or("").trim();
            let value = unescape_ical_text(raw);
            return if value.is_empty() { None } else { Some(value) };
        }
    }
    None
}

/// Unescape RFC 5545 §3.3.11 TEXT value escape sequences.
///
/// | Sequence | Result    |
/// |----------|-----------|
/// | `\n`     | newline   |
/// | `\N`     | newline   |
/// | `\,`     | `,`       |
/// | `\;`     | `;`       |
/// | `\\`     | `\`       |
fn unescape_ical_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some(',')            => out.push(','),
                Some(';')            => out.push(';'),
                Some('\\')           => out.push('\\'),
                Some(other)          => { out.push('\\'); out.push(other); }
                None                 => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use httpmock::prelude::*;
    use std::sync::Mutex;

    /// Serialize tests that mutate process-global env vars.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    // ── from_env ─────────────────────────────────────────────────────────────

    #[test]
    #[serial]
    fn test_from_env_none_without_caldav_url() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("CALDAV_URL");
        assert!(CalDavClient::from_env().is_none(), "should return None when CALDAV_URL is unset");
    }

    #[test]
    #[serial]
    fn test_from_env_some_with_caldav_url() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CALDAV_URL", "https://caldav.example.com/user/events/");
        std::env::set_var("CALDAV_USERNAME", "user@example.com");
        std::env::set_var("CALDAV_PASSWORD", "app-password");

        let client = CalDavClient::from_env();
        assert!(client.is_some(), "should return Some when CALDAV_URL is set");
        let c = client.unwrap();
        assert_eq!(c.base_url, "https://caldav.example.com/user/events/");
        assert_eq!(c.username, "user@example.com");
        assert_eq!(c.password, "app-password");

        std::env::remove_var("CALDAV_URL");
        std::env::remove_var("CALDAV_USERNAME");
        std::env::remove_var("CALDAV_PASSWORD");
    }

    #[test]
    #[serial]
    fn test_from_env_empty_url_returns_none() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("CALDAV_URL", "");
        assert!(CalDavClient::from_env().is_none(), "empty CALDAV_URL should return None");
        std::env::remove_var("CALDAV_URL");
    }

    // ── fetch_events — 207 Multi-Status ──────────────────────────────────────

    #[tokio::test]
    async fn test_fetch_events_returns_events_on_207() {
        let server = MockServer::start();

        let ical_body = concat!(
            "BEGIN:VCALENDAR\r\n",
            "BEGIN:VEVENT\r\n",
            "UID:event-uid-001@example.com\r\n",
            "SUMMARY:Team standup\r\n",
            "DTSTART:20260601T090000Z\r\n",
            "DTEND:20260601T093000Z\r\n",
            "DESCRIPTION:Daily sync\r\n",
            "LOCATION:Conference Room A\r\n",
            "END:VEVENT\r\n",
            "END:VCALENDAR\r\n",
        );

        // httpmock v0.7 does not support custom HTTP methods (REPORT, PROPFIND, etc.)
        // as `when.method()` constraints.  We match on path only — the method is
        // still sent as REPORT to the server; httpmock will match any inbound request
        // to that path regardless of method.
        let _mock = server.mock(|when, then| {
            when.path("/user/events/");
            then.status(207).body(ical_body);
        });

        let client = CalDavClient::new(
            format!("{}/user/events/", server.base_url()),
            "user@example.com",
            "app-password",
        );

        let events = client.fetch_events("20260601T000000Z", "20260601T235959Z").await.unwrap();
        assert_eq!(events.len(), 1, "should parse one event from 207 response");

        let ev = &events[0];
        assert_eq!(ev.uid, "event-uid-001@example.com");
        assert_eq!(ev.summary, "Team standup");
        assert_eq!(ev.dtstart, "20260601T090000Z");
        assert_eq!(ev.dtend, "20260601T093000Z");
        assert_eq!(ev.description.as_deref(), Some("Daily sync"));
        assert_eq!(ev.location.as_deref(), Some("Conference Room A"));
    }

    // ── fetch_events — server error → empty list ──────────────────────────────

    #[tokio::test]
    async fn test_fetch_events_returns_empty_on_500() {
        let server = MockServer::start();

        let _mock = server.mock(|when, then| {
            when.path("/user/events/");
            then.status(500).body("Internal Server Error");
        });

        let client = CalDavClient::new(
            format!("{}/user/events/", server.base_url()),
            "user@example.com",
            "app-password",
        );

        let events = client.fetch_events("20260601T000000Z", "20260601T235959Z").await.unwrap();
        assert!(events.is_empty(), "500 response should yield empty event list");
    }

    #[tokio::test]
    async fn test_fetch_events_returns_empty_on_401() {
        let server = MockServer::start();

        let _mock = server.mock(|when, then| {
            when.path("/user/events/");
            then.status(401).body("Unauthorized");
        });

        let client = CalDavClient::new(
            format!("{}/user/events/", server.base_url()),
            "user@example.com",
            "wrong-password",
        );

        let events = client.fetch_events("20260601T000000Z", "20260601T235959Z").await.unwrap();
        assert!(events.is_empty(), "401 response should yield empty event list");
    }

    // ── iCal parser — correct field extraction ────────────────────────────────

    #[test]
    fn test_parse_ical_extracts_correct_fields() {
        let ical = concat!(
            "BEGIN:VCALENDAR\r\n",
            "VERSION:2.0\r\n",
            "BEGIN:VEVENT\r\n",
            "UID:test-uid-42@example.com\r\n",
            "SUMMARY:Doctor appointment\r\n",
            "DTSTART:20260610T140000Z\r\n",
            "DTEND:20260610T150000Z\r\n",
            "DESCRIPTION:Annual checkup\r\n",
            "LOCATION:123 Medical Blvd\r\n",
            "END:VEVENT\r\n",
            "END:VCALENDAR\r\n",
        );

        let events = parse_ical(ical);
        assert_eq!(events.len(), 1);
        let ev = &events[0];
        assert_eq!(ev.uid, "test-uid-42@example.com");
        assert_eq!(ev.summary, "Doctor appointment");
        assert_eq!(ev.dtstart, "20260610T140000Z");
        assert_eq!(ev.dtend, "20260610T150000Z");
        assert_eq!(ev.description.as_deref(), Some("Annual checkup"));
        assert_eq!(ev.location.as_deref(), Some("123 Medical Blvd"));
    }

    #[test]
    fn test_parse_ical_optional_fields_absent() {
        let ical = concat!(
            "BEGIN:VEVENT\r\n",
            "UID:minimal-uid@example.com\r\n",
            "SUMMARY:Minimal event\r\n",
            "DTSTART:20260610T090000Z\r\n",
            "DTEND:20260610T100000Z\r\n",
            "END:VEVENT\r\n",
        );

        let events = parse_ical(ical);
        assert_eq!(events.len(), 1);
        assert!(events[0].description.is_none());
        assert!(events[0].location.is_none());
    }

    #[test]
    fn test_parse_ical_multiple_events() {
        let ical = concat!(
            "BEGIN:VCALENDAR\r\n",
            "BEGIN:VEVENT\r\n",
            "UID:ev1@example.com\r\n",
            "SUMMARY:First event\r\n",
            "DTSTART:20260601T090000Z\r\n",
            "DTEND:20260601T100000Z\r\n",
            "END:VEVENT\r\n",
            "BEGIN:VEVENT\r\n",
            "UID:ev2@example.com\r\n",
            "SUMMARY:Second event\r\n",
            "DTSTART:20260601T110000Z\r\n",
            "DTEND:20260601T120000Z\r\n",
            "END:VEVENT\r\n",
            "END:VCALENDAR\r\n",
        );

        let events = parse_ical(ical);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].uid, "ev1@example.com");
        assert_eq!(events[1].uid, "ev2@example.com");
    }

    #[test]
    fn test_parse_ical_empty_body() {
        assert!(parse_ical("").is_empty());
        assert!(parse_ical("BEGIN:VCALENDAR\nEND:VCALENDAR\n").is_empty());
    }

    #[test]
    fn test_parse_ical_parameterized_dtstart() {
        // DTSTART;TZID=America/Los_Angeles:20260601T090000
        let ical = concat!(
            "BEGIN:VEVENT\r\n",
            "UID:tz-event@example.com\r\n",
            "SUMMARY:Timezone event\r\n",
            "DTSTART;TZID=America/Los_Angeles:20260601T090000\r\n",
            "DTEND;TZID=America/Los_Angeles:20260601T100000\r\n",
            "END:VEVENT\r\n",
        );

        let events = parse_ical(ical);
        assert_eq!(events.len(), 1);
        // Value is everything after the first ':'.
        assert_eq!(events[0].dtstart, "20260601T090000");
        assert_eq!(events[0].dtend, "20260601T100000");
    }

    #[test]
    fn test_parse_ical_folded_lines_unfolded() {
        // A DESCRIPTION folded across two lines.
        // RFC 5545 §3.1: the continuation SP is the fold indicator and is dropped.
        // "descri\r\n ption" → "description" (SP removed, no inserted space).
        let ical = "BEGIN:VEVENT\r\nUID:fold-uid@example.com\r\nSUMMARY:Folded\r\nDTSTART:20260601T090000Z\r\nDTEND:20260601T100000Z\r\nDESCRIPTION:This is a long descri\r\n ption that is folded\r\nEND:VEVENT\r\n";

        let events = parse_ical(ical);
        assert_eq!(events.len(), 1);
        // The fold SP is stripped: "descri" + "ption" → "description"
        assert_eq!(
            events[0].description.as_deref(),
            Some("This is a long description that is folded")
        );
    }

    #[test]
    fn test_parse_ical_all_day_event() {
        // All-day events use DATE format for DTSTART/DTEND.
        let ical = concat!(
            "BEGIN:VEVENT\r\n",
            "UID:allday@example.com\r\n",
            "SUMMARY:All day event\r\n",
            "DTSTART;VALUE=DATE:20260615\r\n",
            "DTEND;VALUE=DATE:20260616\r\n",
            "END:VEVENT\r\n",
        );

        let events = parse_ical(ical);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].dtstart, "20260615");
        assert_eq!(events[0].dtend, "20260616");
    }

    // ── Helper function unit tests ────────────────────────────────────────────

    #[test]
    fn test_extract_property_bare() {
        let block = "UID:my-uid\nSUMMARY:My summary\n";
        assert_eq!(extract_property(block, "UID"), Some("my-uid".to_string()));
        assert_eq!(extract_property(block, "SUMMARY"), Some("My summary".to_string()));
    }

    #[test]
    fn test_extract_property_parameterized() {
        let block = "DTSTART;TZID=UTC:20260601T090000\n";
        assert_eq!(extract_property(block, "DTSTART"), Some("20260601T090000".to_string()));
    }

    #[test]
    fn test_extract_property_missing() {
        let block = "UID:some-uid\n";
        assert!(extract_property(block, "LOCATION").is_none());
    }

    #[test]
    fn test_extract_property_case_insensitive_key() {
        let block = "summary:lowercase key\n";
        // Key matching is case-insensitive.
        assert_eq!(extract_property(block, "SUMMARY"), Some("lowercase key".to_string()));
    }

    #[test]
    fn test_build_report_body_contains_range() {
        let body = build_report_body("20260601T000000Z", "20260630T235959Z");
        assert!(body.contains("20260601T000000Z"));
        assert!(body.contains("20260630T235959Z"));
        assert!(body.contains("calendar-query"));
        assert!(body.contains("VEVENT"));
    }

    #[test]
    fn test_unfold_ical_removes_continuation() {
        // RFC 5545 §3.1: CRLF followed by a single SP/TAB is a fold indicator.
        // The SP/TAB is part of the fold, NOT part of the value — it is dropped.
        // "Hello\r\n World" → "HelloWorld" (no space between the two halves).
        let folded = "DESCRIPTION:Hello\r\n World\r\nUID:abc\r\n";
        let unfolded = unfold_ical(folded);
        assert!(unfolded.contains("DESCRIPTION:HelloWorld"), "fold continuation should be removed");
        assert!(unfolded.contains("UID:abc"));
    }

    // ── validate_caldav_datetime ─────────────────────────────────────────────

    #[test]
    fn test_validate_caldav_datetime_valid_formats() {
        // Date-only
        assert!(validate_caldav_datetime("20260601").is_ok());
        // Date-time UTC
        assert!(validate_caldav_datetime("20260601T090000Z").is_ok());
        // Date-time local (no Z)
        assert!(validate_caldav_datetime("20260601T090000").is_ok());
    }

    #[test]
    fn test_validate_caldav_datetime_rejects_injection() {
        // XML injection attempts must be rejected.
        assert!(validate_caldav_datetime("/><evil/>").is_err());
        assert!(validate_caldav_datetime("20260601T000000Z&extra=1").is_err());
        assert!(validate_caldav_datetime("2026-06-01T00:00:00Z").is_err()); // ISO 8601 with dashes
        assert!(validate_caldav_datetime("").is_err());
        assert!(validate_caldav_datetime("not-a-date").is_err());
    }

    // ── unescape_ical_text ────────────────────────────────────────────────────

    #[test]
    fn test_unescape_ical_text_newline() {
        assert_eq!(unescape_ical_text(r"line1\nline2"), "line1\nline2");
        assert_eq!(unescape_ical_text(r"line1\Nline2"), "line1\nline2");
    }

    #[test]
    fn test_unescape_ical_text_comma_and_semicolon() {
        assert_eq!(unescape_ical_text(r"a\,b"), "a,b");
        assert_eq!(unescape_ical_text(r"a\;b"), "a;b");
    }

    #[test]
    fn test_unescape_ical_text_backslash() {
        assert_eq!(unescape_ical_text(r"a\\b"), r"a\b");
    }

    #[test]
    fn test_unescape_ical_text_no_escapes() {
        assert_eq!(unescape_ical_text("plain text"), "plain text");
    }

    #[test]
    fn test_unescape_ical_text_complex_description() {
        // Realistic Google Calendar description.
        let raw = r"Meeting notes\nAction items\, prioritized\nSee docs at https://example.com";
        let expected = "Meeting notes\nAction items, prioritized\nSee docs at https://example.com";
        assert_eq!(unescape_ical_text(raw), expected);
    }

    #[test]
    fn test_extract_property_unescapes_value() {
        // DESCRIPTION with \n and \, escape sequences.
        let block = r"DESCRIPTION:First line\nSecond line\, details";
        let val = extract_property(block, "DESCRIPTION").unwrap();
        assert!(val.contains('\n'), "\\n should be unescaped to newline");
        assert!(val.contains(", details"), "\\, should be unescaped to comma");
    }

    #[test]
    fn test_fetch_events_invalid_datetime_returns_error() {
        // Synchronous check: invalid datetime format should return Err immediately.
        let client = CalDavClient::new(
            "https://caldav.example.com/user/events/",
            "user@example.com",
            "app-password",
        );
        // We can't easily call async from sync test, so test the validator directly.
        assert!(validate_caldav_datetime("invalid").is_err());
        assert!(validate_caldav_datetime("/><script>").is_err());
    }
}
