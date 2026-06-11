//! HTML sanitization for the Lumina web client.
//!
//! Strips dangerous or tracking elements from fetched HTML before content
//! extraction. The sanitizer is intentionally conservative: when in doubt,
//! remove the element.
//!
//! # Removed elements (entirely, including content)
//! - `<script>`, `<style>`, `<iframe>`, `<noscript>` — executable / presentational
//! - Tracking pixels: `<img>` with `width="1"` / `height="1"` (or `style="width:1px"`)
//! - `<nav>`, `<footer>`, `<aside>` — chrome that typically contains no article content
//!
//! # Preserved
//! - `<p>`, `<h1>`–`<h6>`, `<li>`, `<blockquote>` — converted to plain text with newlines
//! - `<a href="…">text</a>` — converted to `[text](url)` markdown
//! - All other tags — stripped (attributes removed, inner text kept)

use std::sync::LazyLock;

use regex::Regex;

// ─────────────────────────────────────────────────────────────────────────────
// Pre-compiled regex statics
//
// Compiling a Regex is non-trivial (it runs the NFA/DFA builder).  Hoisting
// these into LazyLock statics means each pattern is compiled exactly once per
// process, regardless of how many pages are fetched.
// ─────────────────────────────────────────────────────────────────────────────

/// Matches self-closing or normally closed `<img>` tags.
static IMG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<img(?:\s[^>]*)?>").unwrap());

/// Matches `<a href="…">…</a>` links (captures href and inner content).
static LINK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)<a(?:\s+[^>]*?\s+|\s+)href=["']([^"']+)["'][^>]*>(.*?)</a\s*>"#).unwrap()
});

/// Matches any HTML tag (used to strip inner-tag markup from link text).
static TAG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").unwrap());

/// Matches opening block-level tags that should become a newline.
static OPEN_BLOCK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)<(p|h[1-6]|li|blockquote|div|tr|article|section|main)[^>]*>").unwrap()
});

/// Matches closing block-level tags that should become a newline.
static CLOSE_BLOCK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)</(p|h[1-6]|li|blockquote|div|tr|article|section|main)\s*>").unwrap()
});

/// Matches `<br>` and `<hr>` line-break tags.
static BR_HR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<br\s*/?>|<hr\s*/?>").unwrap());

/// Matches any remaining HTML tag (for final strip).
static ALL_TAGS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<[^>]+>").unwrap());

/// Matches runs of horizontal whitespace (spaces and tabs).
static WS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[ \t]+").unwrap());

/// Matches three or more consecutive newlines.
static MULTI_NL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\n{3,}").unwrap());

// ─────────────────────────────────────────────────────────────────────────────
// Public functions
// ─────────────────────────────────────────────────────────────────────────────

/// Remove entire tags (opening, content, and closing) for dangerous elements.
///
/// The `tags` slice contains tag names (lowercase, no `<>`).
/// This is intentionally simple: it handles nested tags of the *same* type only
/// when the outer instance closes last, which is sufficient for sanitization
/// purposes because nesting `<script>` inside `<script>` is never valid HTML.
pub fn remove_tags_with_content(html: &str, tags: &[&str]) -> String {
    let mut result = html.to_string();
    for tag in tags {
        // Build a pattern that matches the open tag (with any attributes),
        // all content (including newlines, non-greedy), and the close tag.
        let pattern = format!(
            r"(?si)<{tag}(?:\s[^>]*)?>.*?</{tag}\s*>",
            tag = regex::escape(tag)
        );
        if let Ok(re) = Regex::new(&pattern) {
            result = re.replace_all(&result, "").into_owned();
        }
    }
    result
}

/// Remove tracking pixels: `<img>` tags with `width="1"` / `height="1"` attributes
/// or inline style `width:1px` / `height:1px`.
pub fn remove_tracking_pixels(html: &str) -> String {
    IMG_RE
        .replace_all(html, |caps: &regex::Captures| {
            let full = &caps[0];
            let lower = full.to_lowercase();
            let is_1x1 = (lower.contains("width=\"1\"")
                || lower.contains("width='1'")
                || lower.contains("width: 1px")
                || lower.contains("width:1px"))
                || (lower.contains("height=\"1\"")
                    || lower.contains("height='1'")
                    || lower.contains("height: 1px")
                    || lower.contains("height:1px"));
            if is_1x1 {
                String::new()
            } else {
                full.to_string()
            }
        })
        .into_owned()
}

/// Strip block-level chrome: `<nav>`, `<footer>`, `<aside>` with their content.
pub fn remove_chrome_elements(html: &str) -> String {
    remove_tags_with_content(html, &["nav", "footer", "aside", "header"])
}

/// Convert `<a href="url">text</a>` to `[text](url)` markdown links.
pub fn convert_links_to_markdown(html: &str) -> String {
    LINK_RE
        .replace_all(html, |caps: &regex::Captures| {
            let href = caps[1].trim();
            // Strip any inner HTML tags from the link text using the pre-compiled static.
            let raw_text = &caps[2];
            let text = TAG_RE.replace_all(raw_text, "");
            let text = text.trim();
            if text.is_empty() {
                String::new()
            } else if href.is_empty() || href.starts_with("javascript:") {
                text.to_string()
            } else {
                format!("[{}]({})", text, href)
            }
        })
        .into_owned()
}

/// Convert block-level HTML tags to newlines so extracted text is readable.
///
/// `<p>`, `<h1>`–`<h6>`, `<li>`, `<blockquote>`, `<br>`, `<hr>`, `<div>`, `<tr>`
/// each produce a newline before their content (opening tags) or after (closing tags).
pub fn convert_block_tags_to_newlines(html: &str) -> String {
    // Opening block tags → prepend newline
    let s = OPEN_BLOCK_RE.replace_all(html, "\n").into_owned();

    // Closing block tags → append newline
    let s = CLOSE_BLOCK_RE.replace_all(&s, "\n").into_owned();

    // <br> and <hr> → newline
    BR_HR_RE.replace_all(&s, "\n").into_owned()
}

/// Strip all remaining HTML tags (attributes already gone at this point).
pub fn strip_all_tags(html: &str) -> String {
    ALL_TAGS_RE.replace_all(html, "").into_owned()
}

/// Collapse multiple consecutive blank lines / whitespace.
pub fn collapse_whitespace(text: &str) -> String {
    // Collapse runs of spaces/tabs to a single space
    let s = WS_RE.replace_all(text, " ").into_owned();

    // Collapse 3+ consecutive newlines to 2
    let s = MULTI_NL_RE.replace_all(&s, "\n\n").into_owned();

    // Trim leading/trailing whitespace from each line
    s.lines()
        .map(|l| l.trim())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Full sanitization pipeline.
///
/// Applies all sanitization steps in order and returns cleaned HTML ready for
/// text extraction.
pub fn sanitize(html: &str) -> String {
    let s = remove_tags_with_content(
        html,
        &["script", "style", "iframe", "noscript", "object", "embed"],
    );
    let s = remove_tracking_pixels(&s);
    remove_chrome_elements(&s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_html_sanitization_strips_scripts() {
        let html = r#"<html><body><script>alert('xss');</script><p>Hello</p><script type="text/javascript">var x=1;</script></body></html>"#;
        let result = sanitize(html);
        assert!(!result.contains("<script"), "script tags must be removed");
        assert!(!result.contains("alert("), "script content must be removed");
        assert!(!result.contains("var x=1"), "inline script content must be removed");
        assert!(result.contains("<p>Hello</p>"), "body content must be preserved");
    }

    #[test]
    fn test_html_sanitization_strips_tracking_pixels() {
        let html = r#"<p>Text</p><img src="https://track.example.com/pixel.gif" width="1" height="1" alt=""><p>More</p>"#;
        let result = remove_tracking_pixels(html);
        assert!(!result.contains("track.example.com"), "tracking pixel must be removed");
        assert!(result.contains("<p>Text</p>"), "surrounding content preserved");
        assert!(result.contains("<p>More</p>"), "following content preserved");
    }

    #[test]
    fn test_tracking_pixel_width_one_removed() {
        let html = r#"<img src="https://track.com/p" width="1" alt="">"#;
        let result = remove_tracking_pixels(html);
        assert!(result.is_empty() || !result.contains("track.com"));
    }

    #[test]
    fn test_normal_image_preserved() {
        let html = r#"<img src="https://example.com/photo.jpg" width="800" height="600" alt="photo">"#;
        let result = remove_tracking_pixels(html);
        assert!(result.contains("photo.jpg"), "normal images must not be removed");
    }

    #[test]
    fn test_sanitize_removes_iframes() {
        let html = r#"<div><iframe src="https://ads.example.com"></iframe><p>Content</p></div>"#;
        let result = sanitize(html);
        assert!(!result.contains("<iframe"), "iframe must be removed");
        assert!(!result.contains("ads.example.com"), "iframe src must be removed");
        assert!(result.contains("Content"), "text content preserved");
    }

    #[test]
    fn test_sanitize_removes_noscript() {
        let html = r#"<p>Main</p><noscript><img src="https://track.com/ns"></noscript><p>End</p>"#;
        let result = sanitize(html);
        assert!(!result.contains("noscript"), "noscript removed");
        assert!(!result.contains("track.com/ns"), "noscript content removed");
    }

    #[test]
    fn test_sanitize_removes_style_blocks() {
        let html = r#"<style>.foo { color: red; }</style><p>Article</p>"#;
        let result = sanitize(html);
        assert!(!result.contains(".foo"), "style content removed");
        assert!(result.contains("<p>Article</p>"), "article preserved");
    }

    #[test]
    fn test_remove_chrome_elements() {
        let html = r#"<nav><a href="/">Home</a></nav><main><p>Article</p></main><footer>Copyright</footer>"#;
        let result = remove_chrome_elements(html);
        assert!(!result.contains("<nav>"), "nav removed");
        assert!(!result.contains("Copyright"), "footer content removed");
        assert!(result.contains("Article"), "main content preserved");
    }

    #[test]
    fn test_convert_links_to_markdown() {
        let html = r#"<a href="https://example.com/page">Click here</a>"#;
        let result = convert_links_to_markdown(html);
        assert!(result.contains("[Click here](https://example.com/page)"), "link converted to markdown: {}", result);
    }

    #[test]
    fn test_link_with_inner_tags_stripped() {
        let html = r#"<a href="https://example.com"><strong>Bold link</strong></a>"#;
        let result = convert_links_to_markdown(html);
        assert!(result.contains("[Bold link](https://example.com)"), "inner tags stripped from link text: {}", result);
    }

    #[test]
    fn test_javascript_links_not_preserved_as_links() {
        let html = r#"<a href="javascript:void(0)">Click</a>"#;
        let result = convert_links_to_markdown(html);
        // Should not produce a markdown link with javascript: protocol
        assert!(!result.contains("javascript:"), "javascript links must not pass through");
    }

    #[test]
    fn test_block_tags_produce_newlines() {
        let html = "<p>First</p><p>Second</p>";
        let result = convert_block_tags_to_newlines(html);
        assert!(result.contains('\n'), "block tags should produce newlines");
        assert!(result.contains("First"), "content preserved");
        assert!(result.contains("Second"), "content preserved");
    }

    #[test]
    fn test_strip_all_tags() {
        let html = "<div class=\"foo\"><p>Hello <strong>world</strong></p></div>";
        let result = strip_all_tags(html);
        assert!(!result.contains('<'), "all tags stripped: {}", result);
        assert!(result.contains("Hello"), "text preserved");
        assert!(result.contains("world"), "nested text preserved");
    }

    #[test]
    fn test_collapse_whitespace() {
        let text = "  hello   world  \n\n\n\n  foo  \n  bar  ";
        let result = collapse_whitespace(text);
        assert!(!result.contains("   "), "runs of spaces collapsed");
        // 4 consecutive newlines should collapse to 2
        assert!(!result.contains("\n\n\n"), "too many consecutive newlines collapsed");
        assert!(result.contains("hello"), "content preserved");
    }

    #[test]
    fn test_full_sanitize_pipeline() {
        let html = r#"
            <html><head><style>body{color:red}</style></head>
            <body>
              <nav><a href="/home">Home</a></nav>
              <script>document.write('evil');</script>
              <article>
                <h1>Title</h1>
                <p>Paragraph with <a href="https://example.com">a link</a>.</p>
                <img src="https://track.com/p.gif" width="1" height="1">
                <p>Second paragraph.</p>
              </article>
              <footer>Footer text</footer>
              <iframe src="https://ads.com"></iframe>
            </body></html>
        "#;
        let sanitized = sanitize(html);
        assert!(!sanitized.contains("evil"), "script content removed");
        assert!(!sanitized.contains(".color"), "style removed");
        assert!(!sanitized.contains("track.com"), "tracking pixel removed");
        assert!(!sanitized.contains("ads.com"), "iframe removed");
        assert!(sanitized.contains("Title"), "title preserved");
        assert!(sanitized.contains("Paragraph"), "paragraph preserved");
    }
}
