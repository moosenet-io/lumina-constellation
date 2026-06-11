//! Readable content extraction for the Lumina web client.
//!
//! Converts sanitized HTML into plain text suitable for summarization.
//!
//! The extraction pipeline:
//! 1. Convert `<a>` tags to `[text](url)` markdown
//! 2. Convert block-level tags (`<p>`, `<h1>`–`<h6>`, `<li>`, …) to newlines
//! 3. Strip all remaining HTML tags
//! 4. Collapse whitespace
//! 5. Decode common HTML entities

use crate::web::sanitizer::{
    collapse_whitespace, convert_block_tags_to_newlines, convert_links_to_markdown,
    strip_all_tags,
};

/// Extract readable plain text from sanitized HTML.
///
/// The input should have already been run through [`crate::web::sanitizer::sanitize`].
/// This function handles the conversion from HTML to readable markdown-ish text.
pub fn extract_text(html: &str) -> String {
    let s = convert_links_to_markdown(html);
    let s = convert_block_tags_to_newlines(&s);
    let s = strip_all_tags(&s);
    let s = decode_html_entities(&s);
    collapse_whitespace(&s)
}

/// Decode common HTML entities to their Unicode equivalents.
pub fn decode_html_entities(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
        .replace("&hellip;", "…")
        .replace("&copy;", "©")
        .replace("&reg;", "®")
        .replace("&trade;", "™")
        .replace("&laquo;", "«")
        .replace("&raquo;", "»")
        .replace("&ldquo;", "\u{201C}")
        .replace("&rdquo;", "\u{201D}")
        .replace("&lsquo;", "\u{2018}")
        .replace("&rsquo;", "\u{2019}")
}

/// Extract the page title from an HTML document.
///
/// Looks for `<title>…</title>` in the head. Returns an empty string if no
/// title tag is found.
///
/// # UTF-8 correctness
/// All searches are performed on the *original* string (or case-folded copies
/// of substrings) to avoid the byte-length mismatch that would occur if we
/// searched a fully-lowercased version of the document and then used those
/// byte offsets to index the original.  The comparison character at each
/// candidate position is lowercased individually so we never assume that
/// `to_lowercase()` is length-preserving (it is not for e.g. `ß` → `ss`).
pub fn extract_title(html: &str) -> String {
    // Find `<title` case-insensitively in the original string.
    // We scan byte-by-byte looking for a case-insensitive ASCII match;
    // non-ASCII bytes are never equal to ASCII '<', 't', 'i', etc., so they
    // are safely skipped.
    let needle_open = b"<title";
    let bytes = html.as_bytes();

    let tag_start = 'outer: {
        for i in 0..bytes.len().saturating_sub(needle_open.len()) {
            let slice = &bytes[i..i + needle_open.len()];
            let matches = slice
                .iter()
                .zip(needle_open.iter())
                .all(|(b, n)| b.to_ascii_lowercase() == *n);
            if matches {
                break 'outer Some(i);
            }
        }
        None
    };

    let tag_start = match tag_start {
        Some(pos) => pos,
        None => return String::new(),
    };

    // Find the `>` that closes the opening tag.
    let rest = &html[tag_start..];
    let content_start = match rest.find('>') {
        Some(pos) => pos,
        None => return String::new(),
    };
    let after_open = &rest[content_start + 1..];

    // Find `</title` case-insensitively in `after_open` (same original-string
    // technique — no full lowercasing of `after_open`).
    let needle_close = b"</title";
    let after_bytes = after_open.as_bytes();
    let end = 'outer2: {
        for i in 0..after_bytes.len().saturating_sub(needle_close.len()) {
            let slice = &after_bytes[i..i + needle_close.len()];
            let matches = slice
                .iter()
                .zip(needle_close.iter())
                .all(|(b, n)| b.to_ascii_lowercase() == *n);
            if matches {
                break 'outer2 Some(i);
            }
        }
        None
    };

    match end {
        Some(pos) => {
            let raw = &after_open[..pos];
            decode_html_entities(raw.trim())
        }
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_extraction_from_complex_html() {
        let html = r#"
            <article>
              <h1>Main Title</h1>
              <p>First paragraph with <a href="https://example.com">a link</a>.</p>
              <ul>
                <li>Item one</li>
                <li>Item two</li>
              </ul>
              <p>Second paragraph &amp; more content.</p>
            </article>
        "#;
        let result = extract_text(html);
        assert!(result.contains("Main Title"), "h1 text preserved: {}", result);
        assert!(result.contains("First paragraph"), "paragraph preserved: {}", result);
        assert!(result.contains("[a link](https://example.com)"), "link converted: {}", result);
        assert!(result.contains("Item one"), "list item preserved: {}", result);
        assert!(result.contains("Item two"), "list item preserved: {}", result);
        assert!(result.contains("Second paragraph & more content"), "entity decoded: {}", result);
        assert!(!result.contains('<'), "no HTML tags remain: {}", result);
    }

    #[test]
    fn test_extract_title() {
        let html = "<html><head><title>My Page &amp; Title</title></head><body></body></html>";
        let title = extract_title(html);
        assert_eq!(title, "My Page & Title");
    }

    #[test]
    fn test_extract_title_missing() {
        let html = "<html><head></head><body><p>No title</p></body></html>";
        let title = extract_title(html);
        assert_eq!(title, "");
    }

    #[test]
    fn test_extract_title_uppercase_tag() {
        // Mixed-case <TITLE> tag must still be found.
        let html = "<html><head><TITLE>Upper Case Title</TITLE></head><body></body></html>";
        let title = extract_title(html);
        assert_eq!(title, "Upper Case Title");
    }

    #[test]
    fn test_extract_title_mixed_case_tag() {
        let html = "<html><head><Title>Mixed Case</Title></head><body></body></html>";
        let title = extract_title(html);
        assert_eq!(title, "Mixed Case");
    }

    #[test]
    fn test_extract_title_no_closing_tag() {
        // No </title> — should return empty string, not panic.
        let html = "<html><head><title>Unclosed title</head><body></body></html>";
        let title = extract_title(html);
        // Without a closing tag we cannot extract a title — expect empty.
        assert_eq!(title, "");
    }

    #[test]
    fn test_extract_title_utf8_multibyte() {
        // Title starting with multi-byte UTF-8 characters.
        // Verifies that lowercasing does not shift byte offsets.
        let html = "<html><head><title>Über alles &amp; Straße</title></head><body></body></html>";
        let title = extract_title(html);
        assert_eq!(title, "Über alles & Straße");
    }

    #[test]
    fn test_extract_title_utf8_at_document_start() {
        // Document starts with a multi-byte character before <title>.
        let html = "<!-- ü --><html><head><title>UTF Start</title></head></html>";
        let title = extract_title(html);
        assert_eq!(title, "UTF Start");
    }

    #[test]
    fn test_extract_title_cjk_content() {
        // CJK characters are 3 bytes each in UTF-8 — byte length != char length.
        let html = "<html><head><title>日本語タイトル</title></head></html>";
        let title = extract_title(html);
        assert_eq!(title, "日本語タイトル");
    }

    #[test]
    fn test_decode_html_entities() {
        assert_eq!(decode_html_entities("&amp;"), "&");
        assert_eq!(decode_html_entities("&lt;b&gt;"), "<b>");
        assert_eq!(decode_html_entities("&quot;hello&quot;"), "\"hello\"");
        assert_eq!(decode_html_entities("&nbsp;"), " ");
        assert_eq!(decode_html_entities("&mdash;"), "—");
    }

    #[test]
    fn test_extract_text_no_html_tags_in_output() {
        let html = "<div><p>Some <strong>bold</strong> text.</p><aside>Sidebar</aside></div>";
        let result = extract_text(html);
        assert!(!result.contains('<'), "should have no HTML tags: {}", result);
        assert!(result.contains("bold"), "nested text preserved: {}", result);
    }

    #[test]
    fn test_extract_text_empty_input() {
        let result = extract_text("");
        assert!(result.is_empty(), "empty input → empty output");
    }

    #[test]
    fn test_extract_text_preserves_headings() {
        let html = "<h1>Heading 1</h1><h2>Heading 2</h2><h3>Heading 3</h3>";
        let result = extract_text(html);
        assert!(result.contains("Heading 1"), "h1 preserved: {}", result);
        assert!(result.contains("Heading 2"), "h2 preserved: {}", result);
        assert!(result.contains("Heading 3"), "h3 preserved: {}", result);
    }
}
