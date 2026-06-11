//! P2-15: Soma PWA support — manifest.json, service worker, mobile page.
//!
//! Provides:
//! - [`manifest_json`] — PWA manifest content (JSON string)
//! - [`service_worker_js`] — minimal service worker JavaScript
//! - [`render_mobile_page`] — mobile-optimised HTML dashboard page
//!
//! ## Design rules (MANDATORY — lumina-design-system-spec)
//! - Every HTML page MUST include:
//!   `<link rel="stylesheet" href="/shared/constellation.css">`
//! - NO inline `style=""` attributes
//! - NO hardcoded hex colors
//! - NO custom `<style>` blocks duplicating constellation.css
//! - Use `.card`, `.badge-*`, `.table`, `.btn-*`, `var(--bg-primary)`, etc.

use std::env;
use std::sync::OnceLock;

/// Mandatory constellation.css link — must appear in every HTML page.
const CONSTELLATION_CSS: &str =
    r#"<link rel="stylesheet" href="/shared/constellation.css">"#;

/// Read the app name from the `LUMINA_APP_NAME` environment variable.
/// Falls back to `"Lumina"` — never hardcodes an org name.
/// Cached in a `OnceLock` to avoid a syscall on every request.
fn app_name() -> &'static str {
    static NAME: OnceLock<String> = OnceLock::new();
    NAME.get_or_init(|| {
        env::var("LUMINA_APP_NAME").unwrap_or_else(|_| "Lumina".to_string())
    })
}

// ── Manifest ──────────────────────────────────────────────────────────────────

/// Return the PWA `manifest.json` content as a JSON string.
///
/// The `name` / `short_name` come from the `LUMINA_APP_NAME` environment
/// variable (default: `"Lumina"`).  No org name is hardcoded.
/// `start_url` is `/mobile` so the installed PWA opens the mobile dashboard.
pub fn manifest_json() -> String {
    let name = app_name();
    // Use serde_json to build the manifest so we don't have to wrestle with
    // raw-string delimiters and `#` color values inside format! macros.
    let v = serde_json::json!({
        "name": name,
        "short_name": name,
        "display": "standalone",
        "start_url": "/mobile",
        "theme_color": "#000",
        "background_color": "#000",
        "icons": [{"src": "/icon-192.png", "sizes": "192x192", "type": "image/png"}]
    });
    v.to_string()
}

// ── Service worker ────────────────────────────────────────────────────────────

/// Return the minimal service-worker JavaScript.
///
/// Strategy:
/// - API routes (`/v1/`) — always network-only; never cached (POST responses
///   must never be served stale).
/// - Shell pages (`/`, `/mobile`) and static assets (`/sw.js`, `/manifest.json`)
///   — cache-first with network fallback, pre-cached on install.
/// - An `activate` handler purges stale versioned caches on deploy.
pub fn service_worker_js() -> &'static str {
    // Pre-cache only routes that actually exist and are publicly accessible.
    // "/manifest.json" and "/sw.js" are public (no auth).
    // "/mobile" is behind auth but the browser has a session cookie when the
    // SW registers, so the fetch succeeds.
    // "/" is intentionally omitted — no root route exists, addAll would fail.
    r#"const CACHE="lumina-v1";
const SHELL=["/mobile","/manifest.json","/sw.js"];
self.addEventListener("install",e=>e.waitUntil(caches.open(CACHE).then(c=>c.addAll(SHELL))));
self.addEventListener("activate",e=>e.waitUntil(caches.keys().then(ks=>Promise.all(ks.filter(k=>k!==CACHE).map(k=>caches.delete(k))))));
self.addEventListener("fetch",e=>{if(e.request.url.includes("/v1/")){return;}e.respondWith(caches.match(e.request).then(r=>r||fetch(e.request)));});
"#
}

// ── Mobile page ───────────────────────────────────────────────────────────────

/// Render the Soma mobile dashboard page as a complete HTML document.
///
/// Includes:
/// - `constellation.css` for all styling (no inline styles, no hex colors)
/// - `<meta name="viewport" …>` for responsive layout
/// - `<link rel="manifest" href="/manifest.json">` for PWA install
/// - Inline service-worker registration script
/// - Compact `.card` layout suitable for narrow screens
///
/// All dynamic text is HTML-escaped; no infrastructure addresses are hardcoded.
pub fn render_mobile_page() -> String {
    let name = app_name();
    let name_escaped = html_escape(name);

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>{name} — Mobile</title>
{css}
<link rel="manifest" href="/manifest.json">
</head>
<body>
<div class="page">
<header class="page-header">
  <h1>{name}</h1>
  <p class="text-secondary">Mobile Dashboard</p>
</header>

<div class="grid">

  <div class="card">
    <div class="card-header">
      <h2 class="card-title">Status</h2>
    </div>
    <div class="card-body">
      <span class="badge badge-success">Online</span>
    </div>
  </div>

  <div class="card">
    <div class="card-header">
      <h2 class="card-title">Quick Links</h2>
    </div>
    <div class="card-body">
      <a class="btn-primary" href="/dashboard">Dashboard</a>
      <a class="btn-secondary" href="/chat">Chat</a>
    </div>
  </div>

</div>

<footer class="lumina-footer">
{name} &middot; Mobile
</footer>
</div>
<script>
if ('serviceWorker' in navigator) {{
  navigator.serviceWorker.register('/sw.js').catch(function(e) {{
    console.warn('SW registration failed:', e);
  }});
}}
</script>
</body>
</html>"#,
        css = CONSTELLATION_CSS,
        name = name_escaped,
    )
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Escape HTML special characters to prevent XSS.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    // P2-15 required test: manifest.json is valid JSON with required PWA fields
    #[test]
    fn test_manifest_valid_json() {
        let json_str = manifest_json();
        let parsed: serde_json::Value =
            serde_json::from_str(&json_str).expect("manifest.json must be valid JSON");
        assert!(parsed.get("name").is_some(), "manifest must have 'name'");
        assert!(parsed.get("display").is_some(), "manifest must have 'display'");
        assert!(parsed.get("start_url").is_some(), "manifest must have 'start_url'");
        assert!(parsed.get("icons").is_some(), "manifest must have 'icons'");
    }

    // P2-15 required test: manifest name must not be a hardcoded org name
    #[test]
    fn test_manifest_no_hardcoded_org_name() {
        // LUMINA_APP_NAME is unset in the test environment, so the default
        // "Lumina" is used.  The requirement is that it must NOT be "moosenet"
        // or any other org-level identifier.
        let json_str = manifest_json();
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        let name = parsed["name"].as_str().unwrap_or("");
        assert_ne!(
            name.to_lowercase(),
            "moosenet",
            "manifest name must not be a hardcoded org name"
        );
        // Name must be non-empty (either env override or the default "Lumina")
        assert!(!name.is_empty(), "manifest name must not be empty");
    }

    // P2-15 required test: render_mobile_page() must include constellation.css
    #[test]
    fn test_mobile_has_constellation_css() {
        let html = render_mobile_page();
        assert!(
            html.contains(r#"<link rel="stylesheet" href="/shared/constellation.css">"#),
            "Mobile page must include constellation.css link"
        );
    }

    // P2-15 required test: render_mobile_page() must not contain inline styles
    #[test]
    fn test_mobile_no_inline_styles() {
        let html = render_mobile_page();
        assert!(
            !html.contains("style=\""),
            "Mobile page must not contain inline style= attributes"
        );
    }

    // Additional correctness tests

    #[test]
    fn test_mobile_has_manifest_link() {
        let html = render_mobile_page();
        assert!(
            html.contains(r#"<link rel="manifest" href="/manifest.json">"#),
            "Mobile page must link to manifest.json"
        );
    }

    #[test]
    fn test_mobile_has_viewport_meta() {
        let html = render_mobile_page();
        assert!(
            html.contains(r#"<meta name="viewport""#),
            "Mobile page must have viewport meta tag"
        );
    }

    #[test]
    fn test_mobile_has_service_worker_registration() {
        let html = render_mobile_page();
        assert!(
            html.contains("serviceWorker"),
            "Mobile page must register the service worker"
        );
        assert!(html.contains("/sw.js"), "Must reference /sw.js");
    }

    #[test]
    fn test_mobile_has_constellation_classes() {
        let html = render_mobile_page();
        assert!(html.contains("class=\"card\"") || html.contains("class=\"card "),
            "Mobile page should use .card component");
    }

    #[test]
    fn test_service_worker_contains_cache_name() {
        let sw = service_worker_js();
        assert!(sw.contains("lumina-v1"), "Service worker must define a versioned cache");
    }

    #[test]
    fn test_service_worker_caches_mobile_shell() {
        // "/" is not routed in Axum so it is intentionally omitted from the
        // pre-cache list; /mobile is the PWA entry point instead.
        let sw = service_worker_js();
        assert!(sw.contains("\"/mobile\""), "Service worker must pre-cache /mobile");
        assert!(sw.contains("\"/manifest.json\""), "Service worker must pre-cache /manifest.json");
    }

    #[test]
    fn test_service_worker_handles_fetch() {
        let sw = service_worker_js();
        assert!(sw.contains("fetch"), "Service worker must handle fetch events");
    }

    #[test]
    fn test_service_worker_caches_mobile() {
        let sw = service_worker_js();
        assert!(sw.contains("\"/mobile\""), "Service worker must cache /mobile");
    }

    #[test]
    fn test_service_worker_has_activate_handler() {
        let sw = service_worker_js();
        assert!(sw.contains("activate"), "Service worker must have an activate handler to purge stale caches");
    }

    #[test]
    fn test_service_worker_excludes_api_routes() {
        let sw = service_worker_js();
        // The SW must skip caching for /v1/ API routes so LLM responses are
        // never served stale.
        assert!(sw.contains("/v1/"), "Service worker must exclude /v1/ API routes from caching");
    }

    #[test]
    fn test_manifest_start_url_is_mobile() {
        let json_str = manifest_json();
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(
            parsed["start_url"].as_str().unwrap_or(""),
            "/mobile",
            "manifest start_url should point to /mobile for the installed PWA"
        );
    }

    #[test]
    #[serial]
    fn test_manifest_default_name_is_lumina() {
        // When LUMINA_APP_NAME is not set, default must be "Lumina"
        // (We can only test this deterministically if the env var is unset.)
        if std::env::var("LUMINA_APP_NAME").is_err() {
            let json_str = manifest_json();
            let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
            assert_eq!(parsed["name"].as_str().unwrap_or(""), "Lumina");
        }
    }

    #[test]
    fn test_mobile_page_is_valid_html_structure() {
        let html = render_mobile_page();
        assert!(html.contains("<!DOCTYPE html>"), "Must be a complete HTML document");
        assert!(html.contains("<html"), "Must have <html>");
        assert!(html.contains("<head>"), "Must have <head>");
        assert!(html.contains("<body>"), "Must have <body>");
        assert!(html.contains("</html>"), "Must close <html>");
    }

    #[test]
    fn test_mobile_has_footer() {
        let html = render_mobile_page();
        assert!(html.contains("lumina-footer"), "Mobile page must have lumina-footer");
    }
}
