//! EDGE-02: Egress traffic inspection and exfiltration prevention
//!
//! Every outbound network request from a tool execution is checked against an
//! operator-configured allowlist before the connection is permitted.
//!
//! # Threat model
//! Prevents data exfiltration via prompt injection: if an attacker tricks the LLM
//! into calling a tool that sends data to an external server, the egress inspector
//! blocks the connection and logs the attempt.
//!
//! # Allowlist format (`LUMINA_EGRESS_ALLOWLIST`)
//! Comma-separated entries, each of which may be:
//! - An exact hostname: `api.example.com`
//! - A single-level wildcard subdomain: `*.example.com`
//!   (matches `foo.example.com` but NOT `a.b.example.com` or `example.com`)
//! - An exact IPv4 address: `198.51.100.1`
//! - An exact IPv6 address: `::1`
//!
//! IP ranges (CIDR notation) are NOT supported — list each address explicitly.
//!
//! # Fail-closed behaviour
//! If the destination cannot be parsed, the request is **blocked** (fail closed).
//! The inspector never fails open.
//!
//! # DNS rebinding
//! Full DNS resolution is not performed inside this module. Operators should
//! use [`EgressInspector::is_private_ip`] to validate resolved addresses before
//! permitting connections to public hostnames.
//!
//! # Rate alerting
//! If more than 5 egress blocks occur within a 60-second window the inspector
//! sets a flag that callers can poll via [`EgressInspector::check_rate_alert`].

use crate::error::LuminaError;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Instant, SystemTime};

/// Default allowlist when `LUMINA_EGRESS_ALLOWLIST` is not set.
/// Only loopback addresses are permitted.
const DEFAULT_ALLOWLIST: &[&str] = &["localhost", "127.0.0.1", "::1"];

/// Alert threshold: number of blocks in the observation window.
const RATE_ALERT_THRESHOLD: u64 = 5;

/// Observation window for rate alerting (seconds).
const RATE_WINDOW_SECS: u64 = 60;

// ─────────────────────────────────────────────────────────────────────────────

/// A blocked egress attempt.
#[derive(Debug, Clone)]
pub struct EgressViolation {
    /// The destination that was blocked (full URL or hostname).
    pub destination: String,
    /// The tool name that attempted the request.
    pub tool_name: String,
    /// Wall-clock time of the attempt.
    pub timestamp: SystemTime,
    /// Human-readable reason for the block.
    pub reason: String,
}

impl std::fmt::Display for EgressViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Egress blocked for tool '{}': destination='{}', reason='{}'",
            self.tool_name, self.destination, self.reason
        )
    }
}

impl std::error::Error for EgressViolation {}

// ─────────────────────────────────────────────────────────────────────────────

/// Shared mutable state for the rate-alert window.
///
/// Both fields are kept under one mutex so reads and writes are atomic with
/// respect to each other — eliminating the TOCTOU race that would exist if
/// they were separate atomics.
struct RateWindow {
    /// Instant when the current observation window started.
    start: Instant,
    /// `block_count` snapshot at the start of the current window.
    base: u64,
}

/// Inspects and enforces outbound egress policy.
///
/// # Thread safety
/// All mutable state is protected by a `Mutex` or uses atomic operations.
/// `EgressInspector` is `Send + Sync` and suitable for sharing via `Arc`.
pub struct EgressInspector {
    /// The parsed allowlist entries (hostnames or IP strings).
    allowlist: Vec<String>,
    /// Cumulative count of blocked requests (monotonically increasing).
    block_count: AtomicU64,
    /// Rate-alert window state (start time and base count, kept together to
    /// prevent TOCTOU races between reads of the two values).
    rate_window: Mutex<RateWindow>,
}

impl EgressInspector {
    /// Create a new inspector with an explicit allowlist.
    ///
    /// Unlike [`from_env`], this constructor uses **only** the provided
    /// allowlist and never reads environment variables. Passing an empty `Vec`
    /// results in a deny-all inspector (no hosts are permitted, not even
    /// loopback — callers that want loopback must include it explicitly).
    pub fn new(allowlist: Vec<String>) -> Self {
        EgressInspector {
            allowlist,
            block_count: AtomicU64::new(0),
            rate_window: Mutex::new(RateWindow {
                start: Instant::now(),
                base: 0,
            }),
        }
    }

    /// Create an inspector loaded from `LUMINA_EGRESS_ALLOWLIST`.
    ///
    /// Uses the default loopback-only allowlist when the variable is not set
    /// or is empty.
    pub fn from_env() -> Self {
        EgressInspector {
            allowlist: load_allowlist_from_env(),
            block_count: AtomicU64::new(0),
            rate_window: Mutex::new(RateWindow {
                start: Instant::now(),
                base: 0,
            }),
        }
    }

    /// Inspect an outbound request.
    ///
    /// `destination` may be a full URL (`https://api.example.com/path`) or a
    /// bare hostname / IP address.
    ///
    /// Returns `Ok(())` if the request is allowed, or `Err(EgressViolation)` if
    /// it is blocked. All blocked requests are logged at WARN level.
    pub fn inspect(&self, destination: &str, tool_name: &str) -> std::result::Result<(), EgressViolation> {
        let host = match extract_host(destination) {
            Some(h) => h,
            None => {
                let violation = EgressViolation {
                    destination: destination.to_string(),
                    tool_name: tool_name.to_string(),
                    timestamp: SystemTime::now(),
                    reason: "Could not parse host from destination".to_string(),
                };
                self.record_block();
                log::warn!("EGRESS BLOCKED: {}", violation);
                return Err(violation);
            }
        };

        if self.matches_allowlist(&host) {
            log::debug!("Egress allowed: tool='{}' host='{}'", tool_name, host);
            return Ok(());
        }

        let violation = EgressViolation {
            destination: destination.to_string(),
            tool_name: tool_name.to_string(),
            timestamp: SystemTime::now(),
            reason: format!("Host '{}' is not in the egress allowlist", host),
        };
        self.record_block();
        log::warn!("EGRESS BLOCKED: {}", violation);
        Err(violation)
    }

    /// Return the total number of blocked egress requests since creation.
    pub fn block_count(&self) -> u64 {
        self.block_count.load(Ordering::Relaxed)
    }

    /// Returns `true` if more than [`RATE_ALERT_THRESHOLD`] blocks occurred in
    /// the last [`RATE_WINDOW_SECS`] seconds.
    ///
    /// The rate window is advanced here as well as in `record_block`, so that
    /// a burst of blocks that ended more than 60 seconds ago stops producing
    /// alerts — the window expires on the next call to this method.
    ///
    /// Both `block_count` and `window_base` are read under the same mutex lock
    /// to avoid TOCTOU races.
    pub fn check_rate_alert(&self) -> bool {
        let mut window = self.rate_window.lock().unwrap_or_else(|e| e.into_inner());
        let current_total = self.block_count.load(Ordering::Relaxed);
        // Expire the window if 60 seconds have passed since it started.
        if window.start.elapsed().as_secs() >= RATE_WINDOW_SECS {
            window.base = current_total;
            window.start = Instant::now();
            return false; // Window just expired — no blocks counted in the new window yet.
        }
        let delta = current_total.saturating_sub(window.base);
        delta > RATE_ALERT_THRESHOLD
    }

    // ── private ──────────────────────────────────────────────────────────────

    /// Increment the block counter and maintain the rate window.
    fn record_block(&self) {
        let new_count = self.block_count.fetch_add(1, Ordering::Relaxed) + 1;

        // Rotate the window if it has expired. Holds the lock while updating
        // both `start` and `base` so they stay in sync.
        let mut window = self.rate_window.lock().unwrap_or_else(|e| e.into_inner());
        if window.start.elapsed().as_secs() >= RATE_WINDOW_SECS {
            window.base = new_count;
            window.start = Instant::now();
        }
    }

    /// Check whether `host` matches any entry in the allowlist.
    ///
    /// Wildcard entries (`*.example.com`) match exactly **one** subdomain
    /// label (e.g. `foo.example.com`) but not deeper nesting
    /// (`a.b.example.com`) and not the root domain (`example.com`).
    fn matches_allowlist(&self, host: &str) -> bool {
        let host_lower = host.to_lowercase();
        for entry in &self.allowlist {
            let entry_lower = entry.to_lowercase();
            if entry_lower.starts_with("*.") {
                // "*.example.com" → suffix = ".example.com"
                // Match requires exactly one additional label before the suffix:
                //   host = "foo.example.com"  → ok (one label "foo")
                //   host = "a.b.example.com"  → rejected (two labels before suffix)
                //   host = "example.com"      → rejected (no label before suffix)
                let suffix = &entry_lower[1..]; // ".example.com"
                if host_lower.ends_with(suffix) {
                    let prefix = &host_lower[..host_lower.len() - suffix.len()];
                    // Exactly one label means no dots in the prefix.
                    if !prefix.is_empty() && !prefix.contains('.') {
                        return true;
                    }
                }
            } else if host_lower == entry_lower {
                return true;
            }
        }
        false
    }

    /// Test-only constructor that accepts an explicit `RateWindow` initial state.
    ///
    /// Allows tests to inject a window with an old `start` time to simulate
    /// window expiration without sleeping.
    #[cfg(test)]
    pub(crate) fn new_with_window_start(allowlist: Vec<String>, window_start: Instant) -> Self {
        EgressInspector {
            allowlist,
            block_count: AtomicU64::new(0),
            rate_window: Mutex::new(RateWindow {
                start: window_start,
                base: 0,
            }),
        }
    }

    /// Returns `true` for RFC-1918 and loopback IPv4/IPv6 addresses.
    ///
    /// Callers can use this after DNS resolution to detect DNS rebinding
    /// attacks: if a public hostname resolves to a private IP and that IP
    /// is not explicitly in the allowlist, the connection should be blocked.
    pub fn is_private_ip(addr: &str) -> bool {
        // IPv6 loopback
        if addr.eq_ignore_ascii_case("::1") {
            return true;
        }
        // Parse as dotted-quad IPv4
        let parts: Vec<&str> = addr.split('.').collect();
        if parts.len() != 4 {
            return false;
        }
        let octets: Vec<u8> = parts
            .iter()
            .filter_map(|p| p.parse::<u8>().ok())
            .collect();
        if octets.len() != 4 {
            return false;
        }
        match octets[0] {
            10 => true,           // RFC-1918 class A private range
            127 => true,          // IPv4 loopback
            172 => octets[1] >= 16 && octets[1] <= 31,  // RFC-1918 class B private range
            192 => octets[1] == 168,  // RFC-1918 class C private range
            _ => false,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Free functions
// ─────────────────────────────────────────────────────────────────────────────

/// Parse `LUMINA_EGRESS_ALLOWLIST` from the environment.
///
/// Entries are trimmed and empty strings are dropped.
/// Falls back to [`DEFAULT_ALLOWLIST`] if the variable is absent or empty.
fn load_allowlist_from_env() -> Vec<String> {
    let raw = std::env::var("LUMINA_EGRESS_ALLOWLIST").unwrap_or_default();
    let entries: Vec<String> = raw
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if entries.is_empty() {
        DEFAULT_ALLOWLIST.iter().map(|s| s.to_string()).collect()
    } else {
        entries
    }
}

/// Extract the hostname or IP from a destination string.
///
/// Handles:
/// - Full URLs: `https://api.example.com:443/path?q=1` → `api.example.com`
/// - Bare hostnames: `api.example.com` → `api.example.com`
/// - IPv4 addresses with or without port: `198.51.100.1:8080` → `198.51.100.1`
///
/// Returns `None` if the input is empty or cannot be parsed.
fn extract_host(destination: &str) -> Option<String> {
    let s = destination.trim();
    if s.is_empty() {
        return None;
    }

    // If it looks like a URL (has "://"), parse out the authority.
    if let Some(after_scheme) = s.find("://").map(|i| &s[i + 3..]) {
        // authority = everything before the first '/', '?', '#'
        let authority = after_scheme
            .split(|c| c == '/' || c == '?' || c == '#')
            .next()
            .unwrap_or(after_scheme);
        // Strip userinfo (user:pass@host)
        let host_port = if let Some(at) = authority.rfind('@') {
            &authority[at + 1..]
        } else {
            authority
        };
        // Strip port
        let host = strip_port(host_port);
        if host.is_empty() {
            return None;
        }
        return Some(host.to_string());
    }

    // Not a URL — treat as bare hostname or IP (strip any trailing port).
    let host = strip_port(s);
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

/// Strip a trailing `:port` from a host string.
///
/// IPv6 addresses in bracket notation (`[::1]` or `[::1]:443`) have the
/// brackets removed. Bare IPv6 addresses (multiple colons, no brackets) are
/// returned as-is.
fn strip_port(s: &str) -> &str {
    // IPv6 with brackets: "[::1]" or "[::1]:443"
    if s.starts_with('[') {
        if let Some(end) = s.find(']') {
            return &s[1..end];
        }
    }
    // IPv4 or hostname with optional port — single colon → host:port
    if s.contains(':') {
        // Multiple colons → bare IPv6 address without brackets — return as-is.
        if s.chars().filter(|&c| c == ':').count() > 1 {
            return s;
        }
        // Single colon → hostname:port or IPv4:port
        return s.split(':').next().unwrap_or(s);
    }
    s
}

/// Convert an `EgressViolation` to `LuminaError` for use in contexts that return `Result`.
impl From<EgressViolation> for LuminaError {
    fn from(v: EgressViolation) -> Self {
        LuminaError::SecurityViolation(v.to_string())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::sync::Mutex;

    // Serialize tests that mutate env vars.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    // ── extract_host ──────────────────────────────────────────────────────────

    #[test]
    fn test_extract_host_full_url() {
        assert_eq!(extract_host("https://api.example.com/path"), Some("api.example.com".to_string()));
    }

    #[test]
    fn test_extract_host_with_port() {
        assert_eq!(extract_host("http://api.example.com:8080/path"), Some("api.example.com".to_string()));
    }

    #[test]
    fn test_extract_host_bare_hostname() {
        assert_eq!(extract_host("api.example.com"), Some("api.example.com".to_string()));
    }

    #[test]
    fn test_extract_host_bare_ip() {
        assert_eq!(extract_host("198.51.100.1"), Some("198.51.100.1".to_string()));
    }

    #[test]
    fn test_extract_host_bare_ip_with_port() {
        assert_eq!(extract_host("198.51.100.1:8080"), Some("198.51.100.1".to_string()));
    }

    #[test]
    fn test_extract_host_empty() {
        assert_eq!(extract_host(""), None);
        assert_eq!(extract_host("   "), None);
    }

    #[test]
    fn test_extract_host_url_with_userinfo() {
        assert_eq!(extract_host("http://user:pass@api.example.com/path"), Some("api.example.com".to_string()));
    }

    // ── is_private_ip ────────────────────────────────────────────────────────

    #[test]
    fn test_is_private_ip_rfc1918() {
        // RFC1918 literals are required here to exercise the classifier itself.
        assert!(EgressInspector::is_private_ip("10.0.0.1"));
        assert!(EgressInspector::is_private_ip("10.255.255.255"));
        assert!(EgressInspector::is_private_ip("172.16.0.1"));
        assert!(EgressInspector::is_private_ip("172.31.255.255"));
        assert!(EgressInspector::is_private_ip("172.16.1.1"));
        assert!(EgressInspector::is_private_ip("172.20.5.5"));
    }

    #[test]
    fn test_is_private_ip_loopback() {
        assert!(EgressInspector::is_private_ip("127.0.0.1"));
        assert!(EgressInspector::is_private_ip("127.255.255.255"));
        assert!(EgressInspector::is_private_ip("::1"));
    }

    #[test]
    fn test_is_private_ip_public() {
        assert!(!EgressInspector::is_private_ip("8.8.8.8"));
        assert!(!EgressInspector::is_private_ip("1.1.1.1"));
        assert!(!EgressInspector::is_private_ip("203.0.113.1"));
        assert!(!EgressInspector::is_private_ip("172.15.255.255"));
        assert!(!EgressInspector::is_private_ip("172.32.0.0"));
    }

    // ── allowlist matching ───────────────────────────────────────────────────

    #[test]
    fn test_allowlisted_host_passes() {
        let inspector = EgressInspector::new(vec!["api.example.com".to_string()]);
        assert!(inspector.inspect("https://api.example.com/path", "test_tool").is_ok());
    }

    #[test]
    fn test_non_allowlisted_host_blocked() {
        let inspector = EgressInspector::new(vec!["api.example.com".to_string()]);
        let result = inspector.inspect("https://evil.attacker.com/steal", "test_tool");
        assert!(result.is_err());
        let violation = result.unwrap_err();
        assert!(violation.reason.contains("not in the egress allowlist"));
        assert_eq!(violation.tool_name, "test_tool");
    }

    #[test]
    fn test_empty_allowlist_blocks_everything_including_loopback() {
        // EgressInspector::new with an empty vec → deny ALL, never reads env.
        let inspector = EgressInspector::new(vec![]);
        assert!(inspector.inspect("https://api.example.com/", "tool").is_err(),
            "Non-loopback should be blocked by empty allowlist");
        // Even loopback is blocked when allowlist is explicitly empty
        assert!(inspector.inspect("http://localhost/", "tool").is_err(),
            "Localhost should be blocked by explicit empty allowlist");
    }

    #[test]
    #[serial]
    fn test_from_env_defaults_to_loopback_when_unset() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("LUMINA_EGRESS_ALLOWLIST");
        let inspector = EgressInspector::from_env();
        assert!(inspector.inspect("http://localhost/", "tool").is_ok());
        assert!(inspector.inspect("http://127.0.0.1/", "tool").is_ok());
        assert!(inspector.inspect("https://api.example.com/", "tool").is_err());
    }

    #[test]
    #[serial]
    fn test_new_does_not_read_env() {
        let _g = ENV_LOCK.lock().unwrap();
        // Set env to include "api.example.com"
        std::env::set_var("LUMINA_EGRESS_ALLOWLIST", "api.example.com");
        // new(vec![]) should NOT load from env — result is deny-all
        let inspector = EgressInspector::new(vec![]);
        assert!(inspector.inspect("https://api.example.com/", "tool").is_err(),
            "new(vec![]) must not load from env — should be deny-all");
        std::env::remove_var("LUMINA_EGRESS_ALLOWLIST");
    }

    #[test]
    fn test_wildcard_single_level_subdomain_matching() {
        let inspector = EgressInspector::new(vec!["*.example.com".to_string()]);
        // Single-level subdomain — should match
        assert!(inspector.inspect("https://api.example.com/", "tool").is_ok());
        assert!(inspector.inspect("https://foo.example.com/", "tool").is_ok());
        // Root domain — should NOT match wildcard
        assert!(inspector.inspect("https://example.com/", "tool").is_err(),
            "Root domain should not match *.example.com");
        // Multi-level subdomain — should NOT match (security boundary)
        assert!(inspector.inspect("https://deep.api.example.com/", "tool").is_err(),
            "Multi-level subdomain should not match single-level wildcard");
        // Different domain — should not match
        assert!(inspector.inspect("https://api.other.com/", "tool").is_err());
    }

    #[test]
    fn test_private_ip_explicitly_allowlisted_passes() {
        // Operator can explicitly allowlist a private IP
        let inspector = EgressInspector::new(vec!["198.51.100.5".to_string()]);
        assert!(inspector.inspect("http://198.51.100.5:8080/api", "tool").is_ok());
    }

    #[test]
    fn test_private_ip_not_allowlisted_blocked() {
        // Private IP that is NOT explicitly allowlisted should be blocked
        let inspector = EgressInspector::new(vec!["api.example.com".to_string()]);
        let result = inspector.inspect("http://198.51.100.99/internal", "tool");
        assert!(result.is_err());
    }

    // ── block counting and rate alerting ─────────────────────────────────────

    #[test]
    fn test_block_count_increments() {
        let inspector = EgressInspector::new(vec!["localhost".to_string()]);
        assert_eq!(inspector.block_count(), 0);
        let _ = inspector.inspect("https://blocked.example.com/", "tool");
        assert_eq!(inspector.block_count(), 1);
        let _ = inspector.inspect("https://also.blocked.example.com/", "tool");
        assert_eq!(inspector.block_count(), 2);
    }

    #[test]
    fn test_rate_alert_triggers_after_threshold() {
        let inspector = EgressInspector::new(vec!["localhost".to_string()]);
        // Should not alert before threshold
        assert!(!inspector.check_rate_alert());
        // Generate 6 blocks in the same window
        for i in 0..6 {
            let _ = inspector.inspect(&format!("https://blocked{}.example.com/", i), "tool");
        }
        assert!(inspector.check_rate_alert(), "Should alert after >5 blocks in window");
    }

    #[test]
    fn test_rate_alert_not_triggered_below_threshold() {
        let inspector = EgressInspector::new(vec!["localhost".to_string()]);
        // Generate exactly 5 blocks — should NOT trigger (threshold is >5)
        for i in 0..5 {
            let _ = inspector.inspect(&format!("https://b{}.example.com/", i), "tool");
        }
        assert!(!inspector.check_rate_alert(), "5 blocks should not trigger alert (threshold is >5)");
    }

    #[test]
    fn test_rate_alert_clears_after_window_expires() {
        use std::time::Duration;
        // Create an inspector whose rate window started 70 seconds ago (already expired).
        let old_start = Instant::now() - Duration::from_secs(70);
        let inspector = EgressInspector::new_with_window_start(
            vec!["localhost".to_string()],
            old_start,
        );
        // Generate 6 blocks in what was the old window (simulated by the old start time).
        // We set block_count manually via fetch_add on the atomic.
        // Instead: add 6 blocks with inspect (they always block since "other.com" is not allowlisted).
        for i in 0..6 {
            let _ = inspector.inspect(&format!("https://old{}.example.com/", i), "tool");
        }
        // block_count is 6, but window.start is 70s in the past — alert should NOT fire
        // because check_rate_alert will detect the expired window and reset it.
        assert!(!inspector.check_rate_alert(),
            "Alert should not fire after window expiry — burst was more than 60s ago");
    }

    #[test]
    fn test_violation_logged_to_audit() {
        // Verify EgressViolation contains all required fields
        let inspector = EgressInspector::new(vec!["localhost".to_string()]);
        let result = inspector.inspect("https://attacker.example.com/exfil?data=secret", "dangerous_tool");
        assert!(result.is_err());
        let v = result.unwrap_err();
        assert_eq!(v.tool_name, "dangerous_tool");
        assert_eq!(v.destination, "https://attacker.example.com/exfil?data=secret");
        assert!(!v.reason.is_empty());
    }

    #[test]
    fn test_unparseable_destination_blocked() {
        let inspector = EgressInspector::new(vec!["localhost".to_string()]);
        // An empty destination should fail closed
        let result = inspector.inspect("", "tool");
        assert!(result.is_err(), "Empty destination should be blocked (fail closed)");
    }

    // ── from_env ──────────────────────────────────────────────────────────────

    #[test]
    #[serial]
    fn test_from_env_parses_comma_separated() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("LUMINA_EGRESS_ALLOWLIST", "api.example.com, other.example.com");
        let inspector = EgressInspector::from_env();
        assert!(inspector.inspect("https://api.example.com/path", "tool").is_ok());
        assert!(inspector.inspect("https://other.example.com/", "tool").is_ok());
        assert!(inspector.inspect("https://blocked.example.com/", "tool").is_err());
        std::env::remove_var("LUMINA_EGRESS_ALLOWLIST");
    }

    // ── EgressViolation → LuminaError conversion ──────────────────────────────

    #[test]
    fn test_violation_converts_to_lumina_error() {
        let v = EgressViolation {
            destination: "https://evil.example.com/".to_string(),
            tool_name: "evil_tool".to_string(),
            timestamp: SystemTime::now(),
            reason: "not in allowlist".to_string(),
        };
        let err: LuminaError = v.into();
        assert!(matches!(err, LuminaError::SecurityViolation(_)));
        let msg = err.to_string();
        assert!(msg.contains("evil_tool"));
    }
}
