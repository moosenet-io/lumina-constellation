//! P2-10: Soma web dashboard — server-side rendered HTML overview.
//!
//! Provides:
//! - [`render_dashboard`] — returns a complete HTML document for the `/dashboard` route
//!
//! The dashboard shows:
//! - Agent status header (version, uptime if available)
//! - Recent turns summary (configurable count, default 5)
//! - Memory stats (Engram entry count if available, else "N/A")
//! - System health summary (simple text, no live checks needed)
//!
//! ## Design rules (MANDATORY — lumina-design-system-spec)
//! - Every HTML page MUST include:
//!   `<link rel="stylesheet" href="/shared/constellation.css">`
//! - NO inline `style=""` attributes
//! - NO hardcoded hex colors
//! - NO custom `<style>` blocks duplicating constellation.css
//! - Use `.card`, `.badge-*`, `.table`, `.btn-*`, `var(--bg-primary)`, etc.

// ── Constants ─────────────────────────────────────────────────────────────────

/// Version string embedded in the dashboard header.
///
/// This reads the crate version at compile time from `Cargo.toml`; no
/// hardcoded strings and no network call needed.
const LUMINA_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Mandatory constellation.css link — must appear in every HTML page.
const CONSTELLATION_CSS: &str =
    r#"<link rel="stylesheet" href="/shared/constellation.css">"#;

// ── Dashboard parameters ───────────────────────────────────────────────────────

/// Configuration for the dashboard render.
///
/// All fields default to sensible values — callers only need to set what they
/// have available.  All infrastructure addresses come from the caller (env vars
/// or config structs); none are hardcoded here.
#[derive(Debug, Clone)]
pub struct DashboardParams {
    /// Number of recent turns to display. Default: 5.
    pub recent_turns_count: usize,
    /// Optional uptime string (e.g. "3 days, 4h 12m").
    pub uptime: Option<String>,
    /// Optional Engram memory entry count.
    pub engram_entry_count: Option<usize>,
    /// Optional list of (role, content_preview) for recent turns.
    pub recent_turns: Vec<(String, String)>,
    /// Optional system health summary text.
    pub health_summary: Option<String>,
}

impl Default for DashboardParams {
    fn default() -> Self {
        Self {
            recent_turns_count: 5,
            uptime: None,
            engram_entry_count: None,
            recent_turns: Vec::new(),
            health_summary: None,
        }
    }
}

// ── HTML helpers ───────────────────────────────────────────────────────────────

/// Escape HTML special characters to prevent XSS.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

// ── Render function ────────────────────────────────────────────────────────────

/// Render the Soma dashboard as a complete HTML document.
///
/// Uses `constellation.css` for all styling — no inline styles, no hex colors.
/// All dynamic content is HTML-escaped before insertion.
pub fn render_dashboard(params: &DashboardParams) -> String {
    let uptime_text = params.uptime.as_deref().unwrap_or("unknown");
    let health_text = params
        .health_summary
        .as_deref()
        .unwrap_or("No health data available");

    let engram_text = match params.engram_entry_count {
        Some(n) => format!("{}", n),
        None => "N/A".to_string(),
    };

    // Build recent turns rows — fall back to placeholder if empty.
    let turns_html = if params.recent_turns.is_empty() {
        "<tr><td colspan=\"2\" class=\"text-muted\">No recent turns</td></tr>".to_string()
    } else {
        params
            .recent_turns
            .iter()
            .take(params.recent_turns_count)
            .map(|(role, preview)| {
                let badge_class = match role.as_str() {
                    "user" => "badge-info",
                    "assistant" => "badge-success",
                    _ => "badge-secondary",
                };
                format!(
                    "<tr><td><span class=\"badge {badge}\">{role}</span></td>\
                     <td>{preview}</td></tr>",
                    badge = html_escape(badge_class),
                    role = html_escape(role),
                    preview = html_escape(preview),
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Lumina — Soma Dashboard</title>
{css}
</head>
<body>
<div class="page">
<header class="page-header">
  <h1>Soma Dashboard</h1>
  <p class="text-secondary">Lumina v{version} — uptime: {uptime}</p>
</header>

<div class="grid grid-cols-3">

  <div class="card">
    <div class="card-header">
      <h2 class="card-title">Agent Status</h2>
    </div>
    <div class="card-body">
      <dl>
        <dt>Version</dt>
        <dd><span class="badge badge-info">{version}</span></dd>
        <dt>Uptime</dt>
        <dd>{uptime}</dd>
      </dl>
    </div>
  </div>

  <div class="card">
    <div class="card-header">
      <h2 class="card-title">Memory (Engram)</h2>
    </div>
    <div class="card-body">
      <dl>
        <dt>Entry count</dt>
        <dd>{engram}</dd>
      </dl>
    </div>
  </div>

  <div class="card">
    <div class="card-header">
      <h2 class="card-title">System Health</h2>
    </div>
    <div class="card-body">
      <p>{health}</p>
    </div>
  </div>

</div>

<div class="card">
  <div class="card-header">
    <h2 class="card-title">Recent Turns</h2>
  </div>
  <div class="card-body">
    <table class="table">
      <thead>
        <tr>
          <th>Role</th>
          <th>Preview</th>
        </tr>
      </thead>
      <tbody>
        {turns}
      </tbody>
    </table>
  </div>
</div>

<footer class="lumina-footer">
Lumina Constellation &middot; Soma Dashboard &middot; v{version}
</footer>
</div>
</body>
</html>"#,
        css = CONSTELLATION_CSS,
        version = html_escape(LUMINA_VERSION),
        uptime = html_escape(uptime_text),
        engram = html_escape(&engram_text),
        health = html_escape(health_text),
        turns = turns_html,
    )
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn default_render() -> String {
        render_dashboard(&DashboardParams::default())
    }

    // P2-10 required test: CSS link present
    #[test]
    fn test_dashboard_has_constellation_css() {
        let html = default_render();
        assert!(
            html.contains(r#"<link rel="stylesheet" href="/shared/constellation.css">"#),
            "Dashboard must include the constellation.css link"
        );
    }

    // P2-10 required test: no inline styles
    #[test]
    fn test_dashboard_no_inline_styles() {
        let html = default_render();
        assert!(
            !html.contains("style=\""),
            "Dashboard must not contain inline style= attributes"
        );
    }

    // P2-10 required test: no hardcoded hex colors in style context
    #[test]
    fn test_dashboard_no_hardcoded_colors() {
        let html = default_render();
        // Detect #RRGGBB / #RGB patterns that indicate hardcoded hex colors.
        // Exclude HTML entities (&#x..;) which begin with &# before the hex digits.
        // Also exclude CSS class names that start with # (unlikely in our generated HTML,
        // but the pattern anchors to word boundaries to avoid false positives).
        let re = regex::Regex::new(r"(?:^|[^&])#[0-9a-fA-F]{3,6}\b").unwrap();
        let matches: Vec<_> = re
            .find_iter(&html)
            .filter(|m| {
                // Double-check: exclude any match whose full context shows &#
                let start = m.start();
                !html[..start.saturating_add(2)].ends_with("&#")
            })
            .collect();
        assert!(
            matches.is_empty(),
            "Dashboard must not contain hardcoded hex color codes, found: {:?}",
            matches.iter().map(|m| m.as_str()).collect::<Vec<_>>()
        );
    }

    // Additional correctness tests

    #[test]
    fn test_dashboard_contains_version() {
        let html = default_render();
        assert!(
            html.contains(LUMINA_VERSION),
            "Dashboard should show the crate version"
        );
    }

    #[test]
    fn test_dashboard_shows_unknown_uptime_when_not_set() {
        let html = default_render();
        assert!(html.contains("unknown"), "Uptime should default to 'unknown'");
    }

    #[test]
    fn test_dashboard_shows_custom_uptime() {
        let params = DashboardParams {
            uptime: Some("2 days, 3h".to_string()),
            ..Default::default()
        };
        let html = render_dashboard(&params);
        assert!(html.contains("2 days, 3h"));
    }

    #[test]
    fn test_dashboard_shows_na_when_engram_not_available() {
        let html = default_render();
        assert!(html.contains("N/A"), "Should show N/A for missing Engram count");
    }

    #[test]
    fn test_dashboard_shows_engram_entry_count() {
        let params = DashboardParams {
            engram_entry_count: Some(42),
            ..Default::default()
        };
        let html = render_dashboard(&params);
        assert!(html.contains("42"), "Should display the Engram entry count");
    }

    #[test]
    fn test_dashboard_shows_health_summary() {
        let params = DashboardParams {
            health_summary: Some("All systems nominal".to_string()),
            ..Default::default()
        };
        let html = render_dashboard(&params);
        assert!(html.contains("All systems nominal"));
    }

    #[test]
    fn test_dashboard_no_turns_shows_placeholder() {
        let html = default_render();
        assert!(
            html.contains("No recent turns"),
            "Empty turns list should show placeholder"
        );
    }

    #[test]
    fn test_dashboard_renders_recent_turns() {
        let params = DashboardParams {
            recent_turns: vec![
                ("user".to_string(), "Hello".to_string()),
                ("assistant".to_string(), "Hi there".to_string()),
            ],
            ..Default::default()
        };
        let html = render_dashboard(&params);
        assert!(html.contains("Hello"));
        assert!(html.contains("Hi there"));
        assert!(html.contains("badge-info"));
        assert!(html.contains("badge-success"));
    }

    #[test]
    fn test_dashboard_respects_recent_turns_count() {
        let params = DashboardParams {
            recent_turns_count: 1,
            recent_turns: vec![
                ("user".to_string(), "First".to_string()),
                ("user".to_string(), "Second".to_string()),
            ],
            ..Default::default()
        };
        let html = render_dashboard(&params);
        // With count=1 only the first turn should appear.
        assert!(html.contains("First"));
        assert!(!html.contains("Second"));
    }

    #[test]
    fn test_dashboard_escapes_xss_in_turns() {
        let params = DashboardParams {
            recent_turns: vec![("user".to_string(), "<script>alert('xss')</script>".to_string())],
            ..Default::default()
        };
        let html = render_dashboard(&params);
        assert!(
            !html.contains("<script>"),
            "XSS content must be escaped"
        );
        assert!(html.contains("&lt;script&gt;"), "Must be HTML-escaped");
    }

    #[test]
    fn test_dashboard_has_page_structure() {
        let html = default_render();
        assert!(html.contains("<html"), "Must be a full HTML document");
        assert!(html.contains("<head>"), "Must have a <head> section");
        assert!(html.contains("<body>"), "Must have a <body> section");
        assert!(html.contains("</html>"), "Must close the HTML tag");
    }

    #[test]
    fn test_dashboard_has_footer() {
        let html = default_render();
        assert!(
            html.contains("lumina-footer"),
            "Dashboard must have the lumina-footer class"
        );
    }

    #[test]
    fn test_html_escape_special_chars() {
        assert_eq!(html_escape("<"), "&lt;");
        assert_eq!(html_escape(">"), "&gt;");
        assert_eq!(html_escape("&"), "&amp;");
        assert_eq!(html_escape("\""), "&quot;");
        assert_eq!(html_escape("'"), "&#x27;");
        assert_eq!(html_escape("safe"), "safe");
    }

    #[test]
    fn test_dashboard_params_default() {
        let p = DashboardParams::default();
        assert_eq!(p.recent_turns_count, 5);
        assert!(p.uptime.is_none());
        assert!(p.engram_entry_count.is_none());
        assert!(p.recent_turns.is_empty());
        assert!(p.health_summary.is_none());
    }
}
