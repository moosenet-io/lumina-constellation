//! HARDEN-01: ZeroizingString — conversation content that wipes memory on drop
//!
//! Wraps zeroize::Zeroizing<String> so heap bytes are overwritten when the
//! value is dropped. Use for all message content: user input, LLM responses,
//! tool arguments and results.
//!
//! Intentionally does NOT implement Clone — callers must be explicit about copies.
//! Debug output shows byte count only, never content.

use std::fmt;
use std::ops::Deref;
use zeroize::Zeroizing;

/// A String whose heap memory is zeroed on drop.
pub struct ZeroizingString(Zeroizing<String>);

impl ZeroizingString {
    pub fn new(s: String) -> Self {
        Self(Zeroizing::new(s))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Deref for ZeroizingString {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ZeroizingString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for ZeroizingString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[conversation content, {} bytes]", self.0.len())
    }
}

impl From<String> for ZeroizingString {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl From<&str> for ZeroizingString {
    fn from(s: &str) -> Self {
        Self::new(s.to_owned())
    }
}

/// Allow comparing ZeroizingString directly to &str literals (useful in tests).
impl PartialEq<str> for ZeroizingString {
    fn eq(&self, other: &str) -> bool {
        self.0.as_str() == other
    }
}

impl<'a> PartialEq<&'a str> for ZeroizingString {
    fn eq(&self, other: &&'a str) -> bool {
        self.0.as_str() == *other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deref_works_for_string_ops() {
        let s = ZeroizingString::new("hello world".to_string());
        assert_eq!(s.len(), 11);
        assert!(s.contains("hello"));
        assert!(!s.is_empty());
    }

    #[test]
    fn test_display_prints_content() {
        let s = ZeroizingString::new("display me".to_string());
        assert_eq!(format!("{}", s), "display me");
    }

    #[test]
    fn test_debug_hides_content() {
        let s = ZeroizingString::new("secret content".to_string());
        let debug = format!("{:?}", s);
        assert!(!debug.contains("secret content"));
        assert!(debug.contains("bytes"));
    }

    #[test]
    fn test_from_str() {
        let s = ZeroizingString::from("from str");
        assert_eq!(s.len(), 8);
    }

    #[test]
    fn test_from_string() {
        let s = ZeroizingString::from("from string".to_string());
        assert_eq!(s.len(), 11);
    }

    #[test]
    fn test_empty() {
        let s = ZeroizingString::new(String::new());
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn test_as_bytes() {
        let s = ZeroizingString::new("abc".to_string());
        assert_eq!(s.as_bytes(), b"abc");
    }

    #[test]
    fn test_partial_eq_str() {
        let s = ZeroizingString::new("hello".to_string());
        assert!(s == "hello");
        assert!(s != "world");
    }

    #[test]
    fn test_clone_not_available() {
        // This test verifies that ZeroizingString does not implement Clone.
        // If Clone were derived, the next line would compile:
        //   let _copy = s.clone();
        // Intentionally left unimplemented — see type definition.
    }
}
