//! GUARD-03: Input validation and sanitization
//!
//! Provides comprehensive input validation to prevent injection attacks,
//! malformed data, and other security threats.

use crate::error::{LuminaError, Result};
use std::sync::OnceLock;

/// Maximum input length to prevent DoS attacks
pub const MAX_INPUT_LENGTH: usize = 100_000; // 100KB

/// Maximum lines in input to prevent processing bombs
pub const MAX_INPUT_LINES: usize = 10_000;

/// Blocked patterns that indicate potential security threats
static SECURITY_PATTERNS: OnceLock<Vec<&'static str>> = OnceLock::new();

/// Input validation rules and sanitization
pub struct InputValidator {
    max_length: usize,
    max_lines: usize,
    allow_html: bool,
    allow_script_tags: bool,
}

impl Default for InputValidator {
    fn default() -> Self {
        Self {
            max_length: MAX_INPUT_LENGTH,
            max_lines: MAX_INPUT_LINES,
            allow_html: false,
            allow_script_tags: false,
        }
    }
}

impl InputValidator {
    /// Create new input validator with custom limits
    pub fn new(max_length: usize, max_lines: usize) -> Self {
        Self {
            max_length,
            max_lines,
            allow_html: false,
            allow_script_tags: false,
        }
    }

    /// Create permissive validator for trusted inputs
    pub fn permissive() -> Self {
        Self {
            max_length: MAX_INPUT_LENGTH * 10,
            max_lines: MAX_INPUT_LINES * 10,
            allow_html: true,
            allow_script_tags: false, // Scripts never allowed
        }
    }

    /// Validate and sanitize input string
    pub fn validate(&self, input: &str) -> Result<String> {
        // Length check
        if input.len() > self.max_length {
            return Err(LuminaError::SecurityViolation(format!(
                "Input too long: {} bytes exceeds maximum of {}",
                input.len(),
                self.max_length
            )));
        }

        // Line count check
        let line_count = input.lines().count();
        if line_count > self.max_lines {
            return Err(LuminaError::SecurityViolation(format!(
                "Too many lines: {} exceeds maximum of {}",
                line_count,
                self.max_lines
            )));
        }

        // Security pattern check
        if let Some(pattern) = self.check_security_patterns(input) {
            return Err(LuminaError::SecurityViolation(format!(
                "Blocked security pattern detected: {}",
                pattern
            )));
        }

        // HTML/script validation
        let sanitized = self.sanitize_content(input)?;

        Ok(sanitized)
    }

    /// Check for known security patterns
    fn check_security_patterns(&self, input: &str) -> Option<&'static str> {
        let patterns = SECURITY_PATTERNS.get_or_init(|| {
            vec![
                // SQL injection patterns
                "'; DROP TABLE",
                "' OR 1=1",
                "UNION SELECT",

                // Command injection patterns
                "; rm -rf",
                "&& rm -rf",
                "| rm -rf",
                "; cat /etc/passwd",

                // Script injection patterns (case insensitive check below)
                "<script>",
                "javascript:",
                "data:text/html",
                "vbscript:",

                // Path traversal
                "../../../",
                "..\\..\\",

                // Network addresses (should use vault)
                "192.168.",
                "10.",
                "172.16.",
                "172.17.",
                "172.18.",
                "172.19.",
                "172.20.",
                "172.21.",
                "172.22.",
                "172.23.",
                "172.24.",
                "172.25.",
                "172.26.",
                "172.27.",
                "172.28.",
                "172.29.",
                "172.30.",
                "172.31.",

                // Protocol handlers
                "file://",
                "ftp://",
                "ldap://",
                "gopher://",
            ]
        });

        let input_lower = input.to_lowercase();

        for pattern in patterns.iter() {
            if input_lower.contains(&pattern.to_lowercase()) {
                return Some(pattern);
            }
        }

        None
    }

    /// Sanitize content based on validation rules
    fn sanitize_content(&self, input: &str) -> Result<String> {
        let mut sanitized = input.to_string();

        if !self.allow_html {
            // Basic HTML entity encoding
            sanitized = sanitized
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;")
                .replace('\'', "&#x27;");
        }

        if !self.allow_script_tags {
            // Remove script tags even if HTML is allowed
            sanitized = self.remove_script_tags(&sanitized);
        }

        // Normalize whitespace and remove null bytes
        sanitized = sanitized
            .replace('\0', "")
            .replace('\r', "\n")
            .lines()
            .map(|line| line.trim_end())
            .collect::<Vec<_>>()
            .join("\n");

        Ok(sanitized)
    }

    /// Remove script tags and their content
    fn remove_script_tags(&self, input: &str) -> String {
        let mut result = String::new();
        let mut in_script = false;
        let mut i = 0;
        let chars: Vec<char> = input.chars().collect();

        while i < chars.len() {
            if !in_script {
                // Check for opening script tag
                if i + 7 < chars.len() {
                    let slice: String = chars[i..i+7].iter().collect();
                    if slice.to_lowercase() == "<script" {
                        in_script = true;
                        i += 7;
                        continue;
                    }
                }
                result.push(chars[i]);
            } else {
                // Look for closing script tag
                if i + 9 < chars.len() {
                    let slice: String = chars[i..i+9].iter().collect();
                    if slice.to_lowercase() == "</script>" {
                        in_script = false;
                        i += 9;
                        continue;
                    }
                }
            }
            i += 1;
        }

        result
    }

    /// Validate specific input types
    pub fn validate_url(&self, url: &str) -> Result<String> {
        let validated = self.validate(url)?;

        // Additional URL validation
        if !validated.starts_with("http://") && !validated.starts_with("https://") {
            return Err(LuminaError::SecurityViolation(
                "URL must use HTTP or HTTPS protocol".to_string()
            ));
        }

        // Check for private/local addresses
        if self.contains_private_address(&validated) {
            return Err(LuminaError::SecurityViolation(
                "URLs with private IP addresses are not allowed".to_string()
            ));
        }

        Ok(validated)
    }

    /// Validate JSON input
    pub fn validate_json(&self, json_str: &str) -> Result<String> {
        // First validate JSON structure before applying other validations
        serde_json::from_str::<serde_json::Value>(json_str)
            .map_err(|e| LuminaError::SecurityViolation(format!("Invalid JSON: {}", e)))?;

        // Then apply other input validations (but skip HTML encoding for JSON)
        // Create temporary validator that allows quotes and brackets
        let temp_validator = InputValidator {
            max_length: self.max_length,
            max_lines: self.max_lines,
            allow_html: true, // Allow JSON syntax
            allow_script_tags: false,
        };

        let validated = temp_validator.validate(json_str)?;
        Ok(validated)
    }

    /// Check if input contains private IP addresses
    fn contains_private_address(&self, input: &str) -> bool {
        // Simple regex patterns for private addresses
        let private_patterns = [
            "192.168.",
            "10.",
            "172.16.", "172.17.", "172.18.", "172.19.",
            "172.20.", "172.21.", "172.22.", "172.23.",
            "172.24.", "172.25.", "172.26.", "172.27.",
            "172.28.", "172.29.", "172.30.", "172.31.",
            "127.",
            "localhost",
        ];

        let input_lower = input.to_lowercase();
        private_patterns.iter().any(|pattern| input_lower.contains(pattern))
    }
}

/// Global input validator instance
pub fn global_validator() -> &'static InputValidator {
    static VALIDATOR: OnceLock<InputValidator> = OnceLock::new();
    VALIDATOR.get_or_init(InputValidator::default)
}

/// Validate input using global validator
pub fn validate_input(input: &str) -> Result<String> {
    global_validator().validate(input)
}

/// Validate URL using global validator
pub fn validate_url(url: &str) -> Result<String> {
    global_validator().validate_url(url)
}

/// Validate JSON using global validator
pub fn validate_json(json: &str) -> Result<String> {
    global_validator().validate_json(json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_validation() {
        let validator = InputValidator::default();
        let result = validator.validate("Hello, world!").unwrap();
        assert_eq!(result, "Hello, world!");
    }

    #[test]
    fn test_length_limit() {
        let validator = InputValidator::new(10, 100);
        let long_input = "a".repeat(11);
        let result = validator.validate(&long_input);
        assert!(result.is_err());
    }

    #[test]
    fn test_line_limit() {
        let validator = InputValidator::new(1000, 5);
        let many_lines = "line\n".repeat(6);
        let result = validator.validate(&many_lines);
        assert!(result.is_err());
    }

    #[test]
    fn test_sql_injection_detection() {
        let validator = InputValidator::default();
        let malicious = "'; DROP TABLE users; --";
        let result = validator.validate(malicious);
        assert!(result.is_err());
    }

    #[test]
    fn test_script_tag_removal() {
        // Test the script removal function directly
        let validator = InputValidator::default();
        let input = "Hello <script>alert('xss')</script> world";
        let result = validator.remove_script_tags(input);
        assert!(!result.contains("script"));
        assert!(!result.contains("alert"));
        assert!(result.contains("Hello"));
        assert!(result.contains("world"));
    }

    #[test]
    fn test_html_encoding() {
        let validator = InputValidator::default();
        let input = "Test <b>bold</b> & \"quoted\" text";
        let result = validator.validate(input).unwrap();
        assert!(result.contains("&lt;b&gt;"));
        assert!(result.contains("&amp;"));
        assert!(result.contains("&quot;"));
    }

    #[test]
    fn test_url_validation() {
        let validator = InputValidator::default();

        // Valid URL
        let valid = validator.validate_url("https://example.com").unwrap();
        assert_eq!(valid, "https://example.com");

        // Invalid protocol
        let invalid = validator.validate_url("ftp://example.com");
        assert!(invalid.is_err());

        // Private IP
        let private = validator.validate_url("http://192.168.1.1"); // fake IP fixture (synthetic, not real infrastructure)
        assert!(private.is_err());
    }

    #[test]
    fn test_json_validation() {
        let validator = InputValidator::default();

        // Valid JSON
        let valid = validator.validate_json(r#"{"key": "value"}"#).unwrap();
        assert!(valid.contains("key")); // JSON preserves structure

        // Invalid JSON
        let invalid = validator.validate_json("not json");
        assert!(invalid.is_err());
    }

    #[test]
    fn test_private_ip_detection() {
        let validator = InputValidator::default();

        let private_ips = vec![
            "192.168.1.1", // fake IP fixture (synthetic, not real infrastructure)
            "10.0.0.1", // fake IP fixture (synthetic, not real infrastructure)
            "172.16.0.1", // fake IP fixture (synthetic, not real infrastructure)
            "127.0.0.1",
            "localhost",
        ];

        for ip in private_ips {
            assert!(validator.contains_private_address(ip));
        }

        assert!(!validator.contains_private_address("8.8.8.8"));
    }

    #[test]
    fn test_permissive_validator() {
        let validator = InputValidator::permissive();
        let html_input = "<p>This is <b>bold</b> text</p>";
        let result = validator.validate(html_input).unwrap();
        assert!(result.contains("<p>"));
        assert!(result.contains("<b>"));
    }

    #[test]
    fn test_global_validator() {
        let result = validate_input("test input").unwrap();
        assert_eq!(result, "test input");

        let url_result = validate_url("https://example.com").unwrap();
        assert_eq!(url_result, "https://example.com");
    }
}