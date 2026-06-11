//! GUARD-04: Output filtering and sensitive data protection
//!
//! Prevents accidental leakage of sensitive information in agent outputs,
//! logs, error messages, and other system responses.

use std::sync::OnceLock;

/// Patterns that indicate sensitive data
static SENSITIVE_PATTERNS: OnceLock<Vec<SensitivePattern>> = OnceLock::new();

/// Configuration for output filtering
pub struct OutputFilter {
    redact_secrets: bool,
    redact_tokens: bool,
    redact_keys: bool,
    redact_passwords: bool,
    redact_urls: bool,
    replacement_text: String,
}

/// A sensitive pattern with its detection regex and description
struct SensitivePattern {
    name: &'static str,
    pattern: &'static str,
    case_sensitive: bool,
}

impl Default for OutputFilter {
    fn default() -> Self {
        Self {
            redact_secrets: true,
            redact_tokens: true,
            redact_keys: true,
            redact_passwords: true,
            redact_urls: true,
            replacement_text: "[REDACTED]".to_string(),
        }
    }
}

impl OutputFilter {
    /// Create new output filter with custom settings
    pub fn new() -> Self {
        Self::default()
    }

    /// Create permissive filter for debugging (redacts less)
    pub fn permissive() -> Self {
        Self {
            redact_secrets: true,
            redact_tokens: true,
            redact_keys: true,
            redact_passwords: false, // Allow for debugging
            redact_urls: false, // Allow for debugging
            replacement_text: "[REDACTED]".to_string(),
        }
    }

    /// Create strict filter that redacts everything
    pub fn strict() -> Self {
        Self {
            redact_secrets: true,
            redact_tokens: true,
            redact_keys: true,
            redact_passwords: true,
            redact_urls: true,
            replacement_text: "[CLASSIFIED]".to_string(),
        }
    }

    /// Filter output text and redact sensitive information
    pub fn filter(&self, text: &str) -> String {
        let patterns = SENSITIVE_PATTERNS.get_or_init(|| {
            vec![
                // API keys and tokens
                SensitivePattern {
                    name: "API_KEY",
                    pattern: "api_key_pattern",
                    case_sensitive: false,
                },

                // JWT tokens
                SensitivePattern {
                    name: "JWT_TOKEN",
                    pattern: "jwt_pattern",
                    case_sensitive: true,
                },

                // Base64 encoded secrets (common in config)
                SensitivePattern {
                    name: "BASE64_SECRET",
                    pattern: "base64_secret_pattern",
                    case_sensitive: false,
                },

                // SSH private keys
                SensitivePattern {
                    name: "SSH_PRIVATE_KEY",
                    pattern: "ssh_key_pattern",
                    case_sensitive: true,
                },

                // AWS credentials
                SensitivePattern {
                    name: "AWS_ACCESS_KEY",
                    pattern: "aws_key_pattern",
                    case_sensitive: true,
                },

                // Passwords in URLs
                SensitivePattern {
                    name: "PASSWORD_URL",
                    pattern: "password_url_pattern",
                    case_sensitive: false,
                },

                // Credit card numbers (basic pattern)
                SensitivePattern {
                    name: "CREDIT_CARD",
                    pattern: "credit_card_pattern",
                    case_sensitive: false,
                },

                // Environment variable secrets
                SensitivePattern {
                    name: "ENV_SECRET",
                    pattern: "env_secret_pattern",
                    case_sensitive: false,
                },

                // Private IP addresses in outputs (should not be exposed)
                SensitivePattern {
                    name: "PRIVATE_IP",
                    pattern: "private_ip_pattern",
                    case_sensitive: false,
                },
            ]
        });

        let mut filtered = text.to_string();

        for pattern in patterns.iter() {
            if self.should_redact_pattern(pattern) {
                filtered = self.redact_pattern(&filtered, pattern);
            }
        }

        // Additional specific redactions
        if self.redact_secrets {
            filtered = self.redact_common_secrets(&filtered);
        }

        if self.redact_passwords {
            filtered = self.redact_password_fields(&filtered);
        }

        if self.redact_urls {
            filtered = self.redact_sensitive_urls(&filtered);
        }

        filtered
    }

    /// Check if a pattern should be redacted based on filter configuration
    fn should_redact_pattern(&self, pattern: &SensitivePattern) -> bool {
        match pattern.name {
            "API_KEY" | "JWT_TOKEN" | "BASE64_SECRET" | "AWS_ACCESS_KEY" | "ENV_SECRET" => self.redact_tokens,
            "SSH_PRIVATE_KEY" => self.redact_keys,
            "PASSWORD_URL" => self.redact_passwords,
            "CREDIT_CARD" => true, // Always redact credit cards
            "PRIVATE_IP" => self.redact_urls,
            _ => true,
        }
    }

    /// Redact a specific pattern from text
    fn redact_pattern(&self, text: &str, pattern: &SensitivePattern) -> String {
        // Simple pattern matching - in a real implementation would use regex
        // For now, do basic string matching for key patterns
        match pattern.name {
            "JWT_TOKEN" => self.redact_jwt_tokens(text),
            "SSH_PRIVATE_KEY" => self.redact_ssh_keys(text),
            "PRIVATE_IP" => self.redact_private_ips(text),
            _ => self.redact_key_value_pairs(text, pattern),
        }
    }

    /// Redact JWT tokens
    fn redact_jwt_tokens(&self, text: &str) -> String {
        // Simple JWT pattern: eyJ...
        let mut result = text.to_string();
        let words: Vec<&str> = text.split_whitespace().collect();

        for word in words {
            if word.starts_with("eyJ") && word.contains('.') {
                let parts: Vec<&str> = word.split('.').collect();
                if parts.len() >= 3 {
                    result = result.replace(word, &self.replacement_text);
                }
            }
        }

        result
    }

    /// Redact SSH private keys
    fn redact_ssh_keys(&self, text: &str) -> String {
        let mut result = text.to_string();

        if text.contains("BEGIN") && text.contains("PRIVATE KEY") {
            // Find key blocks and redact them
            let lines: Vec<&str> = text.lines().collect();
            let mut in_key = false;
            let mut key_lines = Vec::new();

            for line in lines {
                if line.contains("BEGIN") && line.contains("PRIVATE KEY") {
                    in_key = true;
                    key_lines.push(line);
                } else if line.contains("END") && line.contains("PRIVATE KEY") {
                    in_key = false;
                    key_lines.push(line);

                    // Replace the entire key block
                    let key_block = key_lines.join("\n");
                    result = result.replace(&key_block, &self.replacement_text);
                    key_lines.clear();
                } else if in_key {
                    key_lines.push(line);
                }
            }
        }

        result
    }

    /// Redact private IP addresses
    fn redact_private_ips(&self, text: &str) -> String {
        let mut result = text.to_string();
        let words: Vec<&str> = text.split_whitespace().collect();

        for word in words {
            if self.is_private_ip(word) {
                result = result.replace(word, &format!("[IP-{}]", "REDACTED"));
            }
        }

        result
    }

    /// Check if a string looks like a private IP
    fn is_private_ip(&self, s: &str) -> bool {
        // Remove common punctuation
        let clean = s.trim_end_matches(|c: char| ".,;:!?".contains(c));

        clean.starts_with("192.168.") ||
        clean.starts_with("10.") ||
        clean.starts_with("172.16.") ||
        clean.starts_with("172.17.") ||
        clean.starts_with("172.18.") ||
        clean.starts_with("172.19.") ||
        clean.starts_with("172.20.") ||
        clean.starts_with("172.21.") ||
        clean.starts_with("172.22.") ||
        clean.starts_with("172.23.") ||
        clean.starts_with("172.24.") ||
        clean.starts_with("172.25.") ||
        clean.starts_with("172.26.") ||
        clean.starts_with("172.27.") ||
        clean.starts_with("172.28.") ||
        clean.starts_with("172.29.") ||
        clean.starts_with("172.30.") ||
        clean.starts_with("172.31.") ||
        clean.starts_with("127.")
    }

    /// Redact key-value pairs that might contain secrets
    fn redact_key_value_pairs(&self, text: &str, _pattern: &SensitivePattern) -> String {
        let mut result = text.to_string();

        // Look for patterns like "key=value" or "key: value"
        for line in text.lines() {
            let lower_line = line.to_lowercase();

            // Check if line contains sensitive keys
            for key_word in ["secret", "password", "key", "token", "api", "auth"] {
                if lower_line.contains(key_word) {
                    // Look for assignment patterns
                    if let Some(eq_pos) = line.find('=') {
                        let before = &line[..eq_pos];
                        let after = &line[eq_pos + 1..];

                        if before.to_lowercase().contains(key_word) {
                            let clean_after = after.trim().trim_matches('"').trim_matches('\'');
                            if clean_after.len() > 4 { // Only redact substantial values
                                let redacted_line = format!("{}={}", before, &self.replacement_text);
                                result = result.replace(line, &redacted_line);
                            }
                        }
                    } else if let Some(colon_pos) = line.find(':') {
                        let before = &line[..colon_pos];
                        let after = &line[colon_pos + 1..];

                        if before.to_lowercase().contains(key_word) {
                            let clean_after = after.trim().trim_matches('"').trim_matches('\'');
                            if clean_after.len() > 4 {
                                let redacted_line = format!("{}: {}", before, &self.replacement_text);
                                result = result.replace(line, &redacted_line);
                            }
                        }
                    }
                }
            }
        }

        result
    }

    /// Redact common secret formats
    fn redact_common_secrets(&self, text: &str) -> String {
        let mut result = text.to_string();

        // Redact long alphanumeric strings that look like secrets
        let words: Vec<&str> = text.split_whitespace().collect();

        for word in words {
            if self.looks_like_secret(word) {
                result = result.replace(word, &self.replacement_text);
            }
        }

        result
    }

    /// Check if a word looks like a secret
    fn looks_like_secret(&self, word: &str) -> bool {
        // Remove quotes and punctuation
        let clean = word.trim_matches(|c: char| "\"'.,;:!?()[]{}".contains(c));

        // Must be reasonably long
        if clean.len() < 16 {
            return false;
        }

        // Must be mostly alphanumeric
        let alphanumeric_count = clean.chars().filter(|c| c.is_alphanumeric()).count();
        if (alphanumeric_count as f64) / (clean.len() as f64) < 0.8 {
            return false;
        }

        // Must have both letters and numbers (typical of generated secrets)
        let has_letters = clean.chars().any(|c| c.is_alphabetic());
        let has_numbers = clean.chars().any(|c| c.is_numeric());

        has_letters && has_numbers
    }

    /// Redact password fields specifically
    fn redact_password_fields(&self, text: &str) -> String {
        // Process line-by-line so each line is independent.  This naturally
        // handles both (a) multiple occurrences on different lines and (b)
        // the last line having no trailing newline, without any offset
        // arithmetic or loop-convergence concerns.
        let lines: Vec<String> = text
            .split('\n')
            .map(|line| {
                let lower = line.to_lowercase();
                for pattern in [
                    "password=",
                    "pwd=",
                    "pass=",
                    "password:",
                    "pwd:",
                    "pass:",
                ] {
                    if let Some(start) = lower.find(pattern) {
                        let after_pattern = start + pattern.len();
                        let value = line[after_pattern..].trim();
                        if !value.is_empty() {
                            // Replace from after_pattern to end-of-line.
                            return format!("{}{}", &line[..after_pattern], &self.replacement_text);
                        }
                    }
                }
                line.to_string()
            })
            .collect();
        lines.join("\n")
    }

    /// Redact sensitive URLs
    fn redact_sensitive_urls(&self, text: &str) -> String {
        let mut result = text.to_string();

        // Find URLs with credentials
        for word in text.split_whitespace() {
            if word.starts_with("http") && word.contains("://") && word.contains('@') {
                // URL has credentials, redact the password part
                if let Some(at_pos) = word.find('@') {
                    if let Some(proto_end) = word.find("://") {
                        let proto_part = &word[..proto_end + 3];
                        let cred_part = &word[proto_end + 3..at_pos];
                        let host_part = &word[at_pos..];

                        if let Some(colon_pos) = cred_part.find(':') {
                            let user_part = &cred_part[..colon_pos];
                            let redacted_url = format!("{}{}:[REDACTED]{}", proto_part, user_part, host_part);
                            result = result.replace(word, &redacted_url);
                        }
                    }
                }
            }
        }

        result
    }

    /// Set custom replacement text
    pub fn with_replacement(mut self, replacement: &str) -> Self {
        self.replacement_text = replacement.to_string();
        self
    }

    /// Enable/disable specific redaction types
    pub fn with_secrets(mut self, enable: bool) -> Self {
        self.redact_secrets = enable;
        self
    }

    pub fn with_tokens(mut self, enable: bool) -> Self {
        self.redact_tokens = enable;
        self
    }

    pub fn with_keys(mut self, enable: bool) -> Self {
        self.redact_keys = enable;
        self
    }

    pub fn with_passwords(mut self, enable: bool) -> Self {
        self.redact_passwords = enable;
        self
    }

    pub fn with_urls(mut self, enable: bool) -> Self {
        self.redact_urls = enable;
        self
    }
}

/// Global output filter instance
pub fn global_filter() -> &'static OutputFilter {
    static FILTER: OnceLock<OutputFilter> = OnceLock::new();
    FILTER.get_or_init(OutputFilter::default)
}

/// Filter output using global filter.
///
/// Returns a ZeroizingString so the filtered response is wiped from heap
/// after it has been delivered to the channel.
pub fn filter_output(text: &str) -> crate::secure_string::ZeroizingString {
    crate::secure_string::ZeroizingString::new(global_filter().filter(text))
}

/// Filter sensitive data for logging (stricter)
pub fn filter_for_logs(text: &str) -> String {
    OutputFilter::strict().filter(text)
}

/// Filter for debug output (more permissive)
pub fn filter_for_debug(text: &str) -> String {
    OutputFilter::permissive().filter(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_filtering() {
        let filter = OutputFilter::default();
        let result = filter.filter("Hello world");
        assert_eq!(result, "Hello world");
    }

    #[test]
    fn test_jwt_token_redaction() {
        let filter = OutputFilter::default();
        let input = "Token: eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        let result = filter.filter(input);
        assert!(result.contains("[REDACTED]"));
        assert!(!result.contains("eyJ"));
    }

    #[test]
    fn test_ssh_key_redaction() {
        let filter = OutputFilter::default();
        let input = "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA7v2Z9v2Z9v2Z9v2Z9v2Z9v2Z9v2Z\n-----END RSA PRIVATE KEY-----";
        let result = filter.filter(input);
        assert!(result.contains("[REDACTED]"));
        assert!(!result.contains("MIIEpA"));
    }

    #[test]
    fn test_private_ip_redaction() {
        let filter = OutputFilter::default();
        let input = "Connect to 192.168.1.100 and 10.0.0.1 for testing";
        let result = filter.filter(input);
        assert!(result.contains("[IP-REDACTED]"));
        assert!(!result.contains("192.168.1.100"));
    }

    #[test]
    fn test_password_redaction() {
        let filter = OutputFilter::default();
        let input = "password=mysecretpass123\napi_key=abcdef123456789";
        let result = filter.filter(input);
        assert!(result.contains("[REDACTED]"));
        assert!(!result.contains("mysecretpass123"));
    }

    /// Regression: single-line output with no trailing newline must be redacted.
    /// Previously `find('\n')` returned None and the value was left in plain text.
    #[test]
    fn test_password_redaction_last_line_no_newline() {
        let filter = OutputFilter::default();
        let input = "password: hunter2";
        let result = filter.redact_password_fields(input);
        assert!(!result.contains("hunter2"), "last-line secret must be redacted; got: {}", result);
        assert!(result.contains("[REDACTED]"));
    }

    /// Regression: multiple occurrences of a password pattern on different
    /// lines must ALL be redacted (previously only the first match was found).
    #[test]
    fn test_password_redaction_multiple_occurrences() {
        let filter = OutputFilter::default();
        let input = "password=secret1\npassword=secret2\npassword=secret3";
        let result = filter.redact_password_fields(input);
        assert!(!result.contains("secret1"), "first secret must be redacted; got: {}", result);
        assert!(!result.contains("secret2"), "second secret must be redacted; got: {}", result);
        assert!(!result.contains("secret3"), "third secret must be redacted; got: {}", result);
    }

    #[test]
    fn test_url_credentials_redaction() {
        let filter = OutputFilter::default();
        let input = "Database URL: https://user:secretpass@db.example.com:5432/mydb";
        let result = filter.filter(input);
        println!("Input: {}", input);
        println!("Result: {}", result);
        assert!(!result.contains("secretpass")); // Main goal: hide password
    }

    #[test]
    fn test_secret_detection() {
        let filter = OutputFilter::default();

        // Should detect as secret (long, alphanumeric mix)
        assert!(filter.looks_like_secret("abc123def456ghi789"));

        // Should not detect as secret (too short)
        assert!(!filter.looks_like_secret("abc123"));

        // Should not detect as secret (no numbers)
        assert!(!filter.looks_like_secret("abcdefghijklmnop"));

        // Should not detect as secret (no letters)
        assert!(!filter.looks_like_secret("123456789012345"));
    }

    #[test]
    fn test_custom_replacement() {
        let filter = OutputFilter::default().with_replacement("***HIDDEN***");
        let input = "password=secret123";
        let result = filter.filter(input);
        assert!(result.contains("***HIDDEN***"));
        assert!(!result.contains("[REDACTED]"));
    }

    #[test]
    fn test_permissive_vs_strict() {
        let permissive = OutputFilter::permissive();
        let strict = OutputFilter::strict();
        let input = "password=test123 and url with private IP 192.0.2.11";

        let perm_result = permissive.filter(input);
        let strict_result = strict.filter(input);

        println!("Input: {}", input);
        println!("Permissive result: {}", perm_result);
        println!("Strict result: {}", strict_result);

        // Both should redact IPs
        assert!(perm_result.contains("[IP-REDACTED]") || !perm_result.contains("192.0.2.11"));
        assert!(strict_result.contains("[IP-REDACTED]") || !strict_result.contains("192.0.2.11"));

        // Strict should be more aggressive
        assert_ne!(perm_result, strict_result);
    }

    #[test]
    fn test_global_filter() {
        let result = filter_output("test eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.test.signature");
        assert!(result.contains("[REDACTED]"));
    }

    #[test]
    fn test_log_filtering() {
        let input = "Error connecting to https://user:pass@db.example.com";
        let result = filter_for_logs(input);
        println!("Input: {}", input);
        println!("Result: {}", result);
        assert!(!result.contains("pass")); // Make sure password is redacted
    }

    #[test]
    fn test_debug_filtering() {
        let input = "Debug info: password=debugpass";
        let result = filter_for_debug(input);
        // Permissive filter might keep some debug info while redacting secrets
        assert!(!result.contains("debugpass")); // But still redacts obvious secrets
    }
}