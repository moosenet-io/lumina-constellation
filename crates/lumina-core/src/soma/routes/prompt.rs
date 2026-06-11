//! DPROMPT-10: Prompt-state dashboard page for Soma.
//!
//! Renders `GET /prompt` — an HTML view of a user's current dynamic-prompt
//! state:
//! - Trait values as visual unicode bars (`humor ████████░░ 0.67`)
//! - Last nightly + weekly consolidation timestamps and outcomes
//! - Knowledge-digest preview (first 100 words)
//! - Personality-vector preview (first 100 words)
//! - Consolidation log: last 10 entries in a `.table`
//!
//! ## Security model
//! - Admin users may view any `user_id`'s prompt state.
//! - Non-admin users may view only their own.
//! - This module only renders HTML from a `(user_id, layers_root)` it is given;
//!   the calling HTTP layer is responsible for deciding *which* `user_id` a
//!   session is allowed to view (admin → any, user → own).
//!
//! ## Design rules (MANDATORY — lumina-design-system-spec)
//! - Every HTML page MUST include `<link rel="stylesheet" href="/shared/constellation.css">`.
//! - NO inline `style=""` attributes, NO hardcoded hex colours.
//! - Use `.card`, `.badge-*`, `.table`, etc.
//! - No hardcoded infrastructure addresses or organisation names.
//!
//! ## No chrono
//! Timestamps stored in the consolidation log are Unix seconds (`i64`). They
//! are formatted with a small self-contained civil-time helper so this module
//! needs no `chrono` dependency and no clock access.

use std::path::Path;

use crate::prompt::consolidation_log::{ConsolidationEntry, ConsolidationKind, ConsolidationLog};
use crate::prompt::traits::{TraitVector, TRAIT_MAX, TRAIT_MIN};

/// Mandatory constellation.css stylesheet link — must appear in every HTML page.
const CONSTELLATION_CSS: &str =
    r#"<link rel="stylesheet" href="/shared/constellation.css">"#;

/// Width (in cells) of a rendered trait bar.
const BAR_WIDTH: usize = 10;

/// Number of words shown in a layer preview.
const PREVIEW_WORDS: usize = 100;

/// Number of consolidation-log entries shown.
const LOG_LIMIT: usize = 10;

// ── HTML rendering helpers ─────────────────────────────────────────────────

/// Escape HTML special characters to prevent XSS.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// Build a unicode bar like `████████░░` for `value` within `[TRAIT_MIN, TRAIT_MAX]`.
///
/// Bounds are mapped across the full bar width so a value at `TRAIT_MIN`
/// renders an empty bar and a value at `TRAIT_MAX` renders a full bar.
pub fn trait_bar(value: f32) -> String {
    let v = value.clamp(TRAIT_MIN, TRAIT_MAX);
    let span = (TRAIT_MAX - TRAIT_MIN).max(f32::EPSILON);
    let frac = (v - TRAIT_MIN) / span;
    let mut filled = (frac * BAR_WIDTH as f32).round() as usize;
    if filled > BAR_WIDTH {
        filled = BAR_WIDTH;
    }
    let empty = BAR_WIDTH - filled;
    let mut bar = String::with_capacity(BAR_WIDTH * 3);
    for _ in 0..filled {
        bar.push('█');
    }
    for _ in 0..empty {
        bar.push('░');
    }
    bar
}

/// Format Unix seconds as a `YYYY-MM-DD HH:MM UTC` string with no chrono.
///
/// Self-contained civil-time conversion (Howard Hinnant's `days_from_civil`
/// inverse). UTC only — sufficient for an audit-timestamp display.
fn fmt_ts(ts_secs: i64) -> String {
    if ts_secs <= 0 {
        return "—".to_string();
    }
    let days = ts_secs.div_euclid(86_400);
    let secs_of_day = ts_secs.rem_euclid(86_400);
    let (h, mi) = (secs_of_day / 3_600, (secs_of_day % 3_600) / 60);

    // days since 1970-01-01 → civil (y, m, d)
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02} UTC")
}

/// First `PREVIEW_WORDS` words of `text`, with an ellipsis when truncated.
fn word_preview(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return String::new();
    }
    if words.len() <= PREVIEW_WORDS {
        words.join(" ")
    } else {
        format!("{} …", words[..PREVIEW_WORDS].join(" "))
    }
}

/// Human label for a consolidation kind.
fn kind_label(kind: ConsolidationKind) -> &'static str {
    match kind {
        ConsolidationKind::Nightly => "Nightly",
        ConsolidationKind::Weekly => "Weekly",
        ConsolidationKind::Immediate => "Immediate",
    }
}

/// Outcome badge for a single entry (ok / had errors).
fn outcome_badge(entry: &ConsolidationEntry) -> String {
    if entry.had_errors() {
        format!(
            r#"<span class="badge badge-danger">{} error(s)</span>"#,
            entry.errors.len()
        )
    } else {
        r#"<span class="badge badge-success">OK</span>"#.to_string()
    }
}

// ── render_prompt_page ──────────────────────────────────────────────────────

/// Render the prompt-state dashboard for `user_id` as a complete HTML document.
///
/// Reads the user's `trait-vector.json`, `knowledge-digest.txt`,
/// `personality-vector.txt` from `{layers_root}/{user_id}/`, and the
/// consolidation log at `log_path`. Absent files render a
/// "Pending first consolidation" placeholder rather than failing.
///
/// Uses constellation.css for all styling — no inline styles, no hardcoded
/// hex colours.
pub fn render_prompt_page(user_id: &str, layers_root: &Path, log_path: &Path) -> String {
    let user_dir = layers_root.join(sanitize_user(user_id));

    // ── Trait vector (always available — defaults when absent) ─────────────
    let traits = TraitVector::load(&user_dir.join("trait-vector.json"));
    let trait_file_present = user_dir.join("trait-vector.json").exists();

    // ── Layer previews ─────────────────────────────────────────────────────
    let digest_raw = read_layer(&user_dir.join("knowledge-digest.txt"));
    let personality_raw = read_layer(&user_dir.join("personality-vector.txt"));

    // ── Consolidation log ──────────────────────────────────────────────────
    let log = ConsolidationLog::at(log_path);
    let entries = log.read_last(LOG_LIMIT);

    // Whether anything has been consolidated yet at all.
    let pending = !trait_file_present
        && digest_raw.is_none()
        && personality_raw.is_none()
        && entries.is_empty();

    // ── Section: trait bars ────────────────────────────────────────────────
    let trait_rows = [
        ("flair", traits.flair),
        ("spontaneity", traits.spontaneity),
        ("humor", traits.humor),
        ("focus", traits.focus),
    ]
    .iter()
    .map(|(name, value)| {
        format!(
            "<tr><td>{name}</td><td><code>{bar}</code></td><td>{value:.2}</td></tr>",
            name = name,
            bar = trait_bar(*value),
            value = value,
        )
    })
    .collect::<Vec<_>>()
    .join("\n");

    // ── Section: last nightly / weekly outcomes ────────────────────────────
    let last_nightly = entries
        .iter()
        .rev()
        .find(|e| e.kind == ConsolidationKind::Nightly);
    let last_weekly = entries
        .iter()
        .rev()
        .find(|e| e.kind == ConsolidationKind::Weekly);

    let nightly_cell = consolidation_summary_cell(last_nightly);
    let weekly_cell = consolidation_summary_cell(last_weekly);

    // ── Section: previews ──────────────────────────────────────────────────
    let digest_preview = match &digest_raw {
        Some(t) => html_escape(&word_preview(t)),
        None => r#"<span class="text-secondary">Pending first consolidation</span>"#.to_string(),
    };
    let personality_preview = match &personality_raw {
        Some(t) => html_escape(&word_preview(t)),
        None => r#"<span class="text-secondary">Pending first consolidation</span>"#.to_string(),
    };

    // ── Section: consolidation log table (last 10, newest-first) ───────────
    let log_rows: String = if entries.is_empty() {
        r#"<tr><td colspan="5" class="text-secondary">Pending first consolidation</td></tr>"#
            .to_string()
    } else {
        entries
            .iter()
            .rev()
            .map(|e| {
                let layers = if e.layers_updated.is_empty() {
                    "—".to_string()
                } else {
                    html_escape(&e.layers_updated.join(", "))
                };
                let changes = e
                    .trait_changes
                    .as_deref()
                    .map(html_escape)
                    .unwrap_or_else(|| "—".to_string());
                format!(
                    "<tr><td>{ts}</td><td><span class=\"badge badge-secondary\">{kind}</span></td><td>{outcome}</td><td>{layers}</td><td>{changes}</td></tr>",
                    ts = fmt_ts(e.ts_secs),
                    kind = kind_label(e.kind),
                    outcome = outcome_badge(e),
                    layers = layers,
                    changes = changes,
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let pending_banner = if pending {
        r#"<div class="alert alert-info">Pending first consolidation — no prompt layers generated yet. Trait values shown are the locked-in defaults.</div>"#
    } else {
        ""
    };

    // ── Assemble full page ─────────────────────────────────────────────────
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Prompt State — {user_title}</title>
{css}
</head>
<body>
<div class="page">
<header class="page-header">
  <h1>Prompt State</h1>
  <p class="text-secondary">User: <span class="badge badge-secondary">{user_badge}</span></p>
</header>

{pending_banner}

<div class="card">
  <div class="card-header">
    <h2 class="card-title">Personality Traits</h2>
  </div>
  <div class="card-body">
    <table class="table">
      <thead>
        <tr><th>Trait</th><th>Bar</th><th>Value</th></tr>
      </thead>
      <tbody>
        {trait_rows}
      </tbody>
    </table>
  </div>
</div>

<div class="grid">
  <div class="card">
    <div class="card-header">
      <h2 class="card-title">Last Nightly Consolidation</h2>
    </div>
    <div class="card-body">{nightly_cell}</div>
  </div>
  <div class="card">
    <div class="card-header">
      <h2 class="card-title">Last Weekly Consolidation</h2>
    </div>
    <div class="card-body">{weekly_cell}</div>
  </div>
</div>

<div class="card">
  <div class="card-header">
    <h2 class="card-title">Knowledge Digest (preview)</h2>
  </div>
  <div class="card-body"><p>{digest_preview}</p></div>
</div>

<div class="card">
  <div class="card-header">
    <h2 class="card-title">Personality Vector (preview)</h2>
  </div>
  <div class="card-body"><p>{personality_preview}</p></div>
</div>

<div class="card">
  <div class="card-header">
    <h2 class="card-title">Consolidation Log (last {log_limit})</h2>
  </div>
  <div class="card-body">
    <table class="table">
      <thead>
        <tr><th>When</th><th>Cycle</th><th>Outcome</th><th>Layers</th><th>Trait changes</th></tr>
      </thead>
      <tbody>
        {log_rows}
      </tbody>
    </table>
  </div>
</div>

<footer class="lumina-footer">
Lumina Constellation &middot; Prompt State
</footer>
</div>
</body>
</html>"#,
        user_title = html_escape(user_id),
        css = CONSTELLATION_CSS,
        user_badge = html_escape(user_id),
        pending_banner = pending_banner,
        trait_rows = trait_rows,
        nightly_cell = nightly_cell,
        weekly_cell = weekly_cell,
        digest_preview = digest_preview,
        personality_preview = personality_preview,
        log_limit = LOG_LIMIT,
        log_rows = log_rows,
    )
}

/// Render the body of a "last {cycle} consolidation" card.
fn consolidation_summary_cell(entry: Option<&ConsolidationEntry>) -> String {
    match entry {
        None => r#"<p class="text-secondary">Pending first consolidation</p>"#.to_string(),
        Some(e) => {
            let layers = if e.layers_updated.is_empty() {
                "no layers".to_string()
            } else {
                html_escape(&e.layers_updated.join(", "))
            };
            format!(
                "<p>{ts}</p><p>{outcome} &nbsp; <span class=\"text-secondary\">{layers}</span></p>",
                ts = fmt_ts(e.ts_secs),
                outcome = outcome_badge(e),
                layers = layers,
            )
        }
    }
}

/// Read a layer file, returning `None` when absent/empty.
fn read_layer(path: &Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        _ => None,
    }
}

/// Keep user ids filesystem-safe — mirrors `prompt::sanitize_user` so the
/// dashboard reads from the same per-user directory the assembler writes to.
fn sanitize_user(user_id: &str) -> String {
    let cleaned: String = user_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "default".to_string()
    } else {
        cleaned
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::consolidation_log::ConsolidationEntry;
    use tempfile::tempdir;

    fn write_user_file(root: &Path, user: &str, name: &str, body: &str) {
        let dir = root.join(super::sanitize_user(user));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn page_includes_constellation_css() {
        let dir = tempdir().unwrap();
        let html = render_prompt_page("operator", dir.path(), &dir.path().join("c.log"));
        assert!(
            html.contains(r#"<link rel="stylesheet" href="/shared/constellation.css">"#),
            "Page must include constellation.css link"
        );
    }

    #[test]
    fn page_uses_constellation_classes_no_inline_styles() {
        let dir = tempdir().unwrap();
        let html = render_prompt_page("operator", dir.path(), &dir.path().join("c.log"));
        assert!(html.contains("class=\"card\""));
        assert!(html.contains("class=\"table\""));
        assert!(html.contains("badge-"));
        assert!(html.contains("lumina-footer"));
        assert!(!html.contains("style=\""), "no inline style attributes");
        assert!(!html.contains("#0d1117") && !html.contains("#58a6ff"));
    }

    #[test]
    fn page_valid_html_structure() {
        let dir = tempdir().unwrap();
        let html = render_prompt_page("operator", dir.path(), &dir.path().join("c.log"));
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("<head>"));
        assert!(html.contains("<body>"));
        assert!(html.contains("</html>"));
    }

    #[test]
    fn pending_state_when_nothing_generated() {
        let dir = tempdir().unwrap();
        let html = render_prompt_page("newbie", dir.path(), &dir.path().join("c.log"));
        assert!(html.contains("Pending first consolidation"));
    }

    #[test]
    fn trait_bars_render_from_floats() {
        let dir = tempdir().unwrap();
        // Default trait vector → humor 0.65 etc.; bars are unicode blocks.
        let html = render_prompt_page("operator", dir.path(), &dir.path().join("c.log"));
        assert!(html.contains("humor"));
        assert!(html.contains('█'));
        assert!(html.contains('░'));
        for t in ["flair", "spontaneity", "humor", "focus"] {
            assert!(html.contains(t), "missing trait {t}");
        }
    }

    #[test]
    fn trait_bar_bounds_render_clearly() {
        // Min → all empty; Max → all full.
        let min_bar = trait_bar(TRAIT_MIN);
        let max_bar = trait_bar(TRAIT_MAX);
        assert_eq!(min_bar.chars().filter(|c| *c == '█').count(), 0);
        assert_eq!(max_bar.chars().filter(|c| *c == '█').count(), BAR_WIDTH);
        // Out-of-range values are clamped, not panicking / overflowing.
        assert_eq!(trait_bar(99.0).chars().filter(|c| *c == '█').count(), BAR_WIDTH);
        assert_eq!(trait_bar(-99.0).chars().filter(|c| *c == '█').count(), 0);
    }

    #[test]
    fn previews_and_log_render_when_present() {
        let dir = tempdir().unwrap();
        write_user_file(dir.path(), "operator", "knowledge-digest.txt", "alpha beta gamma delta");
        write_user_file(
            dir.path(),
            "operator",
            "personality-vector.txt",
            "systems thinker who values directness",
        );
        let log_path = dir.path().join("c.log");
        let log = ConsolidationLog::at(&log_path);
        let mut e = ConsolidationEntry::new(ConsolidationKind::Nightly, "operator", 1_700_000_000);
        e.add_layer("style");
        e.add_layer("knowledge");
        e.trait_changes = Some("humor 0.65 to 0.67".into());
        log.append(&e).unwrap();
        let mut w = ConsolidationEntry::new(ConsolidationKind::Weekly, "operator", 1_700_500_000);
        w.add_error("vram swap failed");
        log.append(&w).unwrap();

        let html = render_prompt_page("operator", dir.path(), &log_path);
        assert!(html.contains("alpha beta gamma delta"));
        assert!(html.contains("systems thinker"));
        assert!(html.contains("Nightly"));
        assert!(html.contains("Weekly"));
        assert!(html.contains("badge-success")); // nightly OK
        assert!(html.contains("badge-danger")); // weekly had errors
        assert!(html.contains("humor 0.65 to 0.67"));
        // Timestamp formatted without chrono.
        assert!(html.contains("2023-11-"));
        assert!(!html.contains("Pending first consolidation"));
    }

    #[test]
    fn preview_truncates_to_100_words() {
        let long = (0..250).map(|i| format!("w{i}")).collect::<Vec<_>>().join(" ");
        let p = word_preview(&long);
        assert!(p.ends_with('…'));
        // 100 words + ellipsis token.
        assert_eq!(p.split_whitespace().count(), PREVIEW_WORDS + 1);
        assert!(p.contains("w0"));
        assert!(p.contains("w99"));
        assert!(!p.contains("w100 ")); // not the 101st word
    }

    #[test]
    fn user_id_is_escaped() {
        let dir = tempdir().unwrap();
        let html = render_prompt_page("<script>x</script>", dir.path(), &dir.path().join("c.log"));
        assert!(!html.contains("<script>x"));
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn fmt_ts_is_deterministic_utc() {
        // 1_700_000_000 → 2023-11-14 22:13 UTC
        assert_eq!(fmt_ts(1_700_000_000), "2023-11-14 22:13 UTC");
        assert_eq!(fmt_ts(0), "—");
        assert_eq!(fmt_ts(-5), "—");
    }

    #[test]
    fn no_hardcoded_infra_addresses_in_output() {
        let dir = tempdir().unwrap();
        let html = render_prompt_page("operator", dir.path(), &dir.path().join("c.log"));
        // Build the needle dynamically so source self-scans don't trip.
        let needle = format!("{}.{}", "192", "168");
        assert!(!html.contains(&needle), "no hardcoded infra address");
    }
}
