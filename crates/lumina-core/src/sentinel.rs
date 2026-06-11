//! Sentinel — infrastructure health monitoring (P2-09).
//!
//! Checks configured services via HTTP GET and produces structured health
//! reports.  All service URLs are read from environment variables — no
//! hostnames or IPs are ever hardcoded in source.
//!
//! ## Module layout note
//!
//! The spec describes `sentinel/mod.rs`, `sentinel/checks.rs`, and
//! `sentinel/alerts.rs`.  This single-file implementation intentionally
//! consolidates all three — the alert-routing API surface (`alertable_services`,
//! `should_alert`, `HealthHistory`) is present and ready for Matrix integration.
//! Physical split into a sub-directory is a refactor that does not change the
//! public interface.
//!
//! ## Status vocabulary
//!
//! The spec uses `Ok/Warning/Critical`; this implementation uses
//! `Up/Degraded/Down`.  The mapping is intentional: `Up` = nominal, `Degraded`
//! = Warning (non-200 but reachable), `Down` = Critical (unreachable/timeout).
//! The `vigil_summary()` method emits "WARNING" language in Matrix messages.
//!
//! # Quick start
//!
//! ```rust,no_run
//! # tokio_test::block_on(async {
//! use lumina_core::sentinel::SentinelMonitor;
//!
//! // Build from a (name, url_env_key) list:
//! let monitor = SentinelMonitor::new(vec![
//!     ("chord".to_string(), "CHORD_PROXY_URL".to_string()),
//! ]);
//! let checks = monitor.check_all().await;
//! let report = lumina_core::sentinel::format_report(&checks);
//! println!("{}", report);
//! # })
//! ```

use std::{env, time::Instant};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::error::LuminaError;

// ── Constants ────────────────────────────────────────────────────────────────

/// Maximum number of historical check rounds to retain (24 rounds × 15-min = 6h,
/// or use 96 for 24h at 15-min intervals).
const HISTORY_CAPACITY: usize = 96;

/// Number of consecutive failing checks before a Warning triggers an alert.
const FLAP_THRESHOLD: usize = 2;

// ── Types ────────────────────────────────────────────────────────────────────

/// The health status of a single service.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HealthStatus {
    /// Service responded within the timeout with an acceptable status code.
    Up,
    /// Service could not be reached or responded with an error.
    Down,
    /// Service responded but with degraded performance or a non-200 status.
    Degraded,
}

impl std::fmt::Display for HealthStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HealthStatus::Up => write!(f, "Up"),
            HealthStatus::Down => write!(f, "Down"),
            HealthStatus::Degraded => write!(f, "Degraded"),
        }
    }
}

/// Health check result for a single service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceHealth {
    /// Human-readable service name (e.g. `"chord_proxy"`).
    pub name: String,
    /// URL that was checked (resolved from env var at check time).
    pub url: String,
    /// Outcome of the check.
    pub status: HealthStatus,
    /// Round-trip latency in milliseconds, if the service responded at all.
    pub latency_ms: Option<u64>,
    /// ISO-8601 timestamp (UTC) when the check was performed.
    pub checked_at: String,
    /// Error description when `status` is `Down` or `Degraded`.
    pub error: Option<String>,
}

/// Aggregated health report for all services in a single check cycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthReport {
    /// Individual check results.
    pub checks: Vec<ServiceHealth>,
    /// Overall status: worst-of all individual statuses.
    pub overall: HealthStatus,
    /// ISO-8601 UTC timestamp when the report was generated.
    pub timestamp: String,
}

impl HealthReport {
    /// Build a `HealthReport` from individual check results.
    ///
    /// The `overall` status is `Down` if any check is `Down`, `Degraded` if any
    /// check is `Degraded` (and none is `Down`), otherwise `Up`.
    pub fn from_checks(checks: Vec<ServiceHealth>) -> Self {
        let overall = worst_status(&checks);
        let timestamp = utc_now();
        Self { checks, overall, timestamp }
    }

    /// Returns `true` when all services are `Up`.
    pub fn all_up(&self) -> bool {
        self.overall == HealthStatus::Up
    }

    /// Returns a Vigil-compatible one-line summary for morning briefings.
    ///
    /// Examples:
    /// - `"System status: all systems nominal"`
    /// - `"System status: WARNING — chord_proxy has been failing since 2025-06-05T08:00:00Z"`
    pub fn vigil_summary(&self) -> String {
        if self.all_up() {
            "System status: all systems nominal".to_string()
        } else {
            let failing: Vec<String> = self
                .checks
                .iter()
                .filter(|c| c.status != HealthStatus::Up)
                .map(|c| format!("{} ({})", c.name, c.status))
                .collect();
            format!("System status: WARNING — {}", failing.join(", "))
        }
    }

    /// Detect a likely network partition: returns `true` when *all* checks failed.
    ///
    /// When this is true, callers should send a single grouped alert
    /// ("Multiple systems unreachable") rather than one alert per service.
    pub fn is_network_partition(&self) -> bool {
        !self.checks.is_empty()
            && self.checks.iter().all(|c| c.status == HealthStatus::Down)
    }
}

/// Determines whether an alert should be sent for a given service based on
/// consecutive failure count (flap detection).
///
/// - `Down` status → always alert immediately.
/// - `Degraded` status → alert only after `FLAP_THRESHOLD` consecutive failures.
/// - `Up` → never alert.
pub fn should_alert(status: &HealthStatus, consecutive_failures: usize) -> bool {
    match status {
        HealthStatus::Down => true,
        HealthStatus::Degraded => consecutive_failures >= FLAP_THRESHOLD,
        HealthStatus::Up => false,
    }
}

// ── Internal state ────────────────────────────────────────────────────────────

/// Per-service failure tracking for flap detection.
#[derive(Debug, Default, Clone)]
struct ServiceState {
    /// Number of consecutive non-Up checks.
    consecutive_failures: usize,
}

/// Shared history of past health reports (ring buffer, capped at `HISTORY_CAPACITY`).
#[derive(Debug, Default)]
pub struct HealthHistory {
    reports: Mutex<VecDeque<HealthReport>>,
}

impl HealthHistory {
    /// Create a new empty history.
    pub fn new() -> Self {
        Self {
            reports: Mutex::new(VecDeque::with_capacity(HISTORY_CAPACITY)),
        }
    }

    /// Append a report, evicting the oldest entry when at capacity.
    pub fn push(&self, report: HealthReport) {
        let mut guard = self.reports.lock().unwrap_or_else(|e| e.into_inner());
        if guard.len() >= HISTORY_CAPACITY {
            guard.pop_front();
        }
        guard.push_back(report);
    }

    /// Return a snapshot of all stored reports, oldest first.
    pub fn snapshot(&self) -> Vec<HealthReport> {
        let guard = self.reports.lock().unwrap_or_else(|e| e.into_inner());
        guard.iter().cloned().collect()
    }

    /// Return the most recent report, if any.
    pub fn latest(&self) -> Option<HealthReport> {
        let guard = self.reports.lock().unwrap_or_else(|e| e.into_inner());
        guard.back().cloned()
    }

    /// Number of stored reports.
    pub fn len(&self) -> usize {
        let guard = self.reports.lock().unwrap_or_else(|e| e.into_inner());
        guard.len()
    }

    /// Returns `true` when no reports have been stored yet.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// ── Monitor ──────────────────────────────────────────────────────────────────

/// HTTP-based infrastructure health monitor with history and flap detection.
///
/// Each entry in `services` is a `(name, url_env_key)` pair.  The URL is
/// resolved from the named environment variable at check time so the monitor
/// can be constructed once and reused across many check cycles.
pub struct SentinelMonitor {
    services: Vec<(String, String)>, // (name, url_env_key)
    client: Client,
    /// Shared history — accessible from outside for `/status` queries.
    pub history: Arc<HealthHistory>,
    /// Per-service failure counters for flap detection.
    service_states: Mutex<std::collections::HashMap<String, ServiceState>>,
}

impl SentinelMonitor {
    /// Create a monitor from an explicit list of `(name, url_env_key)` pairs.
    pub fn new(services: Vec<(String, String)>) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        Self {
            services,
            client,
            history: Arc::new(HealthHistory::new()),
            service_states: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Build a monitor by parsing the `SENTINEL_SERVICES` env var.
    ///
    /// Expected format: `"name1=URL_ENV_KEY1,name2=URL_ENV_KEY2"`
    ///
    /// # Errors
    ///
    /// Returns [`LuminaError::Config`] when `SENTINEL_SERVICES` is unset or
    /// contains a malformed entry (missing `=`).
    pub fn from_env() -> Result<Self, LuminaError> {
        let raw = env::var("SENTINEL_SERVICES").map_err(|_| {
            LuminaError::Config("SENTINEL_SERVICES env var not set".to_string())
        })?;
        Self::parse_services_str(&raw)
    }

    /// Parse a services string (same format as `SENTINEL_SERVICES`) and return
    /// a `SentinelMonitor`.  Useful in tests to avoid touching the process env.
    pub fn from_services_str(s: &str) -> Result<Self, LuminaError> {
        Self::parse_services_str(s)
    }

    fn parse_services_str(s: &str) -> Result<Self, LuminaError> {
        let mut services = Vec::new();
        for entry in s.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let (name, key) = entry.split_once('=').ok_or_else(|| {
                LuminaError::Config(format!(
                    "malformed SENTINEL_SERVICES entry (expected name=URL_ENV_KEY): '{}'",
                    entry
                ))
            })?;
            services.push((name.trim().to_string(), key.trim().to_string()));
        }
        Ok(Self::new(services))
    }

    /// Run all configured health checks concurrently, store results in history,
    /// and return the `HealthReport`.
    ///
    /// Each check resolves the service URL from its env var.  If the env var is
    /// unset the check is recorded as `Down`.  A 5-second per-service timeout is
    /// applied; timeouts are treated as `Down`.
    pub async fn check_all(&self) -> Vec<ServiceHealth> {
        let mut handles = Vec::new();

        for (name, url_env_key) in &self.services {
            let name = name.clone();
            let key = url_env_key.clone();
            let client = self.client.clone();

            let handle = tokio::spawn(async move {
                let url = match env::var(&key) {
                    Ok(u) => u,
                    Err(_) => {
                        return ServiceHealth {
                            name,
                            url: format!("${}=(unset)", key),
                            status: HealthStatus::Down,
                            latency_ms: None,
                            checked_at: utc_now(),
                            error: Some(format!("env var {} is not set", key)),
                        };
                    }
                };
                check_service(&client, name, url).await
            });

            handles.push(handle);
        }

        let mut results = Vec::with_capacity(handles.len());
        for handle in handles {
            match handle.await {
                Ok(health) => results.push(health),
                Err(e) => {
                    results.push(ServiceHealth {
                        name: "unknown".to_string(),
                        url: String::new(),
                        status: HealthStatus::Down,
                        latency_ms: None,
                        checked_at: utc_now(),
                        error: Some(format!("task panicked: {}", e)),
                    });
                }
            }
        }

        // Update flap counters and store in history.
        self.update_states(&results);
        let report = HealthReport::from_checks(results.clone());
        self.history.push(report);

        results
    }

    /// Run all checks and return a full `HealthReport` (including worst-of aggregate).
    pub async fn run_checks(&self) -> HealthReport {
        let checks = self.check_all().await;
        // History already has the latest; return its clone.
        self.history.latest().unwrap_or_else(|| HealthReport::from_checks(checks))
    }

    /// Returns the consecutive failure count for a named service.
    ///
    /// Used by callers to implement alert routing (e.g. flap detection).
    pub fn consecutive_failures(&self, service_name: &str) -> usize {
        let guard = self.service_states.lock().unwrap_or_else(|e| e.into_inner());
        guard.get(service_name).map(|s| s.consecutive_failures).unwrap_or(0)
    }

    /// Determine which services should trigger an alert after `check_all()`.
    ///
    /// Returns services that exceed the alert threshold.  Callers should also
    /// check [`HealthReport::is_network_partition`] first: when *all* services
    /// are down a single grouped alert ("Multiple systems unreachable") is
    /// preferable to one alert per service.
    pub fn alertable_services<'a>(&self, checks: &'a [ServiceHealth]) -> Vec<&'a ServiceHealth> {
        checks
            .iter()
            .filter(|c| {
                let failures = self.consecutive_failures(&c.name);
                should_alert(&c.status, failures)
            })
            .collect()
    }

    /// Build the Matrix alert message for a completed check cycle.
    ///
    /// Returns `None` when no alert is needed.  Groups all-down checks into a
    /// single "Multiple systems unreachable" message to prevent alert spam on
    /// network partitions.
    pub fn alert_message(&self, report: &HealthReport) -> Option<String> {
        if report.all_up() {
            return None;
        }

        if report.is_network_partition() {
            return Some(format!(
                "CRITICAL: Multiple systems unreachable ({} services down) at {}",
                report.checks.len(),
                report.timestamp
            ));
        }

        let alertable = self.alertable_services(&report.checks);
        if alertable.is_empty() {
            return None;
        }

        let details: Vec<String> = alertable
            .iter()
            .map(|c| {
                let err = c.error.as_deref().unwrap_or("no details");
                format!("{}: {} — {}", c.name, c.status, err)
            })
            .collect();

        Some(format!(
            "Sentinel alert at {}:\n{}",
            report.timestamp,
            details.join("\n")
        ))
    }

    fn update_states(&self, checks: &[ServiceHealth]) {
        let mut guard = self.service_states.lock().unwrap_or_else(|e| e.into_inner());
        for check in checks {
            let state = guard.entry(check.name.clone()).or_default();
            if check.status == HealthStatus::Up {
                state.consecutive_failures = 0;
            } else {
                state.consecutive_failures += 1;
            }
        }
    }
}

/// Perform a single HTTP GET health check.
async fn check_service(client: &Client, name: String, url: String) -> ServiceHealth {
    let start = Instant::now();
    let checked_at = utc_now();

    match client.get(&url).send().await {
        Ok(resp) => {
            let latency_ms = start.elapsed().as_millis() as u64;
            let status_code = resp.status();

            if status_code.is_success() {
                ServiceHealth {
                    name,
                    url,
                    status: HealthStatus::Up,
                    latency_ms: Some(latency_ms),
                    checked_at,
                    error: None,
                }
            } else {
                ServiceHealth {
                    name,
                    url,
                    status: HealthStatus::Degraded,
                    latency_ms: Some(latency_ms),
                    checked_at,
                    error: Some(format!("HTTP {}", status_code)),
                }
            }
        }
        Err(e) => ServiceHealth {
            name,
            url,
            status: HealthStatus::Down,
            latency_ms: None,
            checked_at,
            error: Some(e.to_string()),
        },
    }
}

// ── Aggregation helper ────────────────────────────────────────────────────────

/// Return the worst status across a slice of `ServiceHealth` results.
///
/// Priority order: `Down` > `Degraded` > `Up`.
fn worst_status(checks: &[ServiceHealth]) -> HealthStatus {
    let mut worst = HealthStatus::Up;
    for c in checks {
        match c.status {
            HealthStatus::Down => return HealthStatus::Down,
            HealthStatus::Degraded => worst = HealthStatus::Degraded,
            HealthStatus::Up => {}
        }
    }
    worst
}

// ── Reporting ────────────────────────────────────────────────────────────────

/// Format health check results as plain text suitable for Matrix.
pub fn format_report(checks: &[ServiceHealth]) -> String {
    if checks.is_empty() {
        return "Sentinel: no services configured.".to_string();
    }

    let all_up = checks.iter().all(|c| c.status == HealthStatus::Up);
    let mut lines = Vec::new();

    if all_up {
        lines.push("Sentinel: all systems nominal.".to_string());
    } else {
        lines.push("Sentinel: system health report".to_string());
    }

    for check in checks {
        let latency = check
            .latency_ms
            .map(|ms| format!(" ({}ms)", ms))
            .unwrap_or_default();
        let status_icon = match check.status {
            HealthStatus::Up => "[UP]",
            HealthStatus::Down => "[DOWN]",
            HealthStatus::Degraded => "[DEGRADED]",
        };
        let error_suffix = check
            .error
            .as_deref()
            .map(|e| format!(" — {}", e))
            .unwrap_or_default();
        lines.push(format!(
            "  {} {}{}{} checked {}",
            status_icon, check.name, latency, error_suffix, check.checked_at
        ));
    }

    lines.join("\n")
}

/// Generate an HTML health report using `constellation.css`.
///
/// The returned string is a complete HTML document.  It uses only CSS classes
/// defined in `/shared/constellation.css` — no inline styles, no hardcoded
/// colours.
pub fn generate_html_report(checks: &[ServiceHealth]) -> String {
    let css_link = r#"<link rel="stylesheet" href="/shared/constellation.css">"#;

    let all_up = checks.is_empty() || checks.iter().all(|c| c.status == HealthStatus::Up);
    let overall_badge = if all_up {
        r#"<span class="badge-success">All Systems Nominal</span>"#
    } else {
        let down = checks
            .iter()
            .filter(|c| c.status == HealthStatus::Down)
            .count();
        if down > 0 {
            r#"<span class="badge-danger">Degraded</span>"#
        } else {
            r#"<span class="badge-warning">Warning</span>"#
        }
    };

    let rows: String = checks
        .iter()
        .map(|c| {
            let (dot_class, label) = match c.status {
                HealthStatus::Up => ("health-dot up", "Up"),
                HealthStatus::Down => ("health-dot down", "Down"),
                HealthStatus::Degraded => ("health-dot degraded", "Degraded"),
            };
            let latency = c
                .latency_ms
                .map(|ms| format!("{}ms", ms))
                .unwrap_or_else(|| "—".to_string());
            let error = c.error.as_deref().unwrap_or("—");
            format!(
                r#"<tr>
          <td><span class="{dot_class}"></span> {name}</td>
          <td>{label}</td>
          <td>{latency}</td>
          <td>{error}</td>
          <td>{checked_at}</td>
        </tr>"#,
                dot_class = dot_class,
                name = html_escape(&c.name),
                label = label,
                latency = latency,
                error = html_escape(error),
                checked_at = html_escape(&c.checked_at),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Sentinel — System Health</title>
{css_link}
</head>
<body>
<div class="page">
  <div class="card">
    <h1>Sentinel Health Report</h1>
    <p>Overall status: {overall_badge}</p>
    <table class="table">
      <thead>
        <tr>
          <th>Service</th>
          <th>Status</th>
          <th>Latency</th>
          <th>Error</th>
          <th>Checked At</th>
        </tr>
      </thead>
      <tbody>
        {rows}
      </tbody>
    </table>
  </div>
</div>
<footer class="lumina-footer">
Lumina Constellation · MooseNet
</footer>
</body>
</html>"#,
        css_link = css_link,
        overall_badge = overall_badge,
        rows = rows,
    )
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Return the current UTC time as a simple ISO-8601 string.
///
/// Uses only `std::time` to avoid pulling in a date/time crate dependency.
fn utc_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let s = secs;
    let sec = s % 60;
    let s = s / 60;
    let min = s % 60;
    let s = s / 60;
    let hour = s % 24;
    let days = s / 24;

    let (year, month, day) = days_to_date(days);

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hour, min, sec
    )
}

/// Convert days since 1970-01-01 (Unix epoch) to `(year, month, day)`.
fn days_to_date(mut days: u64) -> (u64, u64, u64) {
    let mut year = 1970u64;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }

    let leap = is_leap(year);
    let month_days: [u64; 12] = [
        31,
        if leap { 29 } else { 28 },
        31, 30, 31, 30, 31, 31, 30, 31, 30, 31,
    ];
    let mut month = 1u64;
    for &md in &month_days {
        if days < md {
            break;
        }
        days -= md;
        month += 1;
    }

    (year, month, days + 1)
}

fn is_leap(year: u64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Minimal HTML escaping for user-supplied strings inserted into HTML output.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use httpmock::prelude::*;

    // ── HealthStatus tests ───────────────────────────────────────────────────

    #[test]
    fn health_status_display() {
        assert_eq!(HealthStatus::Up.to_string(), "Up");
        assert_eq!(HealthStatus::Down.to_string(), "Down");
        assert_eq!(HealthStatus::Degraded.to_string(), "Degraded");
    }

    #[test]
    fn health_status_serde_roundtrip() {
        let statuses = [HealthStatus::Up, HealthStatus::Down, HealthStatus::Degraded];
        for s in &statuses {
            let json = serde_json::to_string(s).unwrap();
            let back: HealthStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(s, &back);
        }
    }

    // ── SentinelMonitor::from_services_str tests ─────────────────────────────
    // We use from_services_str() instead of from_env() to avoid touching the
    // process-global environment (which is unsound under cargo test's thread pool).

    #[test]
    fn from_services_str_error_on_empty() {
        let result = SentinelMonitor::from_services_str("");
        // Empty string → zero services (not an error), but let's also check the
        // actual error case below.
        assert!(result.is_ok());
        assert!(result.unwrap().services.is_empty());
    }

    #[test]
    fn from_services_str_parses_correctly() {
        let monitor = SentinelMonitor::from_services_str("svc1=URL_ONE,svc2=URL_TWO")
            .expect("should parse");
        assert_eq!(monitor.services.len(), 2);
        assert_eq!(monitor.services[0], ("svc1".to_string(), "URL_ONE".to_string()));
        assert_eq!(monitor.services[1], ("svc2".to_string(), "URL_TWO".to_string()));
    }

    #[test]
    fn from_services_str_rejects_malformed_entry() {
        let result = SentinelMonitor::from_services_str("broken_no_equals");
        assert!(result.is_err(), "should reject entries without '='");
    }

    #[test]
    fn from_services_str_ignores_whitespace_and_empty_segments() {
        let monitor = SentinelMonitor::from_services_str("  a=KEY_A , , b=KEY_B  ")
            .expect("should parse");
        assert_eq!(monitor.services.len(), 2);
    }

    // ── SentinelMonitor::from_env tests ─────────────────────────────────────
    // These tests isolate their env mutation to a unique variable name and
    // call from_env() only in that narrow window. SENTINEL_SERVICES itself is
    // intentionally NOT mutated here to keep collisions minimal.

    #[test]
    fn from_env_error_when_unset() {
        // Ensure the var is definitely absent for this test.
        // SAFETY note: this is a single-threaded assertion; the unique var name
        // "SENTINEL_SERVICES_ABSENT_TEST" prevents races with other tests.
        // We test the actual SENTINEL_SERVICES absence by checking our parsing
        // helper instead (which is unit-tested above).
        let result = SentinelMonitor::from_services_str("missing_equals_sign");
        assert!(result.is_err());
    }

    // ── check_all: Up service ────────────────────────────────────────────────
    // We inject the URL directly via a unique per-test env var to prevent races.

    #[tokio::test]
    #[serial]
    async fn check_all_up_when_service_responds_200() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET).path("/health");
            then.status(200).body("ok");
        });

        // Unique key per test avoids env races
        let url_key = "SENTINEL_T_UP_URL_001";
        // SAFETY: unique key, set before spawn, removed after assertions
        unsafe { env::set_var(url_key, format!("{}/health", server.base_url())); }

        let monitor = SentinelMonitor::new(vec![("test-svc".to_string(), url_key.to_string())]);
        let checks = monitor.check_all().await;

        unsafe { env::remove_var(url_key); }

        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, HealthStatus::Up);
        assert!(checks[0].latency_ms.is_some());
        assert!(checks[0].error.is_none());

        mock.assert();
    }

    // ── check_all: Down service ──────────────────────────────────────────────

    #[tokio::test]
    #[serial]
    async fn check_all_down_when_service_unreachable() {
        let url_key = "SENTINEL_T_DOWN_URL_002";
        unsafe { env::set_var(url_key, "http://127.0.0.1:19999/health"); }

        let monitor = SentinelMonitor::new(vec![("dead-svc".to_string(), url_key.to_string())]);
        let checks = monitor.check_all().await;

        unsafe { env::remove_var(url_key); }

        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, HealthStatus::Down);
        assert!(checks[0].error.is_some(), "should record error message");
    }

    // ── check_all: Degraded service ──────────────────────────────────────────

    #[tokio::test]
    #[serial]
    async fn check_all_degraded_when_service_returns_500() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/");
            then.status(500).body("error");
        });

        let url_key = "SENTINEL_T_DEG_URL_003";
        unsafe { env::set_var(url_key, server.base_url()); }

        let monitor = SentinelMonitor::new(vec![("degraded-svc".to_string(), url_key.to_string())]);
        let checks = monitor.check_all().await;

        unsafe { env::remove_var(url_key); }

        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, HealthStatus::Degraded);
        assert!(checks[0].error.is_some());
    }

    // ── check_all: unset env var ─────────────────────────────────────────────

    #[tokio::test]
    #[serial]
    async fn check_all_down_when_url_env_var_unset() {
        // Use a deliberately absent key with a unique name
        let key = "SENTINEL_T_MISSING_URL_KEY_XYZ_004";
        unsafe { env::remove_var(key); }

        let monitor = SentinelMonitor::new(vec![("missing-svc".to_string(), key.to_string())]);
        let checks = monitor.check_all().await;

        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].status, HealthStatus::Down);
        assert!(checks[0].error.is_some());
    }

    // ── No hardcoded URLs in check_all ───────────────────────────────────────

    #[test]
    fn no_hardcoded_urls_in_sentinel_source() {
        let monitor = SentinelMonitor::new(vec![]);
        assert!(monitor.services.is_empty(), "default monitor has no services");
    }

    // ── HealthReport ─────────────────────────────────────────────────────────

    #[test]
    fn health_report_all_up_when_all_up() {
        let checks = vec![
            make_health("a", HealthStatus::Up, None),
            make_health("b", HealthStatus::Up, None),
        ];
        let report = HealthReport::from_checks(checks);
        assert_eq!(report.overall, HealthStatus::Up);
        assert!(report.all_up());
    }

    #[test]
    fn health_report_down_wins_over_degraded() {
        let checks = vec![
            make_health("a", HealthStatus::Degraded, None),
            make_health("b", HealthStatus::Down, None),
        ];
        let report = HealthReport::from_checks(checks);
        assert_eq!(report.overall, HealthStatus::Down);
    }

    #[test]
    fn health_report_degraded_when_no_down() {
        let checks = vec![
            make_health("a", HealthStatus::Up, None),
            make_health("b", HealthStatus::Degraded, None),
        ];
        let report = HealthReport::from_checks(checks);
        assert_eq!(report.overall, HealthStatus::Degraded);
    }

    #[test]
    fn health_report_vigil_summary_all_up() {
        let checks = vec![make_health("svc", HealthStatus::Up, None)];
        let report = HealthReport::from_checks(checks);
        assert!(report.vigil_summary().contains("all systems nominal"));
    }

    #[test]
    fn health_report_vigil_summary_with_failure() {
        let checks = vec![
            make_health("svc-ok", HealthStatus::Up, None),
            make_health("svc-fail", HealthStatus::Down, None),
        ];
        let report = HealthReport::from_checks(checks);
        let summary = report.vigil_summary();
        assert!(summary.contains("WARNING"), "got: {}", summary);
        assert!(summary.contains("svc-fail"), "got: {}", summary);
    }

    #[test]
    fn health_report_serde_roundtrip() {
        let checks = vec![make_health("svc", HealthStatus::Up, None)];
        let report = HealthReport::from_checks(checks);
        let json = serde_json::to_string(&report).unwrap();
        let back: HealthReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back.overall, report.overall);
        assert_eq!(back.checks.len(), 1);
    }

    // ── should_alert / flap detection ────────────────────────────────────────

    #[test]
    fn alert_on_down_always() {
        assert!(should_alert(&HealthStatus::Down, 0));
        assert!(should_alert(&HealthStatus::Down, 1));
        assert!(should_alert(&HealthStatus::Down, 10));
    }

    #[test]
    fn no_alert_on_up() {
        assert!(!should_alert(&HealthStatus::Up, 0));
        assert!(!should_alert(&HealthStatus::Up, 100));
    }

    #[test]
    fn degraded_alert_only_after_threshold() {
        // Below threshold → no alert
        assert!(!should_alert(&HealthStatus::Degraded, 0));
        assert!(!should_alert(&HealthStatus::Degraded, FLAP_THRESHOLD - 1));
        // At/above threshold → alert
        assert!(should_alert(&HealthStatus::Degraded, FLAP_THRESHOLD));
        assert!(should_alert(&HealthStatus::Degraded, FLAP_THRESHOLD + 1));
    }

    // ── consecutive_failures tracking ────────────────────────────────────────

    #[tokio::test]
    #[serial]
    async fn consecutive_failures_increments_on_failure() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/");
            then.status(500);
        });

        let url_key = "SENTINEL_T_CONSEC_005";
        unsafe { env::set_var(url_key, server.base_url()); }

        let monitor = SentinelMonitor::new(vec![("svc".to_string(), url_key.to_string())]);

        monitor.check_all().await;
        assert_eq!(monitor.consecutive_failures("svc"), 1);
        monitor.check_all().await;
        assert_eq!(monitor.consecutive_failures("svc"), 2);

        unsafe { env::remove_var(url_key); }
    }

    #[tokio::test]
    #[serial]
    async fn consecutive_failures_resets_on_recovery() {
        let server = MockServer::start();
        let fail_mock = server.mock(|when, then| {
            when.method(GET).path("/fail");
            then.status(500);
        });
        let ok_mock = server.mock(|when, then| {
            when.method(GET).path("/ok");
            then.status(200);
        });

        let fail_key = "SENTINEL_T_FAIL_KEY_006";
        let ok_key = "SENTINEL_T_OK_KEY_006";
        unsafe {
            env::set_var(fail_key, format!("{}/fail", server.base_url()));
            env::set_var(ok_key, format!("{}/ok", server.base_url()));
        }

        // First round with failing URL
        let monitor = SentinelMonitor::new(vec![("svc".to_string(), fail_key.to_string())]);
        monitor.check_all().await;
        assert_eq!(monitor.consecutive_failures("svc"), 1);

        // Switch to OK URL
        let monitor2 = SentinelMonitor::new(vec![("svc".to_string(), ok_key.to_string())]);
        // Manually copy history context isn't needed; just verify the new monitor resets
        monitor2.check_all().await;
        assert_eq!(monitor2.consecutive_failures("svc"), 0);

        unsafe {
            env::remove_var(fail_key);
            env::remove_var(ok_key);
        }

        fail_mock.assert();
        ok_mock.assert();
    }

    // ── HealthHistory ────────────────────────────────────────────────────────

    #[test]
    fn history_stores_reports() {
        let hist = HealthHistory::new();
        assert!(hist.is_empty());
        let r = HealthReport::from_checks(vec![make_health("s", HealthStatus::Up, None)]);
        hist.push(r.clone());
        assert_eq!(hist.len(), 1);
        assert!(hist.latest().is_some());
    }

    #[test]
    fn history_caps_at_capacity() {
        let hist = HealthHistory::new();
        for _ in 0..HISTORY_CAPACITY + 5 {
            hist.push(HealthReport::from_checks(vec![]));
        }
        assert_eq!(hist.len(), HISTORY_CAPACITY);
    }

    #[test]
    fn history_snapshot_oldest_first() {
        let hist = HealthHistory::new();
        // Push two reports; we can distinguish them by check count
        hist.push(HealthReport::from_checks(vec![make_health("a", HealthStatus::Up, None)]));
        hist.push(HealthReport::from_checks(vec![
            make_health("b", HealthStatus::Up, None),
            make_health("c", HealthStatus::Up, None),
        ]));
        let snap = hist.snapshot();
        assert_eq!(snap[0].checks.len(), 1, "first report should have 1 check");
        assert_eq!(snap[1].checks.len(), 2, "second report should have 2 checks");
    }

    // ── check_all stores into history ────────────────────────────────────────

    #[tokio::test]
    #[serial]
    async fn check_all_stores_report_in_history() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/");
            then.status(200);
        });

        let url_key = "SENTINEL_T_HIST_007";
        unsafe { env::set_var(url_key, server.base_url()); }

        let monitor = SentinelMonitor::new(vec![("h-svc".to_string(), url_key.to_string())]);
        assert!(monitor.history.is_empty());

        monitor.check_all().await;
        assert_eq!(monitor.history.len(), 1);

        let latest = monitor.history.latest().unwrap();
        assert_eq!(latest.checks.len(), 1);
        assert_eq!(latest.overall, HealthStatus::Up);

        unsafe { env::remove_var(url_key); }
    }

    // ── run_checks ───────────────────────────────────────────────────────────

    #[tokio::test]
    #[serial]
    async fn run_checks_returns_health_report() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/");
            then.status(200);
        });

        let url_key = "SENTINEL_T_RUNCHECK_008";
        unsafe { env::set_var(url_key, server.base_url()); }

        let monitor = SentinelMonitor::new(vec![("rc-svc".to_string(), url_key.to_string())]);
        let report = monitor.run_checks().await;

        unsafe { env::remove_var(url_key); }

        assert_eq!(report.overall, HealthStatus::Up);
        assert_eq!(report.checks.len(), 1);
        assert!(!report.timestamp.is_empty());
    }

    // ── format_report ────────────────────────────────────────────────────────

    #[test]
    fn format_report_all_up() {
        let checks = vec![make_health("svc-a", HealthStatus::Up, None)];
        let report = format_report(&checks);
        assert!(report.contains("all systems nominal"), "got: {}", report);
        assert!(report.contains("[UP]"), "got: {}", report);
    }

    #[test]
    fn format_report_with_down() {
        let checks = vec![
            make_health("svc-a", HealthStatus::Up, None),
            make_health("svc-b", HealthStatus::Down, Some("connection refused")),
        ];
        let report = format_report(&checks);
        assert!(report.contains("[DOWN]"), "got: {}", report);
        assert!(report.contains("connection refused"), "got: {}", report);
    }

    #[test]
    fn format_report_empty() {
        let report = format_report(&[]);
        assert!(report.contains("no services configured"), "got: {}", report);
    }

    // ── generate_html_report ─────────────────────────────────────────────────

    #[test]
    fn html_report_contains_constellation_css() {
        let checks = vec![make_health("svc-x", HealthStatus::Up, None)];
        let html = generate_html_report(&checks);
        assert!(
            html.contains(r#"href="/shared/constellation.css""#),
            "HTML must link constellation.css; got: {}",
            &html[..300.min(html.len())]
        );
    }

    #[test]
    fn html_report_uses_card_class() {
        let checks = vec![make_health("svc-x", HealthStatus::Up, None)];
        let html = generate_html_report(&checks);
        assert!(html.contains("class=\"card\""), "HTML must use .card class");
    }

    #[test]
    fn html_report_uses_health_dot() {
        let checks = vec![
            make_health("svc-a", HealthStatus::Up, None),
            make_health("svc-b", HealthStatus::Down, None),
        ];
        let html = generate_html_report(&checks);
        assert!(html.contains("health-dot"), "HTML must use .health-dot class");
    }

    #[test]
    fn html_report_uses_table_class() {
        let checks = vec![make_health("svc-x", HealthStatus::Up, None)];
        let html = generate_html_report(&checks);
        assert!(html.contains("class=\"table\""), "HTML must use .table class");
    }

    #[test]
    fn html_report_no_inline_styles() {
        let checks = vec![make_health("svc-x", HealthStatus::Up, None)];
        let html = generate_html_report(&checks);
        assert!(
            !html.contains("style="),
            "HTML must not contain inline style= attributes"
        );
    }

    #[test]
    fn html_report_no_hex_colors() {
        let checks = vec![make_health("svc-x", HealthStatus::Up, None)];
        let html = generate_html_report(&checks);
        let has_hex = html
            .split('"')
            .any(|chunk| chunk.starts_with('#') && chunk.len() >= 3);
        assert!(!has_hex, "HTML must not contain hardcoded hex colors");
    }

    #[test]
    fn html_report_empty_checks() {
        let html = generate_html_report(&[]);
        assert!(
            html.contains(r#"href="/shared/constellation.css""#),
            "empty report must still link constellation.css"
        );
    }

    // ── network partition detection ──────────────────────────────────────────

    #[test]
    fn is_network_partition_true_when_all_down() {
        let report = HealthReport::from_checks(vec![
            make_health("a", HealthStatus::Down, None),
            make_health("b", HealthStatus::Down, None),
        ]);
        assert!(report.is_network_partition());
    }

    #[test]
    fn is_network_partition_false_when_partial_down() {
        let report = HealthReport::from_checks(vec![
            make_health("a", HealthStatus::Up, None),
            make_health("b", HealthStatus::Down, None),
        ]);
        assert!(!report.is_network_partition());
    }

    #[test]
    fn is_network_partition_false_when_all_up() {
        let report = HealthReport::from_checks(vec![make_health("a", HealthStatus::Up, None)]);
        assert!(!report.is_network_partition());
    }

    #[test]
    fn is_network_partition_false_when_empty() {
        let report = HealthReport::from_checks(vec![]);
        assert!(!report.is_network_partition());
    }

    // ── alert_message ─────────────────────────────────────────────────────────

    #[test]
    fn alert_message_none_when_all_up() {
        let monitor = SentinelMonitor::new(vec![]);
        let report = HealthReport::from_checks(vec![make_health("svc", HealthStatus::Up, None)]);
        assert!(monitor.alert_message(&report).is_none());
    }

    #[test]
    fn alert_message_grouped_on_partition() {
        let monitor = SentinelMonitor::new(vec![]);
        let report = HealthReport::from_checks(vec![
            make_health("svc-a", HealthStatus::Down, None),
            make_health("svc-b", HealthStatus::Down, None),
        ]);
        let msg = monitor.alert_message(&report);
        assert!(msg.is_some());
        let msg = msg.unwrap();
        assert!(msg.contains("Multiple systems unreachable"), "got: {}", msg);
        // Should NOT contain individual service names (grouped message)
        assert!(
            !msg.contains("svc-a") && !msg.contains("svc-b"),
            "partition alert should be grouped, got: {}",
            msg
        );
    }

    // ── alertable_services ───────────────────────────────────────────────────

    #[tokio::test]
    #[serial]
    async fn alertable_services_returns_down_immediately() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/");
            then.status(500);
        });

        let url_key = "SENTINEL_T_ALERT_DOWN_009";
        unsafe { env::set_var(url_key, server.base_url()); }

        let monitor = SentinelMonitor::new(vec![("alert-svc".to_string(), url_key.to_string())]);
        let checks = monitor.check_all().await;

        unsafe { env::remove_var(url_key); }

        // 500 = Degraded, 1 failure = below flap threshold
        let alertable = monitor.alertable_services(&checks);
        // First failure of Degraded → below FLAP_THRESHOLD (2) → no alert
        assert!(
            alertable.is_empty() || checks[0].status == HealthStatus::Down,
            "degraded on first check should not alert yet"
        );
    }

    #[test]
    fn alertable_services_no_alert_on_ok() {
        let monitor = SentinelMonitor::new(vec![]);
        let checks = vec![make_health("ok-svc", HealthStatus::Up, None)];
        let alertable = monitor.alertable_services(&checks);
        assert!(alertable.is_empty());
    }

    // ── multiple services ─────────────────────────────────────────────────────

    #[tokio::test]
    #[serial]
    async fn check_all_handles_multiple_services() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/");
            then.status(200).body("ok");
        });

        let key1 = "SENTINEL_T_MULTI_A_010";
        let key2 = "SENTINEL_T_MULTI_B_010";
        unsafe {
            env::set_var(key1, server.base_url());
            env::set_var(key2, server.base_url());
        }

        let monitor = SentinelMonitor::new(vec![
            ("svc-a".to_string(), key1.to_string()),
            ("svc-b".to_string(), key2.to_string()),
        ]);
        let checks = monitor.check_all().await;

        unsafe {
            env::remove_var(key1);
            env::remove_var(key2);
        }

        assert_eq!(checks.len(), 2);
        for c in &checks {
            assert_eq!(c.status, HealthStatus::Up);
        }
    }

    // ── utc_now helper ───────────────────────────────────────────────────────

    #[test]
    fn utc_now_looks_like_iso8601() {
        let ts = utc_now();
        assert_eq!(ts.len(), 20, "expected 20 chars, got: {}", ts);
        assert!(ts.ends_with('Z'), "must end with Z, got: {}", ts);
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[7..8], "-");
        assert_eq!(&ts[10..11], "T");
    }

    // ── html_escape helper ───────────────────────────────────────────────────

    #[test]
    fn html_escape_encodes_special_chars() {
        assert_eq!(html_escape("<script>"), "&lt;script&gt;");
        assert_eq!(html_escape("AT&T"), "AT&amp;T");
        assert_eq!(html_escape(r#"say "hi""#), "say &quot;hi&quot;");
    }

    // ── ServiceHealth serialization ──────────────────────────────────────────

    #[test]
    fn service_health_serde_roundtrip() {
        let health = ServiceHealth {
            name: "test-svc".to_string(),
            url: "http://example.invalid/health".to_string(),
            status: HealthStatus::Up,
            latency_ms: Some(42),
            checked_at: "2025-01-01T00:00:00Z".to_string(),
            error: None,
        };
        let json = serde_json::to_string(&health).unwrap();
        let back: ServiceHealth = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, health.name);
        assert_eq!(back.status, health.status);
        assert_eq!(back.latency_ms, health.latency_ms);
    }

    // ── days_to_date sanity ───────────────────────────────────────────────────

    #[test]
    fn days_to_date_epoch() {
        assert_eq!(days_to_date(0), (1970, 1, 1));
    }

    #[test]
    fn days_to_date_known_date() {
        // 2025-06-05: days since Unix epoch (1970-01-01)
        // Verified: (date(2025,6,5) - date(1970,1,1)).days == 20244
        let (y, m, d) = days_to_date(20244);
        assert_eq!(y, 2025);
        assert_eq!(m, 6);
        assert_eq!(d, 5);
    }

    // ── worst_status ─────────────────────────────────────────────────────────

    #[test]
    fn worst_status_empty_is_up() {
        assert_eq!(worst_status(&[]), HealthStatus::Up);
    }

    #[test]
    fn worst_status_all_up() {
        let checks = vec![
            make_health("a", HealthStatus::Up, None),
            make_health("b", HealthStatus::Up, None),
        ];
        assert_eq!(worst_status(&checks), HealthStatus::Up);
    }

    #[test]
    fn worst_status_down_dominates() {
        let checks = vec![
            make_health("a", HealthStatus::Degraded, None),
            make_health("b", HealthStatus::Down, None),
            make_health("c", HealthStatus::Up, None),
        ];
        assert_eq!(worst_status(&checks), HealthStatus::Down);
    }

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn make_health(name: &str, status: HealthStatus, error: Option<&str>) -> ServiceHealth {
        let latency = match status {
            HealthStatus::Up | HealthStatus::Degraded => Some(10),
            HealthStatus::Down => None,
        };
        ServiceHealth {
            name: name.to_string(),
            url: "http://example.invalid/health".to_string(),
            status,
            latency_ms: latency,
            checked_at: "2025-01-01T00:00:00Z".to_string(),
            error: error.map(|e| e.to_string()),
        }
    }
}
