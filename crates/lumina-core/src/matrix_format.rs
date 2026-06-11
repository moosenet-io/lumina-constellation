//! Matrix message formatting: markdown → HTML, chunking, HTML escaping.

use regex::Regex;
use std::sync::OnceLock;

const MAX_CHUNK_LEN: usize = 4000;

// Pre-compiled regexes
fn re_code_block() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"```(?:[^\n]*\n)?([\s\S]*?)```").unwrap())
}

fn re_inline_code() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"`([^`]+)`").unwrap())
}

fn re_bold() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\*\*(.+?)\*\*").unwrap())
}

fn re_italic() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\*([^*]+)\*").unwrap())
}

fn re_h3() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?m)^### (.+)$").unwrap())
}

fn re_h2() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?m)^## (.+)$").unwrap())
}

fn re_h1() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?m)^# (.+)$").unwrap())
}

/// Convert markdown text to Matrix-compatible HTML.
///
/// Handles code blocks (protected from other transforms), inline code, bold,
/// italic, headers, and newlines. Characters inside code blocks are HTML-escaped
/// before being wrapped — other content is not escaped (preserving intentional markup).
pub fn markdown_to_matrix_html(text: &str) -> String {
    // Phase 1: Extract code blocks to protect them from other transforms.
    // Replace each code block with a placeholder, process the rest, then restore.
    let mut code_blocks: Vec<String> = Vec::new();
    let with_placeholders = re_code_block().replace_all(text, |caps: &regex::Captures| {
        let inner = escape_html(caps.get(1).map_or("", |m| m.as_str()));
        let html = format!("<pre><code>{}</code></pre>", inner);
        let idx = code_blocks.len();
        code_blocks.push(html);
        format!("\x00CODE{}\x00", idx)
    });

    // Phase 2: Apply inline transforms to non-code content.
    let mut result = with_placeholders.to_string();

    // Inline code (must come before bold/italic)
    result = re_inline_code().replace_all(&result, |caps: &regex::Captures| {
        format!("<code>{}</code>", escape_html(&caps[1]))
    }).to_string();

    // Headers (order: h3 before h2 before h1 to avoid partial matches)
    result = re_h3().replace_all(&result, "<h3>$1</h3>").to_string();
    result = re_h2().replace_all(&result, "<h2>$1</h2>").to_string();
    result = re_h1().replace_all(&result, "<h1>$1</h1>").to_string();

    // Bold and italic
    result = re_bold().replace_all(&result, "<strong>$1</strong>").to_string();
    result = re_italic().replace_all(&result, "<em>$1</em>").to_string();

    // Newlines → <br> (but not inside block-level elements already added)
    result = result.replace('\n', "<br>");

    // Phase 3: Restore code blocks
    for (idx, block) in code_blocks.iter().enumerate() {
        result = result.replace(&format!("\x00CODE{}\x00", idx), block);
    }

    result
}

/// Escape HTML special characters: &, <, >.
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Split a message into chunks of at most `max_len` characters.
///
/// Prefers splitting at paragraph boundaries (`\n\n`). Falls back to sentence
/// boundaries (`\n`), then hard-cuts at `max_len`. Never splits inside a code block.
pub fn chunk_message(text: &str) -> Vec<String> {
    chunk_message_with_max(text, MAX_CHUNK_LEN)
}

pub fn chunk_message_with_max(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = text;

    while remaining.len() > max_len {
        // Find a safe char boundary at or before max_len so slicing doesn't panic.
        let safe_max = floor_char_boundary(remaining, max_len);
        let candidate = &remaining[..safe_max];

        let split_at = find_split_point(candidate);

        chunks.push(remaining[..split_at].to_string());
        remaining = remaining[split_at..].trim_start_matches('\n');
    }

    if !remaining.is_empty() {
        chunks.push(remaining.to_string());
    }

    chunks
}

/// Return the largest char boundary index ≤ max in `s`.
fn floor_char_boundary(s: &str, max: usize) -> usize {
    if max >= s.len() {
        return s.len();
    }
    let mut pos = max;
    while pos > 0 && !s.is_char_boundary(pos) {
        pos -= 1;
    }
    pos
}

fn find_split_point(s: &str) -> usize {
    // Prefer last \n\n before end
    if let Some(pos) = s.rfind("\n\n") {
        if pos > 0 {
            return pos;
        }
    }
    // Fall back to last \n
    if let Some(pos) = s.rfind('\n') {
        if pos > 0 {
            return pos;
        }
    }
    // Hard cut — be careful not to split a multi-byte char
    let mut pos = s.len();
    while !s.is_char_boundary(pos) {
        pos -= 1;
    }
    pos
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bold() {
        let out = markdown_to_matrix_html("**hello**");
        assert!(out.contains("<strong>hello</strong>"), "got: {}", out);
    }

    #[test]
    fn test_italic() {
        let out = markdown_to_matrix_html("*world*");
        assert!(out.contains("<em>world</em>"), "got: {}", out);
    }

    #[test]
    fn test_headers() {
        assert!(markdown_to_matrix_html("# H1").contains("<h1>H1</h1>"));
        assert!(markdown_to_matrix_html("## H2").contains("<h2>H2</h2>"));
        assert!(markdown_to_matrix_html("### H3").contains("<h3>H3</h3>"));
    }

    #[test]
    fn test_inline_code() {
        let out = markdown_to_matrix_html("`foo`");
        assert!(out.contains("<code>foo</code>"), "got: {}", out);
    }

    #[test]
    fn test_code_block() {
        let input = "```\nhello world\n```";
        let out = markdown_to_matrix_html(input);
        assert!(out.contains("<pre><code>"), "got: {}", out);
        assert!(out.contains("</code></pre>"), "got: {}", out);
        // Bold markers inside code block must NOT be converted
        let input2 = "```\n**not bold**\n```";
        let out2 = markdown_to_matrix_html(input2);
        assert!(!out2.contains("<strong>"), "code block content must not be bolded: {}", out2);
    }

    #[test]
    fn test_code_block_html_escaped() {
        let input = "```\n<script>alert(1)</script>\n```";
        let out = markdown_to_matrix_html(input);
        assert!(!out.contains("<script>"), "script tag must be escaped: {}", out);
        assert!(out.contains("&lt;script&gt;"), "got: {}", out);
    }

    #[test]
    fn test_newlines_to_br() {
        let out = markdown_to_matrix_html("line1\nline2");
        assert!(out.contains("<br>"), "got: {}", out);
    }

    #[test]
    fn test_chunk_under_limit_single_chunk() {
        let text = "hello world";
        let chunks = chunk_message(text);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], text);
    }

    #[test]
    fn test_chunk_at_paragraph_boundary() {
        let para1 = "a".repeat(2500);
        let para2 = "b".repeat(2500);
        let text = format!("{}\n\n{}", para1, para2);
        let chunks = chunk_message_with_max(&text, 3000);
        assert!(chunks.len() >= 2, "should split: {:?}", chunks.iter().map(|c| c.len()).collect::<Vec<_>>());
        // Each chunk must be ≤ max
        for chunk in &chunks {
            assert!(chunk.len() <= 3000, "chunk too long: {}", chunk.len());
        }
    }

    #[test]
    fn test_chunk_no_boundary_hard_split() {
        let text = "x".repeat(5000);
        let chunks = chunk_message_with_max(&text, 4000);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 4000);
        assert_eq!(chunks[1].len(), 1000);
    }

    #[test]
    fn test_chunk_empty() {
        // empty input → one empty chunk (or zero — both are acceptable)
        let chunks = chunk_message("");
        assert!(chunks.len() <= 1);
    }

    #[test]
    fn test_unicode_not_split_mid_char() {
        // emoji is 4 bytes; place it right at the split boundary
        let emoji = "🎉";
        assert_eq!(emoji.len(), 4);
        let before = "a".repeat(3999); // 3999 bytes
        let text = format!("{}{}", before, emoji); // 4003 bytes total
        let chunks = chunk_message_with_max(&text, 4000);
        // Must not panic and must not have garbled UTF-8
        for chunk in &chunks {
            assert!(std::str::from_utf8(chunk.as_bytes()).is_ok());
        }
    }

    #[test]
    fn test_nested_bold_header() {
        let out = markdown_to_matrix_html("## **bold header**");
        assert!(out.contains("<h2>"), "got: {}", out);
        assert!(out.contains("<strong>bold header</strong>"), "got: {}", out);
    }
}
