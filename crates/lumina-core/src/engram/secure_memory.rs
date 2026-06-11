//! ESEC-01: Memory content zeroization in retrieval pipeline.
//!
//! After retrieval, all memory content must be zeroized from heap memory when
//! the turn completes. This prevents heap scanning / memory dump attacks from
//! recovering private memory content after the LLM call.
//!
//! Pattern: SecureMemory wraps Memory content in a RedactedString so that
//! when the SecureMemory is dropped, the content is overwritten with zeros before
//! the memory is released to the allocator.
//!
//! Note: deliberately does NOT implement Clone — callers must be explicit about
//! copies of sensitive content (aligns with secure_string::ZeroizingString policy).

use std::fmt;
use zeroize::{Zeroize, ZeroizeOnDrop};

// ── RedactedString ────────────────────────────────────────────────────────────

/// A String whose heap bytes are zeroed on drop, with Debug/Display that
/// outputs `[REDACTED]` to prevent content leakage in logs or error messages.
///
/// Intentionally does NOT implement Clone — callers must be explicit about
/// creating copies of sensitive memory content.
///
/// Different from `secure_string::ZeroizingString`: that type shows byte count
/// in Debug (for conversation content tracing); this type shows `[REDACTED]`
/// because memory content must never appear in logs under any circumstance.
#[derive(ZeroizeOnDrop)]
pub struct RedactedString(String);

impl RedactedString {
    /// Create a new RedactedString, taking ownership of the content.
    pub fn new(s: String) -> Self {
        Self(s)
    }

    /// Borrow the string contents for reading.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Explicitly zeroize the content now (same effect as dropping, but callable inline).
    ///
    /// After calling this, `as_str()` returns an empty string.
    /// Useful for tests and for callers who want deterministic zeroing timing.
    pub fn zeroize_now(&mut self) {
        self.0.zeroize();
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for RedactedString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl fmt::Display for RedactedString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl PartialEq for RedactedString {
    fn eq(&self, other: &Self) -> bool {
        use subtle::ConstantTimeEq;
        self.0.as_bytes().ct_eq(other.0.as_bytes()).into()
    }
}

impl Eq for RedactedString {}

impl From<String> for RedactedString {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl From<&str> for RedactedString {
    fn from(s: &str) -> Self {
        Self::new(s.to_string())
    }
}

impl AsRef<str> for RedactedString {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::ops::Deref for RedactedString {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

// ── SecureMemory ───────────────────────────────────────────────────────────────

/// A retrieved memory with its content wrapped in a RedactedString.
///
/// When this struct is dropped (e.g. after the LLM turn completes and the
/// response is delivered), the content field is automatically zeroized.
///
/// Intentionally does NOT implement Clone — to copy a SecureMemory, callers
/// must explicitly acknowledge they are creating a second copy of sensitive data.
///
/// All other metadata fields (id, type, timestamps) are not sensitive and
/// don't need zeroization.
#[derive(Debug, ZeroizeOnDrop)]
pub struct SecureMemory {
    /// Memory UUID — not sensitive, kept as plain String.
    #[zeroize(skip)]
    pub id: String,
    /// Owner user ID — not sensitive.
    #[zeroize(skip)]
    pub user_id: String,
    /// Memory type label for context injection.
    #[zeroize(skip)]
    pub type_label: &'static str,
    /// The actual memory content — ZEROIZED ON DROP.
    pub content: RedactedString,
    /// Confidence score — not sensitive.
    #[zeroize(skip)]
    pub confidence: f32,
    /// Retrieval score from hybrid search.
    #[zeroize(skip)]
    pub rrf_score: f64,
}

impl SecureMemory {
    /// Construct a SecureMemory from a Memory, moving content into RedactedString.
    pub fn from_memory(memory: crate::engram::types::Memory, rrf_score: f64) -> Self {
        Self {
            id: memory.id,
            user_id: memory.user_id,
            type_label: memory.memory_type.type_label_static(),
            content: RedactedString::new(memory.content),
            confidence: memory.confidence,
            rrf_score,
        }
    }

    /// Construct from individual fields.
    pub fn from_result(
        id: String,
        user_id: String,
        type_label: &'static str,
        content: String,
        confidence: f32,
        rrf_score: f64,
    ) -> Self {
        Self {
            id,
            user_id,
            type_label,
            content: RedactedString::new(content),
            confidence,
            rrf_score,
        }
    }
}

// ── Context formatting ─────────────────────────────────────────────────────────

/// Format a list of SecureMemory items for LLM context injection.
///
/// Returns a `RedactedString` so the formatted block is also zeroized after use.
/// Priority ordering: Principle > Preference > Semantic > Episodic (lower index = higher priority).
///
/// The output is truncated to `max_tokens` approximate tokens (1 token ≈ 4 chars).
pub fn format_for_context(memories: &[SecureMemory], max_tokens: usize) -> RedactedString {
    if memories.is_empty() {
        return RedactedString::new(String::new());
    }

    let max_chars = max_tokens * 4;
    let mut out = String::from("## What I know about you:\n");

    for m in memories {
        if out.len() >= max_chars {
            out.push_str("[... more memories available, increase context budget to see all]\n");
            break;
        }
        let line = format!("{} {}\n", m.type_label, m.content.as_str());
        out.push_str(&line);
    }

    RedactedString::new(out)
}

// ── MemoryType extension ───────────────────────────────────────────────────────

impl crate::engram::types::MemoryType {
    /// Static string type label for use in SecureMemory (no allocation).
    pub fn type_label_static(&self) -> &'static str {
        match self {
            Self::Principle => "[Principle]",
            Self::Preference => "[Preference]",
            Self::Semantic => "[Fact]",
            Self::Episodic => "[Recent]",
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── RedactedString ─────────────────────────────────────────────────────────

    #[test]
    fn test_redacted_string_debug_redacts_content() {
        let s = RedactedString::new("shellfish allergy".to_string());
        let debug = format!("{s:?}");
        assert_eq!(debug, "[REDACTED]", "Debug must not reveal content");
        assert!(!debug.contains("shellfish"), "Debug must not contain content: {debug}");
    }

    #[test]
    fn test_redacted_string_display_redacts_content() {
        let s = RedactedString::new("bank account number 1234".to_string());
        let display = format!("{s}");
        assert_eq!(display, "[REDACTED]");
        assert!(!display.contains("bank"), "Display must not reveal content: {display}");
    }

    #[test]
    fn test_redacted_string_as_str_accessible() {
        let s = RedactedString::new("hello world".to_string());
        assert_eq!(s.as_str(), "hello world");
    }

    #[test]
    fn test_redacted_string_deref() {
        let s = RedactedString::new("test content".to_string());
        let borrowed: &str = &s;
        assert_eq!(borrowed, "test content");
    }

    #[test]
    fn test_redacted_string_len() {
        let s = RedactedString::new("hello".to_string());
        assert_eq!(s.len(), 5);
        assert!(!s.is_empty());
        let empty = RedactedString::new(String::new());
        assert!(empty.is_empty());
    }

    #[test]
    fn test_redacted_string_from_str() {
        let s = RedactedString::from("borrowed str");
        assert_eq!(s.as_str(), "borrowed str");
    }

    #[test]
    fn test_redacted_string_equality() {
        let a = RedactedString::new("same".to_string());
        let b = RedactedString::new("same".to_string());
        let c = RedactedString::new("different".to_string());
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    /// Verify that explicit zeroization clears the content.
    ///
    /// This tests the same mechanism that fires on drop (ZeroizeOnDrop derives
    /// a Drop impl that calls Zeroize::zeroize on the inner field). After calling
    /// zeroize_now(), the String is zeroed (bytes overwritten, len set to 0).
    #[test]
    fn test_zeroize_now_clears_content() {
        let mut s = RedactedString::new("sensitive personal information".to_string());
        assert!(!s.is_empty(), "Content should be present before zeroize");
        assert_eq!(s.as_str(), "sensitive personal information");
        s.zeroize_now();
        assert!(s.is_empty(), "Content should be cleared after zeroize_now()");
        assert_eq!(s.as_str(), "", "String bytes should be zeroed");
    }

    /// Verify that ZeroizeOnDrop behavior matches explicit zeroize: the two are
    /// equivalent by construction (ZeroizeOnDrop derives a Drop impl that calls
    /// the same Zeroize::zeroize).
    #[test]
    fn test_zeroize_on_drop_is_derived() {
        // We can't inspect freed heap memory (UB), but we CAN verify:
        // 1. The type compiles with #[derive(ZeroizeOnDrop)]
        // 2. The zeroize_now() method (same effect as drop) works
        // 3. After zeroize, the content is gone from the live struct
        let mut s = RedactedString::new("another secret".to_string());
        let before = s.as_str().to_string();
        s.zeroize_now();
        assert_eq!(before, "another secret", "Content was readable before zeroize");
        assert!(s.is_empty(), "Content gone after zeroize (same as what drop does)");
    }

    // ── SecureMemory ───────────────────────────────────────────────────────────

    #[test]
    fn test_secure_memory_debug_redacts_content() {
        let sm = SecureMemory::from_result(
            "id-1".to_string(),
            "user-alice".to_string(),
            "[Fact]",
            "salary is $100k".to_string(),
            0.9,
            0.5,
        );
        let debug = format!("{sm:?}");
        assert!(!debug.contains("salary"), "Debug must not reveal content: {debug}");
        assert!(!debug.contains("100k"), "Debug must not reveal content: {debug}");
    }

    #[test]
    fn test_secure_memory_content_accessible_via_deref() {
        let sm = SecureMemory::from_result(
            "id-1".to_string(),
            "system".to_string(),
            "[Preference]",
            "likes dark roast coffee".to_string(),
            0.95,
            0.8,
        );
        assert_eq!(sm.content.as_str(), "likes dark roast coffee");
    }

    #[test]
    fn test_secure_memory_from_memory() {
        use crate::engram::types::{Memory, MemoryType, SensitivityCategory};
        let mut m = Memory::new("user-bob", MemoryType::Preference, SensitivityCategory::General, "prefers tea");
        m.confidence = 0.88;
        let sm = SecureMemory::from_memory(m, 0.75);
        assert_eq!(sm.content.as_str(), "prefers tea");
        assert_eq!(sm.type_label, "[Preference]");
        assert!((sm.confidence - 0.88).abs() < 1e-6);
        assert!((sm.rrf_score - 0.75).abs() < 1e-9);
    }

    #[test]
    fn test_secure_memory_zeroize_on_drop() {
        // Verify that the content is accessible, then that zeroize_now() clears it
        // (same mechanism that fires on drop)
        let mut sm = SecureMemory::from_result(
            "id-zeroize".to_string(),
            "user".to_string(),
            "[Fact]",
            "highly sensitive data".to_string(),
            1.0,
            1.0,
        );
        assert_eq!(sm.content.as_str(), "highly sensitive data");
        sm.content.zeroize_now();
        assert!(sm.content.is_empty(), "Content must be cleared after explicit zeroize");
    }

    // ── format_for_context ────────────────────────────────────────────────────

    #[test]
    fn test_format_for_context_empty_returns_empty() {
        let result = format_for_context(&[], 1000);
        assert!(result.as_str().is_empty() || !result.as_str().contains('['));
    }

    #[test]
    fn test_format_for_context_includes_type_labels() {
        let memories = vec![
            SecureMemory::from_result("1".into(), "u".into(), "[Principle]", "values directness".into(), 0.9, 0.9),
            SecureMemory::from_result("2".into(), "u".into(), "[Preference]", "likes dark mode".into(), 0.8, 0.8),
            SecureMemory::from_result("3".into(), "u".into(), "[Fact]", "is a senior manager".into(), 0.7, 0.7),
        ];
        let context = format_for_context(&memories, 1000);
        let text = context.as_str();
        assert!(text.contains("[Principle]"), "Should include Principle label");
        assert!(text.contains("[Preference]"), "Should include Preference label");
        assert!(text.contains("[Fact]"), "Should include Fact label");
        assert!(text.contains("values directness"), "Should include content");
        assert!(text.contains("likes dark mode"), "Should include content");
    }

    #[test]
    fn test_format_for_context_result_is_redacted_string() {
        let memories = vec![
            SecureMemory::from_result("1".into(), "u".into(), "[Fact]", "sensitive fact".into(), 0.9, 0.9),
        ];
        let context = format_for_context(&memories, 500);
        // The return type is RedactedString — verify debug redacts it
        let debug = format!("{context:?}");
        assert_eq!(debug, "[REDACTED]", "Context result must be RedactedString: {debug}");
    }

    #[test]
    fn test_format_for_context_truncates_at_budget() {
        let memories: Vec<SecureMemory> = (0..100)
            .map(|i| SecureMemory::from_result(
                format!("id-{i}"),
                "u".to_string(),
                "[Fact]",
                format!("memory content number {i} with some padding text to make it longer"),
                0.5,
                0.5,
            ))
            .collect();
        let context = format_for_context(&memories, 50);
        let text = context.as_str();
        assert!(text.len() <= 400, "Should be truncated, got {} chars", text.len());
    }

    #[test]
    fn test_memory_type_label_static() {
        use crate::engram::types::MemoryType;
        assert_eq!(MemoryType::Principle.type_label_static(), "[Principle]");
        assert_eq!(MemoryType::Preference.type_label_static(), "[Preference]");
        assert_eq!(MemoryType::Semantic.type_label_static(), "[Fact]");
        assert_eq!(MemoryType::Episodic.type_label_static(), "[Recent]");
    }

    #[test]
    fn test_format_for_context_result_zeroize_on_drop() {
        let memories = vec![
            SecureMemory::from_result("1".into(), "u".into(), "[Fact]", "memory content".into(), 0.9, 0.9),
        ];
        let mut context = format_for_context(&memories, 500);
        assert!(!context.is_empty(), "Context should have content before zeroize");
        context.zeroize_now();
        assert!(context.is_empty(), "Context should be cleared after zeroize_now()");
    }
}
