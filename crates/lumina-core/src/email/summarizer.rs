//! P2-08: Email summarization prompt builder
//!
//! Constructs LLM prompts for email summarization.  The actual LLM call is
//! performed by the caller (Vigil, agent_loop, etc.); this module is
//! responsible only for:
//!
//! 1. Building a well-structured summarization prompt from a list of messages.
//! 2. Ensuring email bodies pass through `output_filter` before being included
//!    in the prompt (prevents PII/secret leakage into LLM context).
//! 3. Enforcing per-message body truncation (max 2 KB per large email).
//!
//! ## Design principle
//!
//! The summarizer never calls an LLM directly — it produces a `String` prompt
//! that the caller passes to the inference tier.  This keeps the module
//! testable without a live LLM.

use crate::email::EmailMessage;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum characters to include per-message body in the summarization prompt.
/// Emails larger than this are truncated to prevent token bloat.
pub const MAX_BODY_CHARS_PER_MESSAGE: usize = 2_048;

/// Maximum number of messages to include in a single summarization prompt.
/// If there are more unread messages the oldest ones are omitted.
pub const MAX_MESSAGES_IN_PROMPT: usize = 20;

// ── OutputFilter stub ─────────────────────────────────────────────────────────

/// Redact patterns that should never appear in LLM context.
///
/// This is a lightweight client-side filter applied before email content is
/// sent to an LLM.  It is intentionally conservative — false positives (over-
/// redaction) are preferred over false negatives.
///
/// Patterns redacted:
/// - Sequences that look like passwords/tokens (bearer tokens, API keys, etc.)
/// - Credit card numbers (16-digit sequences)
/// - Social Security / national ID numbers (e.g. `NNN-NN-NNNN`)
pub fn apply_output_filter(text: &str) -> String {
    let mut out = text.to_string();

    // Redact bearer tokens / API keys: long alphanumeric/base64 runs that
    // follow keywords like "token", "key", "password", "secret", "Bearer".
    //
    // We search on a lowercased *copy* to find the match offset, then use
    // that offset on the *original* string.  This is safe for ASCII-only
    // keywords (all our keywords are ASCII), since ASCII lowercasing preserves
    // byte length and therefore byte offsets are identical between the
    // original and the lowercased copy.
    let token_keywords = ["Bearer ", "token=", "key=", "password=", "secret=", "api_key="];
    for kw in &token_keywords {
        let kw_lower = kw.to_lowercase();
        // Safety: `kw` is all-ASCII so `kw.len() == kw_lower.len()` always.
        debug_assert!(kw.len() == kw_lower.len(), "keyword must be ASCII-only");
        loop {
            let out_lower = out.to_lowercase();
            let idx = match out_lower.find(&kw_lower) {
                Some(i) => i,
                None => break,
            };
            // `idx` is a byte offset valid in `out` because the keyword is ASCII
            // and ASCII chars have identical byte lengths in any Unicode string.
            let after = &out[idx + kw.len()..];
            // Measure the run in characters then convert to bytes for slicing.
            let run_chars: usize = after
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '-' | '_' | '.'))
                .count();
            let run_bytes: usize = after
                .chars()
                .take(run_chars)
                .map(|c| c.len_utf8())
                .sum();
            if run_chars >= 16 {
                let replacement = format!("{}[REDACTED]", kw);
                out = format!("{}{}{}", &out[..idx], replacement, &out[idx + kw.len() + run_bytes..]);
            } else {
                break;
            }
        }
    }

    // Redact credit card numbers: 4 groups of 4 digits.
    let cc_pattern = regex_replace_simple(
        &out,
        |window: &str| is_cc_pattern(window),
        "[CC-REDACTED]",
    );
    out = cc_pattern;

    // Redact SSN patterns: NNN-NN-NNNN.
    let ssn_pattern = regex_replace_simple(
        &out,
        |window: &str| is_ssn_pattern(window),
        "[SSN-REDACTED]",
    );
    out = ssn_pattern;

    out
}

/// Check if a string window looks like a credit card number (`NNNN NNNN NNNN NNNN`
/// or `NNNN-NNNN-NNNN-NNNN`).
fn is_cc_pattern(s: &str) -> bool {
    let digits_only: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits_only.len() != 16 {
        return false;
    }
    // Check structure: 4 groups of 4 separated by space or dash.
    let parts: Vec<&str> = if s.contains('-') {
        s.split('-').collect()
    } else if s.contains(' ') {
        s.split(' ').collect()
    } else {
        return false;
    };
    parts.len() == 4 && parts.iter().all(|p| p.len() == 4 && p.chars().all(|c| c.is_ascii_digit()))
}

/// Check if a string window looks like a US SSN (`NNN-NN-NNNN`).
fn is_ssn_pattern(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 3
        && parts[0].len() == 3
        && parts[1].len() == 2
        && parts[2].len() == 4
        && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()))
}

/// A simple sliding-window scan-and-replace for patterns that are hard to
/// express cleanly without pulling in the `regex` crate for a trivial use-case.
///
/// Scans `text` for any maximal run that satisfies `matcher`, then replaces it
/// with `replacement`.  This is O(n²) in the worst case but email previews are
/// short so it is acceptable.
fn regex_replace_simple<F>(text: &str, matcher: F, replacement: &str) -> String
where
    F: Fn(&str) -> bool,
{
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        // Try windows of decreasing size starting at position i.
        let mut matched = false;
        for window_len in (7..=19).rev() {
            if i + window_len <= len {
                let window: String = chars[i..i + window_len].iter().collect();
                if matcher(&window) {
                    out.push_str(replacement);
                    i += window_len;
                    matched = true;
                    break;
                }
            }
        }
        if !matched {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

// ── Prompt builder ────────────────────────────────────────────────────────────

/// Build a summarization prompt from a list of email messages.
///
/// Each message body is:
/// 1. Passed through `apply_output_filter` to redact PII/secrets.
/// 2. Truncated to `MAX_BODY_CHARS_PER_MESSAGE` characters.
///
/// The prompt instructs the LLM to summarize the emails concisely and
/// highlight messages that require action.
///
/// Returns an empty string if `messages` is empty.
pub fn build_summarize_prompt(messages: &[EmailMessage]) -> String {
    if messages.is_empty() {
        return String::new();
    }

    let messages_to_include = if messages.len() > MAX_MESSAGES_IN_PROMPT {
        &messages[..MAX_MESSAGES_IN_PROMPT]
    } else {
        messages
    };

    let mut prompt = String::new();
    prompt.push_str("Summarize the following unread emails concisely. ");
    prompt.push_str("Highlight any messages that require action. ");
    prompt.push_str("If an email appears to be spam or automated, note that briefly.\n\n");
    prompt.push_str("---\n");

    for (i, msg) in messages_to_include.iter().enumerate() {
        let filtered_body = apply_output_filter(&msg.body_preview);
        // Truncate on char boundary, not byte boundary (avoids panic on non-ASCII).
        let truncated_body = {
            let chars: Vec<char> = filtered_body.chars().collect();
            if chars.len() > MAX_BODY_CHARS_PER_MESSAGE {
                let s: String = chars[..MAX_BODY_CHARS_PER_MESSAGE].iter().collect();
                format!("{}…", s.trim_end())
            } else {
                filtered_body
            }
        };

        prompt.push_str(&format!(
            "Email {}:\nFrom: {}\nDate: {}\nSubject: {}\nPreview: {}\n---\n",
            i + 1,
            msg.from,
            msg.date,
            msg.subject,
            truncated_body,
        ));
    }

    prompt.push_str("\nProvide a concise summary of the above emails.");
    prompt
}

/// Build a Vigil briefing snippet for email status.
///
/// Returns a short plain-text string suitable for inclusion in a morning
/// briefing, e.g. "3 unread emails — action needed: Invoice due, Meeting request".
///
/// If there are no unread emails, returns `"Inbox clear — no unread emails."`.
pub fn build_vigil_email_snippet(messages: &[EmailMessage]) -> String {
    if messages.is_empty() {
        return "Inbox clear — no unread emails.".to_string();
    }

    let count = messages.len();
    let subjects: Vec<String> = messages
        .iter()
        .take(3)
        .map(|m| m.subject.clone())
        .collect();

    if count == 1 {
        format!("1 unread email: {}", subjects[0])
    } else if subjects.len() < count {
        format!(
            "{} unread emails — highlights: {}… and {} more",
            count,
            subjects.join(", "),
            count - subjects.len()
        )
    } else {
        format!("{} unread emails — highlights: {}", count, subjects.join(", "))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_message(uid: &str, subject: &str, from: &str, body: &str) -> EmailMessage {
        EmailMessage {
            uid: uid.to_string(),
            subject: subject.to_string(),
            from: from.to_string(),
            date: "Thu, 01 Jan 2026 12:00:00 +0000".to_string(),
            body_preview: body.to_string(),
        }
    }

    // ── build_summarize_prompt ────────────────────────────────────────────────

    #[test]
    fn test_build_summarize_prompt_empty() {
        assert_eq!(build_summarize_prompt(&[]), "");
    }

    #[test]
    fn test_build_summarize_prompt_contains_headers() {
        let msgs = vec![
            make_message("1", "Budget review", "alice@example.com", "Please review Q1 budget."),
        ];
        let prompt = build_summarize_prompt(&msgs);
        assert!(prompt.contains("Budget review"), "prompt should include subject");
        assert!(prompt.contains("alice@example.com"), "prompt should include sender");
        assert!(prompt.contains("Please review Q1 budget."), "prompt should include body preview");
    }

    #[test]
    fn test_build_summarize_prompt_multiple_emails_numbered() {
        let msgs = vec![
            make_message("1", "Subject A", "a@example.com", "Body A"),
            make_message("2", "Subject B", "b@example.com", "Body B"),
        ];
        let prompt = build_summarize_prompt(&msgs);
        assert!(prompt.contains("Email 1:"), "should number emails starting at 1");
        assert!(prompt.contains("Email 2:"), "should number second email");
    }

    #[test]
    fn test_build_summarize_prompt_truncates_long_body() {
        let long_body = "X".repeat(3000);
        let msgs = vec![make_message("1", "Long email", "sender@example.com", &long_body)];
        let prompt = build_summarize_prompt(&msgs);
        // Body preview in prompt should be truncated.
        assert!(
            prompt.len() < 5000,
            "prompt should be significantly shorter than raw body length"
        );
        assert!(prompt.contains('…'), "truncated body should end with ellipsis in prompt");
    }

    #[test]
    fn test_build_summarize_prompt_caps_at_max_messages() {
        let msgs: Vec<EmailMessage> = (0..30)
            .map(|i| make_message(&i.to_string(), &format!("Subject {i}"), "x@example.com", "Body"))
            .collect();
        let prompt = build_summarize_prompt(&msgs);
        // Only MAX_MESSAGES_IN_PROMPT = 20 should appear.
        assert!(
            !prompt.contains("Email 21:"),
            "prompt should not include more than MAX_MESSAGES_IN_PROMPT emails"
        );
    }

    // ── apply_output_filter ───────────────────────────────────────────────────

    #[test]
    fn test_output_filter_plain_text_unchanged() {
        let text = "Here is your meeting summary for today.";
        // No PII patterns — should pass through unchanged (or at most trimmed).
        let filtered = apply_output_filter(text);
        assert!(filtered.contains("meeting summary"), "plain text should pass through");
    }

    #[test]
    fn test_output_filter_redacts_bearer_token() {
        let text = "Authorization: Bearer eyJhbGciOiJSUzI1NiJ9abcdefghijklmnopqrstuvwxyz1234";
        let filtered = apply_output_filter(text);
        assert!(
            filtered.contains("[REDACTED]"),
            "bearer token should be redacted"
        );
        assert!(
            !filtered.contains("eyJhbGciOiJSUzI1NiJ9"),
            "token value should not appear in filtered output"
        );
    }

    #[test]
    fn test_output_filter_redacts_ssn() {
        let text = "Your SSN: 123-45-6789 has been updated.";
        let filtered = apply_output_filter(text);
        assert!(
            filtered.contains("[SSN-REDACTED]"),
            "SSN should be redacted: {}", filtered
        );
        assert!(
            !filtered.contains("123-45-6789"),
            "SSN value should not appear in filtered output"
        );
    }

    #[test]
    fn test_output_filter_applied_before_llm_in_prompt() {
        // A body preview that contains a bearer token should be redacted in the prompt.
        let sensitive_body = "Click here: Bearer abcdefghijklmnopqrstuvwxyz1234567890";
        let msgs = vec![make_message("1", "Important", "sec@example.com", sensitive_body)];
        let prompt = build_summarize_prompt(&msgs);
        assert!(
            !prompt.contains("abcdefghijklmnopqrstuvwxyz1234567890"),
            "sensitive token should not appear in LLM prompt"
        );
    }

    // ── build_vigil_email_snippet ─────────────────────────────────────────────

    #[test]
    fn test_vigil_snippet_no_unread() {
        let snippet = build_vigil_email_snippet(&[]);
        assert!(snippet.contains("Inbox clear"), "should indicate inbox is empty");
    }

    #[test]
    fn test_vigil_snippet_single_email() {
        let msgs = vec![make_message("1", "Invoice #1234", "billing@example.com", "Your invoice...")];
        let snippet = build_vigil_email_snippet(&msgs);
        assert!(snippet.contains("1 unread email"), "should report count");
        assert!(snippet.contains("Invoice #1234"), "should include subject");
    }

    #[test]
    fn test_vigil_snippet_multiple_emails() {
        let msgs: Vec<EmailMessage> = (0..5)
            .map(|i| make_message(&i.to_string(), &format!("Subject {i}"), "x@example.com", "Body"))
            .collect();
        let snippet = build_vigil_email_snippet(&msgs);
        assert!(snippet.contains("5 unread emails"), "should report total count");
    }

    #[test]
    fn test_vigil_snippet_shows_at_most_three_subjects() {
        let msgs: Vec<EmailMessage> = (0..6)
            .map(|i| make_message(&i.to_string(), &format!("Subject {i}"), "x@example.com", "Body"))
            .collect();
        let snippet = build_vigil_email_snippet(&msgs);
        // Should mention "more" since there are >3 messages.
        assert!(snippet.contains("more"), "should indicate there are more emails beyond the 3 shown");
    }
}
