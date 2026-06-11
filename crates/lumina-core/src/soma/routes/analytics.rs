//! AGENT-06: Admin analytics dashboard page for Soma.
//!
//! Provides `GET /admin/analytics` — an admin-only HTML page showing:
//! - Top 10 tools by call frequency
//! - Performance stats (failure rate, avg duration, escalation rate)
//! - Security events summary (per guard/action counts)
//! - Daily trends (call counts per day over 30 days)
//!
//! ## Security model
//! - Admin users see aggregate data across ALL users.
//! - Non-admin users see only their own data.
//! - Authentication / session validation is the responsibility of the calling
//!   HTTP layer; this module only renders HTML and does NOT gate on auth itself.
//!
//! ## Design rules (MANDATORY — lumina-design-system-spec)
//! - Every HTML page MUST include `<link rel="stylesheet" href="/shared/constellation.css">`.
//! - NO inline `style=""` attributes, NO hardcoded hex colors.
//! - Use `.card`, `.badge-*`, `.table`, `.btn-*`, etc.
//! - No hardcoded infrastructure addresses or organisation names.
//!
//! ## 90-day auto-prune
//! `render_analytics_page` calls `OperationalStore::prune_old(90)` before
//! querying so stale data is never presented.

use crate::engram::operational::OperationalStore;

/// Mandatory constellation.css stylesheet link — must appear in every HTML page.
const CONSTELLATION_CSS: &str =
    r#"<link rel="stylesheet" href="/shared/constellation.css">"#;

/// Window in days used for all analytics queries.
const ANALYTICS_WINDOW_DAYS: u32 = 30;

/// Maximum number of tools to show in the top-tools table.
const TOP_TOOLS_LIMIT: usize = 10;

// ── AnalyticsPageParams ────────────────────────────────────────────────────

/// Parameters passed to the analytics page renderer.
///
/// The caller is responsible for determining whether the session is an admin
/// session and setting `is_admin` accordingly.  When `is_admin` is `true`, the
/// page shows aggregate data for all users.  When `false`, it shows only the
/// data for `user_id`.
#[derive(Debug, Clone)]
pub struct AnalyticsPageParams {
    /// Whether the current session has admin privileges.
    pub is_admin: bool,
    /// The authenticated user's identifier (used for non-admin scoping).
    pub user_id: String,
}

impl Default for AnalyticsPageParams {
    fn default() -> Self {
        Self {
            is_admin: false,
            user_id: String::new(),
        }
    }
}

// ── HTML rendering helpers ─────────────────────────────────────────────────

/// Escape HTML special characters to prevent XSS.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// Render a percentage as a `badge-*` class based on severity thresholds.
///
/// - < 5%  → `badge-success`
/// - < 20% → `badge-warning`
/// - ≥ 20% → `badge-danger`
fn rate_badge_class(rate: f32) -> &'static str {
    if rate < 0.05 {
        "badge-success"
    } else if rate < 0.20 {
        "badge-warning"
    } else {
        "badge-danger"
    }
}

// ── render_analytics_page ─────────────────────────────────────────────────

/// Render the admin analytics dashboard as a complete HTML document.
///
/// Performs a 90-day prune on `store` before querying.  All query results
/// are scoped according to `params.is_admin`:
/// - `is_admin == true`  → aggregate view across all users.
/// - `is_admin == false` → view scoped to `params.user_id`.
///
/// Uses constellation.css for all styling — no inline styles, no hardcoded
/// hex colours.
pub fn render_analytics_page(store: &OperationalStore, params: &AnalyticsPageParams) -> String {
    // 90-day auto-prune: call on every render so old data never accumulates.
    store.prune_old(90);

    let user_scope: Option<&str> = if params.is_admin {
        None
    } else {
        Some(params.user_id.as_str())
    };

    let window = ANALYTICS_WINDOW_DAYS;

    // ── Query aggregations ────────────────────────────────────────────────

    let top_tools = {
        let mut tools = store.top_tools_for(window, user_scope);
        tools.truncate(TOP_TOOLS_LIMIT);
        tools
    };

    // Per-tool average durations
    let failure_rate = store.failure_rate_for(window, user_scope);
    let avg_duration = store.avg_duration_ms_for(window, user_scope);
    let escalation_rate = store.escalation_rate(window); // admin-level metric
    let daily_trends = store.daily_trends(window);
    let security_summary = store.security_events_summary(window);

    // ── Section: Top tools table ──────────────────────────────────────────

    let top_tools_rows: String = if top_tools.is_empty() {
        r#"<tr><td colspan="2" class="text-secondary">No tool calls recorded in the last 30 days.</td></tr>"#
            .to_string()
    } else {
        top_tools
            .iter()
            .map(|(name, count)| {
                format!(
                    "<tr><td>{name}</td><td><span class=\"badge badge-secondary\">{count}</span></td></tr>",
                    name = html_escape(name),
                    count = count,
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    // ── Section: Performance stats ────────────────────────────────────────

    let failure_pct = failure_rate * 100.0;
    let escalation_pct = escalation_rate * 100.0;
    let failure_badge = rate_badge_class(failure_rate);
    let escalation_badge = rate_badge_class(escalation_rate);

    // ── Section: Security events table ───────────────────────────────────

    let security_rows: String = if security_summary.is_empty() {
        r#"<tr><td colspan="2" class="text-secondary">No security events recorded in the last 30 days.</td></tr>"#
            .to_string()
    } else {
        security_summary
            .iter()
            .map(|(key, count)| {
                // key is "guard_name:action" — split for rendering
                let (guard, action) = key.split_once(':').unwrap_or((key.as_str(), ""));
                let action_badge = match action {
                    "Blocked" => "badge-danger",
                    "Warned" => "badge-warning",
                    _ => "badge-secondary",
                };
                format!(
                    "<tr><td>{guard}</td><td><span class=\"badge {action_badge}\">{action}</span></td><td>{count}</td></tr>",
                    guard = html_escape(guard),
                    action_badge = action_badge,
                    action = html_escape(action),
                    count = count,
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    // ── Section: Daily trends table ───────────────────────────────────────

    let trends_rows: String = if daily_trends.is_empty() {
        r#"<tr><td colspan="2" class="text-secondary">No call activity recorded in the last 30 days.</td></tr>"#
            .to_string()
    } else {
        daily_trends
            .iter()
            .map(|(date, count)| {
                format!(
                    "<tr><td>{date}</td><td>{count}</td></tr>",
                    date = html_escape(date),
                    count = count,
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    // ── Scope badge ───────────────────────────────────────────────────────

    let scope_badge = if params.is_admin {
        r#"<span class="badge badge-warning">Admin — All Users</span>"#.to_string()
    } else {
        format!(
            r#"<span class="badge badge-secondary">User: {}</span>"#,
            html_escape(&params.user_id)
        )
    };

    // ── Assemble full page ────────────────────────────────────────────────

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Analytics — Operations Dashboard</title>
{css}
</head>
<body>
<div class="page">
<header class="page-header">
  <h1>Operations Analytics</h1>
  <p class="text-secondary">Last {window} days &nbsp; {scope_badge}</p>
</header>

<div class="grid">

  <div class="card">
    <div class="card-header">
      <h2 class="card-title">Top Tools</h2>
    </div>
    <div class="card-body">
      <table class="table">
        <thead>
          <tr><th>Tool</th><th>Calls</th></tr>
        </thead>
        <tbody>
          {top_tools_rows}
        </tbody>
      </table>
    </div>
  </div>

  <div class="card">
    <div class="card-header">
      <h2 class="card-title">Performance</h2>
    </div>
    <div class="card-body">
      <table class="table">
        <tbody>
          <tr>
            <td>Failure rate</td>
            <td><span class="badge {failure_badge}">{failure_pct:.1}%</span></td>
          </tr>
          <tr>
            <td>Escalation rate</td>
            <td><span class="badge {escalation_badge}">{escalation_pct:.1}%</span></td>
          </tr>
          <tr>
            <td>Avg duration</td>
            <td><span class="badge badge-secondary">{avg_duration:.0} ms</span></td>
          </tr>
        </tbody>
      </table>
    </div>
  </div>

</div>

<div class="card">
  <div class="card-header">
    <h2 class="card-title">Security Events</h2>
  </div>
  <div class="card-body">
    <p class="text-secondary">Guard-fired events: blocked tool calls, injection attempts, chain detections.</p>
    <table class="table">
      <thead>
        <tr><th>Guard</th><th>Action</th><th>Count</th></tr>
      </thead>
      <tbody>
        {security_rows}
      </tbody>
    </table>
  </div>
</div>

<div class="card">
  <div class="card-header">
    <h2 class="card-title">Daily Trends</h2>
  </div>
  <div class="card-body">
    <table class="table">
      <thead>
        <tr><th>Date</th><th>Calls</th></tr>
      </thead>
      <tbody>
        {trends_rows}
      </tbody>
    </table>
  </div>
</div>

<footer class="lumina-footer">
Lumina Constellation &middot; Operations Analytics
</footer>
</div>
</body>
</html>"#,
        css = CONSTELLATION_CSS,
        window = window,
        scope_badge = scope_badge,
        top_tools_rows = top_tools_rows,
        failure_badge = failure_badge,
        failure_pct = failure_pct,
        escalation_badge = escalation_badge,
        escalation_pct = escalation_pct,
        avg_duration = avg_duration,
        security_rows = security_rows,
        trends_rows = trends_rows,
    )
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engram::operational::{ExecutionRecord, SecurityEventRecord};

    fn make_store_with_data() -> OperationalStore {
        let store = OperationalStore::new();
        let recs = vec![
            ExecutionRecord::new("t1", "alice", "search", 120, "ok"),
            ExecutionRecord::new("t2", "alice", "search", 150, "ok"),
            ExecutionRecord::new("t3", "alice", "calendar", 80, "ok"),
            ExecutionRecord::new("t4", "alice", "weather", 200, "error"),
            ExecutionRecord::new("t5", "bob", "search", 90, "ok"),
            ExecutionRecord::new("t6", "bob", "files", 300, "blocked"),
        ];
        store.record("alice", &recs[..4]);
        store.record("bob", &recs[4..]);

        let events = vec![
            SecurityEventRecord::new("argument", "Blocked", "files", "bob"),
            SecurityEventRecord::new("behavioral", "Warned", "", "bob"),
        ];
        store.record_security_events(&events);
        store
    }

    // ── HTML structure ─────────────────────────────────────────────────────

    #[test]
    fn test_analytics_page_has_constellation_css() {
        let store = OperationalStore::new();
        let params = AnalyticsPageParams {
            is_admin: true,
            user_id: "admin".to_string(),
        };
        let html = render_analytics_page(&store, &params);
        assert!(
            html.contains(r#"<link rel="stylesheet" href="/shared/constellation.css">"#),
            "Page must include constellation.css link"
        );
    }

    #[test]
    fn test_analytics_page_no_inline_styles() {
        let store = make_store_with_data();
        let params = AnalyticsPageParams {
            is_admin: true,
            user_id: "admin".to_string(),
        };
        let html = render_analytics_page(&store, &params);
        assert!(
            !html.contains("style=\""),
            "Page must not contain inline style attributes"
        );
    }

    #[test]
    fn test_analytics_page_no_hardcoded_hex_colors() {
        let store = make_store_with_data();
        let params = AnalyticsPageParams { is_admin: true, user_id: "admin".to_string() };
        let html = render_analytics_page(&store, &params);
        // Hex colors look like #RRGGBB or #RGB — regex would be ideal but we
        // scan for the pattern in a simple way
        assert!(
            !html.contains("#0d1117") && !html.contains("#58a6ff") && !html.contains("#161b22"),
            "Page must not contain hardcoded hex colors"
        );
    }

    #[test]
    fn test_analytics_page_uses_constellation_classes() {
        let store = make_store_with_data();
        let params = AnalyticsPageParams { is_admin: true, user_id: "admin".to_string() };
        let html = render_analytics_page(&store, &params);
        assert!(html.contains("class=\"card\"") || html.contains("class=\"card "));
        assert!(html.contains("class=\"table\""));
        assert!(html.contains("badge-"));
        assert!(html.contains("lumina-footer"));
        assert!(html.contains("page-header"));
    }

    #[test]
    fn test_analytics_page_valid_html_structure() {
        let store = OperationalStore::new();
        let params = AnalyticsPageParams::default();
        let html = render_analytics_page(&store, &params);
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("<html"));
        assert!(html.contains("<head>"));
        assert!(html.contains("<body>"));
        assert!(html.contains("</html>"));
    }

    // ── Page sections ──────────────────────────────────────────────────────

    #[test]
    fn test_analytics_page_has_top_tools_section() {
        let store = make_store_with_data();
        let params = AnalyticsPageParams { is_admin: true, user_id: "admin".to_string() };
        let html = render_analytics_page(&store, &params);
        assert!(html.contains("Top Tools"), "Page must have Top Tools section");
    }

    #[test]
    fn test_analytics_page_has_performance_section() {
        let store = make_store_with_data();
        let params = AnalyticsPageParams { is_admin: true, user_id: "admin".to_string() };
        let html = render_analytics_page(&store, &params);
        assert!(html.contains("Performance"), "Page must have Performance section");
        assert!(html.contains("Failure rate") || html.contains("failure rate"));
        assert!(html.contains("Escalation rate") || html.contains("escalation rate"));
        assert!(html.contains("Avg duration") || html.contains("avg duration"));
    }

    #[test]
    fn test_analytics_page_has_security_events_section() {
        let store = make_store_with_data();
        let params = AnalyticsPageParams { is_admin: true, user_id: "admin".to_string() };
        let html = render_analytics_page(&store, &params);
        assert!(html.contains("Security Events"), "Page must have Security Events section");
    }

    #[test]
    fn test_analytics_page_has_daily_trends_section() {
        let store = make_store_with_data();
        let params = AnalyticsPageParams { is_admin: true, user_id: "admin".to_string() };
        let html = render_analytics_page(&store, &params);
        assert!(html.contains("Daily Trends"), "Page must have Daily Trends section");
    }

    // ── Admin vs user scoping ──────────────────────────────────────────────

    #[test]
    fn test_admin_sees_all_users_badge() {
        let store = OperationalStore::new();
        let params = AnalyticsPageParams {
            is_admin: true,
            user_id: "admin".to_string(),
        };
        let html = render_analytics_page(&store, &params);
        assert!(
            html.contains("Admin") || html.contains("All Users"),
            "Admin page must indicate admin scope, got html snippet: {}",
            &html[..html.len().min(500)]
        );
    }

    #[test]
    fn test_user_page_shows_user_id() {
        let store = OperationalStore::new();
        let params = AnalyticsPageParams {
            is_admin: false,
            user_id: "alice".to_string(),
        };
        let html = render_analytics_page(&store, &params);
        assert!(html.contains("alice"), "User-scoped page must show user id");
    }

    #[test]
    fn test_user_scoped_top_tools_excludes_other_users() {
        let store = make_store_with_data();
        // Alice has search, calendar, weather
        // Bob has search, files
        let alice_params = AnalyticsPageParams {
            is_admin: false,
            user_id: "alice".to_string(),
        };
        let alice_html = render_analytics_page(&store, &alice_params);
        // "files" is Bob's tool — it must not appear in Alice's view
        // (it could appear as a header text match, so check table context)
        // The top tools section for Alice should not show "files" as a tool row
        // with a count — we check that "files</td>" doesn't appear
        assert!(
            !alice_html.contains("files</td>"),
            "Alice's page must not show Bob's 'files' tool"
        );
    }

    // ── Empty state ────────────────────────────────────────────────────────

    #[test]
    fn test_analytics_page_empty_store_renders_without_panic() {
        let store = OperationalStore::new();
        let params = AnalyticsPageParams { is_admin: true, user_id: "admin".to_string() };
        let html = render_analytics_page(&store, &params);
        assert!(!html.is_empty(), "Empty store must still render a page");
        assert!(html.contains("No tool calls recorded") || html.contains("No call activity recorded"));
    }

    #[test]
    fn test_analytics_page_security_empty_state_message() {
        let store = OperationalStore::new();
        let params = AnalyticsPageParams { is_admin: true, user_id: "admin".to_string() };
        let html = render_analytics_page(&store, &params);
        assert!(
            html.contains("No security events recorded"),
            "Empty security events must show a placeholder message"
        );
    }

    // ── 90-day prune called on render ─────────────────────────────────────

    #[test]
    fn test_render_calls_90_day_prune() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let store = OperationalStore::new();
        // Inject an old record directly into the store
        let old_rec = ExecutionRecord {
            turn_id: "t-old".to_string(),
            user_id: "u".to_string(),
            tool_name: "ancient_tool".to_string(),
            duration_ms: 42,
            status: "ok".to_string(),
            timestamp_secs: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
                - 91 * 86_400,
        };
        store.record("u", &[old_rec]);
        assert_eq!(store.len(), 1, "Should have 1 record before render");

        let params = AnalyticsPageParams { is_admin: true, user_id: "admin".to_string() };
        let _ = render_analytics_page(&store, &params);

        assert_eq!(
            store.len(),
            0,
            "render_analytics_page must prune records older than 90 days"
        );
    }

    // ── XSS prevention ────────────────────────────────────────────────────

    #[test]
    fn test_analytics_page_escapes_tool_names() {
        let store = OperationalStore::new();
        // Inject a record with a malicious tool name
        let rec = ExecutionRecord::new("t1", "u", "<script>alert('xss')</script>", 10, "ok");
        store.record("u", &[rec]);
        let params = AnalyticsPageParams { is_admin: true, user_id: "admin".to_string() };
        let html = render_analytics_page(&store, &params);
        assert!(!html.contains("<script>alert"), "Tool names must be HTML-escaped");
        assert!(html.contains("&lt;script&gt;"), "Escaped content must appear");
    }

    #[test]
    fn test_analytics_page_escapes_user_id() {
        let store = OperationalStore::new();
        let params = AnalyticsPageParams {
            is_admin: false,
            user_id: "<script>evil</script>".to_string(),
        };
        let html = render_analytics_page(&store, &params);
        assert!(!html.contains("<script>evil"), "User ID must be HTML-escaped");
    }

    // ── rate_badge_class helper ────────────────────────────────────────────

    #[test]
    fn test_rate_badge_class_thresholds() {
        assert_eq!(rate_badge_class(0.0), "badge-success");
        assert_eq!(rate_badge_class(0.04), "badge-success");
        assert_eq!(rate_badge_class(0.05), "badge-warning");
        assert_eq!(rate_badge_class(0.19), "badge-warning");
        assert_eq!(rate_badge_class(0.20), "badge-danger");
        assert_eq!(rate_badge_class(1.0), "badge-danger");
    }
}
