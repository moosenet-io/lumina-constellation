//! GUARD-07: Audit logging and security event tracking
//!
//! Provides comprehensive audit logging for security-related events,
//! user actions, and system access for compliance and investigation.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;
use serde::{Deserialize, Serialize};

/// Audit event severity levels
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AuditSeverity {
    Info,
    Warning,
    Error,
    Critical,
}

impl std::fmt::Display for AuditSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuditSeverity::Info => write!(f, "INFO"),
            AuditSeverity::Warning => write!(f, "WARN"),
            AuditSeverity::Error => write!(f, "ERROR"),
            AuditSeverity::Critical => write!(f, "CRITICAL"),
        }
    }
}

/// Audit event categories
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AuditCategory {
    Authentication,
    Authorization,
    DataAccess,
    Configuration,
    Security,
    System,
    UserAction,
    NetworkAccess,
    Custom(String),
}

impl std::fmt::Display for AuditCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuditCategory::Authentication => write!(f, "AUTH"),
            AuditCategory::Authorization => write!(f, "AUTHZ"),
            AuditCategory::DataAccess => write!(f, "DATA"),
            AuditCategory::Configuration => write!(f, "CONFIG"),
            AuditCategory::Security => write!(f, "SECURITY"),
            AuditCategory::System => write!(f, "SYSTEM"),
            AuditCategory::UserAction => write!(f, "USER"),
            AuditCategory::NetworkAccess => write!(f, "NETWORK"),
            AuditCategory::Custom(name) => write!(f, "CUSTOM:{}", name),
        }
    }
}

/// Audit event record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    /// Unique event identifier
    pub id: String,
    /// Timestamp of the event
    pub timestamp: SystemTime,
    /// Event severity level
    pub severity: AuditSeverity,
    /// Event category
    pub category: AuditCategory,
    /// Event action/operation
    pub action: String,
    /// User ID (if applicable)
    pub user_id: Option<String>,
    /// Session ID (if applicable)
    pub session_id: Option<String>,
    /// Source IP address
    pub source_ip: Option<String>,
    /// User agent string
    pub user_agent: Option<String>,
    /// Resource being accessed/modified
    pub resource: Option<String>,
    /// Event outcome (success/failure)
    pub success: bool,
    /// Error message (if failed)
    pub error_message: Option<String>,
    /// Additional event data
    pub metadata: HashMap<String, String>,
}

impl AuditEvent {
    /// Create a new audit event
    pub fn new(severity: AuditSeverity, category: AuditCategory, action: String) -> Self {
        Self {
            id: generate_event_id(),
            timestamp: SystemTime::now(),
            severity,
            category,
            action,
            user_id: None,
            session_id: None,
            source_ip: None,
            user_agent: None,
            resource: None,
            success: true,
            error_message: None,
            metadata: HashMap::new(),
        }
    }

    /// Create a success event
    pub fn success(category: AuditCategory, action: String) -> Self {
        Self::new(AuditSeverity::Info, category, action)
    }

    /// Create a failure event
    pub fn failure(category: AuditCategory, action: String, error: String) -> Self {
        let mut event = Self::new(AuditSeverity::Warning, category, action);
        event.success = false;
        event.error_message = Some(error);
        event
    }

    /// Create a critical security event
    pub fn security_critical(action: String) -> Self {
        Self::new(AuditSeverity::Critical, AuditCategory::Security, action)
    }

    /// Builder pattern methods
    pub fn with_user(mut self, user_id: String) -> Self {
        self.user_id = Some(user_id);
        self
    }

    pub fn with_session(mut self, session_id: String) -> Self {
        self.session_id = Some(session_id);
        self
    }

    pub fn with_source_ip(mut self, ip: String) -> Self {
        self.source_ip = Some(ip);
        self
    }

    pub fn with_user_agent(mut self, user_agent: String) -> Self {
        self.user_agent = Some(user_agent);
        self
    }

    pub fn with_resource(mut self, resource: String) -> Self {
        self.resource = Some(resource);
        self
    }

    pub fn with_metadata(mut self, key: String, value: String) -> Self {
        self.metadata.insert(key, value);
        self
    }

    pub fn with_severity(mut self, severity: AuditSeverity) -> Self {
        self.severity = severity;
        self
    }

    pub fn with_error(mut self, error: String) -> Self {
        self.success = false;
        self.error_message = Some(error);
        if self.severity == AuditSeverity::Info {
            self.severity = AuditSeverity::Warning;
        }
        self
    }

    /// Format event for logging
    pub fn to_log_line(&self) -> String {
        let timestamp = self.timestamp
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        format!(
            "[{}] {} {} {} | User: {} | Session: {} | Source: {} | Resource: {} | Success: {} | {}{}",
            timestamp,
            self.severity,
            self.category,
            self.action,
            self.user_id.as_deref().unwrap_or("N/A"),
            self.session_id.as_deref().unwrap_or("N/A"),
            self.source_ip.as_deref().unwrap_or("N/A"),
            self.resource.as_deref().unwrap_or("N/A"),
            self.success,
            if let Some(err) = &self.error_message {
                format!("Error: {} | ", err)
            } else {
                String::new()
            },
            if self.metadata.is_empty() {
                String::new()
            } else {
                format!("Meta: {:?}", self.metadata)
            }
        )
    }
}

/// Audit logger configuration
#[derive(Debug, Clone)]
pub struct AuditConfig {
    /// Whether to enable audit logging
    pub enabled: bool,
    /// Maximum number of events to keep in memory
    pub max_memory_events: usize,
    /// Whether to log to file
    pub log_to_file: bool,
    /// Log file path
    pub log_file_path: Option<String>,
    /// Whether to log to console
    pub log_to_console: bool,
    /// Minimum severity level to log
    pub min_severity: AuditSeverity,
    /// Whether to include sensitive data in logs
    pub include_sensitive_data: bool,
    /// HARDEN-04: rotate when log file exceeds this many bytes (default 10 MB)
    pub max_log_size_bytes: u64,
    /// HARDEN-04: number of rotated .gz files to keep (default 5)
    pub max_rotated_files: u32,
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_memory_events: 10_000,
            log_to_file: false,
            log_file_path: None,
            log_to_console: true,
            min_severity: AuditSeverity::Info,
            include_sensitive_data: false,
            max_log_size_bytes: 10 * 1024 * 1024, // 10 MB
            max_rotated_files: 5,
        }
    }
}

impl AuditConfig {
    /// Production configuration - secure and comprehensive
    pub fn production() -> Self {
        Self {
            enabled: true,
            max_memory_events: 50_000,
            log_to_file: true,
            log_file_path: Some("/var/log/lumina/audit.log".to_string()),
            log_to_console: false,
            min_severity: AuditSeverity::Info,
            include_sensitive_data: false,
            max_log_size_bytes: 10 * 1024 * 1024,
            max_rotated_files: 5,
        }
    }

    /// Development configuration - verbose and console-friendly
    pub fn development() -> Self {
        Self {
            enabled: true,
            max_memory_events: 5_000,
            log_to_file: false,
            log_file_path: None,
            log_to_console: true,
            min_severity: AuditSeverity::Info,
            include_sensitive_data: true,
            max_log_size_bytes: 1024 * 1024, // 1 MB for dev
            max_rotated_files: 2,
        }
    }

    /// Security-focused configuration
    pub fn security() -> Self {
        Self {
            enabled: true,
            max_memory_events: 100_000,
            log_to_file: true,
            log_file_path: Some("/var/log/lumina/security-audit.log".to_string()),
            log_to_console: true,
            min_severity: AuditSeverity::Warning,
            include_sensitive_data: false,
            max_log_size_bytes: 10 * 1024 * 1024,
            max_rotated_files: 5,
        }
    }
}

/// Audit logger implementation
pub struct AuditLogger {
    config: AuditConfig,
    events: Arc<Mutex<Vec<AuditEvent>>>,
}

impl AuditLogger {
    /// Create new audit logger with default configuration
    pub fn new() -> Self {
        Self::with_config(AuditConfig::default())
    }

    /// Create audit logger with custom configuration
    pub fn with_config(config: AuditConfig) -> Self {
        Self {
            config,
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Log an audit event
    pub fn log(&self, event: AuditEvent) {
        if !self.config.enabled {
            return;
        }

        // Check minimum severity
        if !self.should_log_severity(&event.severity) {
            return;
        }

        // Filter sensitive data if needed
        let filtered_event = if self.config.include_sensitive_data {
            event
        } else {
            self.filter_sensitive_data(event)
        };

        // Store in memory
        self.store_event(filtered_event.clone());

        // Output to configured destinations
        if self.config.log_to_console {
            self.log_to_console(&filtered_event);
        }

        if self.config.log_to_file {
            self.log_to_file(&filtered_event);
        }
    }

    /// Log authentication event
    pub fn log_auth(&self, action: &str, user_id: Option<String>, session_id: Option<String>, success: bool) {
        let mut event = AuditEvent::new(
            if success { AuditSeverity::Info } else { AuditSeverity::Warning },
            AuditCategory::Authentication,
            action.to_string(),
        );

        if let Some(uid) = user_id {
            event = event.with_user(uid);
        }

        if let Some(sid) = session_id {
            event = event.with_session(sid);
        }

        event.success = success;

        self.log(event);
    }

    /// Log authorization failure
    pub fn log_authz_failure(&self, action: &str, user_id: String, resource: String, reason: String) {
        let event = AuditEvent::failure(AuditCategory::Authorization, action.to_string(), reason)
            .with_user(user_id)
            .with_resource(resource)
            .with_severity(AuditSeverity::Warning);

        self.log(event);
    }

    /// Log data access
    pub fn log_data_access(&self, action: &str, resource: &str, user_id: Option<String>) {
        let mut event = AuditEvent::success(AuditCategory::DataAccess, action.to_string())
            .with_resource(resource.to_string());

        if let Some(uid) = user_id {
            event = event.with_user(uid);
        }

        self.log(event);
    }

    /// Log security violation
    pub fn log_security_violation(&self, violation_type: &str, details: String) {
        let event = AuditEvent::security_critical(violation_type.to_string())
            .with_metadata("details".to_string(), details);

        self.log(event);
    }

    /// Log configuration change
    pub fn log_config_change(&self, setting: &str, old_value: Option<String>, new_value: String, user_id: String) {
        let mut event = AuditEvent::success(AuditCategory::Configuration, "config_change".to_string())
            .with_user(user_id)
            .with_resource(setting.to_string())
            .with_metadata("new_value".to_string(), new_value);

        if let Some(old_val) = old_value {
            event = event.with_metadata("old_value".to_string(), old_val);
        }

        self.log(event);
    }

    /// Get recent events
    pub fn get_events(&self, limit: Option<usize>) -> Vec<AuditEvent> {
        let events = self.events.lock().unwrap();
        let limit = limit.unwrap_or(events.len());
        events.iter().rev().take(limit).cloned().collect()
    }

    /// Get events by category
    pub fn get_events_by_category(&self, category: AuditCategory, limit: Option<usize>) -> Vec<AuditEvent> {
        let events = self.events.lock().unwrap();
        let filtered: Vec<_> = events
            .iter()
            .rev()
            .filter(|e| e.category == category)
            .take(limit.unwrap_or(events.len()))
            .cloned()
            .collect();
        filtered
    }

    /// Get events by severity
    pub fn get_events_by_severity(&self, severity: AuditSeverity, limit: Option<usize>) -> Vec<AuditEvent> {
        let events = self.events.lock().unwrap();
        let filtered: Vec<_> = events
            .iter()
            .rev()
            .filter(|e| e.severity == severity)
            .take(limit.unwrap_or(events.len()))
            .cloned()
            .collect();
        filtered
    }

    /// Clear all stored events
    pub fn clear_events(&self) {
        let mut events = self.events.lock().unwrap();
        events.clear();
    }

    /// Get current configuration
    pub fn config(&self) -> &AuditConfig {
        &self.config
    }

    /// Check if severity should be logged
    fn should_log_severity(&self, severity: &AuditSeverity) -> bool {
        match (&self.config.min_severity, severity) {
            (AuditSeverity::Info, _) => true,
            (AuditSeverity::Warning, AuditSeverity::Info) => false,
            (AuditSeverity::Warning, _) => true,
            (AuditSeverity::Error, AuditSeverity::Info | AuditSeverity::Warning) => false,
            (AuditSeverity::Error, _) => true,
            (AuditSeverity::Critical, AuditSeverity::Critical) => true,
            (AuditSeverity::Critical, _) => false,
        }
    }

    /// Filter sensitive data from events
    fn filter_sensitive_data(&self, mut event: AuditEvent) -> AuditEvent {
        // Filter IP addresses if they're internal
        if let Some(ref ip) = event.source_ip {
            if is_private_ip(ip) {
                event.source_ip = Some("[FILTERED_PRIVATE_IP]".to_string());
            }
        }

        // Filter potentially sensitive metadata
        let sensitive_keys = ["password", "token", "secret", "key", "credential"];
        for key in sensitive_keys {
            if event.metadata.contains_key(key) {
                event.metadata.insert(key.to_string(), "[FILTERED]".to_string());
            }
        }

        event
    }

    /// Store event in memory
    fn store_event(&self, event: AuditEvent) {
        let mut events = self.events.lock().unwrap();
        events.push(event);

        // Trim events if we exceed the limit
        if events.len() > self.config.max_memory_events {
            let excess = events.len() - self.config.max_memory_events;
            events.drain(0..excess);
        }
    }

    /// Log to console
    fn log_to_console(&self, event: &AuditEvent) {
        match event.severity {
            AuditSeverity::Critical | AuditSeverity::Error => {
                eprintln!("AUDIT: {}", event.to_log_line());
            }
            _ => {
                println!("AUDIT: {}", event.to_log_line());
            }
        }
    }

    /// Append event to the log file, rotating when it exceeds the size limit.
    ///
    /// HARDEN-04: rotation shifts .1.gz → .2.gz up to max_rotated_files, then
    /// gzips the current file into .1.gz and creates a fresh empty log.
    fn log_to_file(&self, event: &AuditEvent) {
        if let Some(ref path) = self.config.log_file_path {
            let log_path = std::path::Path::new(path);

            if let Some(parent) = log_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }

            // Rotate if file exceeds size limit
            if let Ok(meta) = std::fs::metadata(log_path) {
                if meta.len() >= self.config.max_log_size_bytes {
                    rotate_log_file(log_path, self.config.max_rotated_files);
                }
            }

            // Append the new event
            use std::io::Write;
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_path)
            {
                let _ = writeln!(file, "{}", event.to_log_line());
            }
        }
    }
}

impl Default for AuditLogger {
    fn default() -> Self {
        Self::new()
    }
}

/// HARDEN-04: Rotate an audit log file.
///
/// Shifts existing rotated files up by one index (`.1.gz` → `.2.gz`, etc.),
/// gzips the current log into `.1.gz`, and creates a fresh empty log.
/// Files beyond `max_files` are deleted. Rotation is atomic within rename
/// limits — the log file always exists after this function returns.
fn rotate_log_file(log_path: &std::path::Path, max_files: u32) {
    use flate2::{write::GzEncoder, Compression};
    use std::io::Write;

    let base = log_path.to_string_lossy().into_owned();

    // Shift existing rotated files: N-1 → N, down from the highest
    for i in (1..max_files).rev() {
        let src = format!("{}.{}.gz", base, i);
        let dst = format!("{}.{}.gz", base, i + 1);
        if std::path::Path::new(&src).exists() {
            if i + 1 > max_files {
                let _ = std::fs::remove_file(&src);
            } else {
                let _ = std::fs::rename(&src, &dst);
            }
        }
    }

    // Delete slot N+1 if it somehow exists (cleanup)
    let overflow = format!("{}.{}.gz", base, max_files + 1);
    let _ = std::fs::remove_file(&overflow);

    // Compress current log → .1.gz
    let gz_path = format!("{}.1.gz", base);
    if let Ok(content) = std::fs::read(log_path) {
        if let Ok(gz_file) = std::fs::File::create(&gz_path) {
            let mut encoder = GzEncoder::new(gz_file, Compression::default());
            let _ = encoder.write_all(&content);
            let _ = encoder.finish();
        }
    }

    // Truncate (recreate) the current log file
    let _ = std::fs::write(log_path, b"");
}

/// HARDEN-06: Delete rotated audit log files older than `retention_days` days.
///
/// Scans for `{log_path}.N.gz` files next to the active log and removes
/// those whose modification time is older than the retention window.
/// Returns the count of files removed.
pub fn cleanup_old_rotated_logs(log_path: &std::path::Path, retention_days: u64) -> usize {
    use std::time::{Duration, SystemTime};

    let base = log_path.to_string_lossy().into_owned();
    let cutoff = match SystemTime::now().checked_sub(Duration::from_secs(retention_days * 86400)) {
        Some(t) => t,
        None => return 0,
    };

    let mut removed = 0;
    for i in 1..=99u32 {
        let gz = format!("{}.{}.gz", base, i);
        let p = std::path::Path::new(&gz);
        if !p.exists() {
            break;
        }
        if let Ok(meta) = std::fs::metadata(p) {
            if let Ok(modified) = meta.modified() {
                if modified < cutoff {
                    if std::fs::remove_file(p).is_ok() {
                        removed += 1;
                    }
                }
            }
        }
    }
    removed
}

/// Check if an IP address is private/internal
fn is_private_ip(ip: &str) -> bool {
    ip.starts_with("192.168.") ||
    ip.starts_with("10.") ||
    ip.starts_with("172.16.") ||
    ip.starts_with("172.17.") ||
    ip.starts_with("172.18.") ||
    ip.starts_with("172.19.") ||
    ip.starts_with("172.20.") ||
    ip.starts_with("172.21.") ||
    ip.starts_with("172.22.") ||
    ip.starts_with("172.23.") ||
    ip.starts_with("172.24.") ||
    ip.starts_with("172.25.") ||
    ip.starts_with("172.26.") ||
    ip.starts_with("172.27.") ||
    ip.starts_with("172.28.") ||
    ip.starts_with("172.29.") ||
    ip.starts_with("172.30.") ||
    ip.starts_with("172.31.") ||
    ip.starts_with("127.") ||
    ip == "localhost"
}

/// Generate unique event ID
fn generate_event_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("audit_{}", timestamp)
}

/// Global audit logger instance
static GLOBAL_AUDIT_LOGGER: OnceLock<AuditLogger> = OnceLock::new();

/// Get or initialize global audit logger
pub fn global_audit_logger() -> &'static AuditLogger {
    GLOBAL_AUDIT_LOGGER.get_or_init(|| AuditLogger::new())
}

/// Log audit event using global logger
pub fn audit_log(event: AuditEvent) {
    global_audit_logger().log(event);
}

/// Log authentication event using global logger
pub fn audit_auth(action: &str, user_id: Option<String>, session_id: Option<String>, success: bool) {
    global_audit_logger().log_auth(action, user_id, session_id, success);
}

/// Log authorization failure using global logger
pub fn audit_authz_failure(action: &str, user_id: String, resource: String, reason: String) {
    global_audit_logger().log_authz_failure(action, user_id, resource, reason);
}

/// Log data access using global logger
pub fn audit_data_access(action: &str, resource: &str, user_id: Option<String>) {
    global_audit_logger().log_data_access(action, resource, user_id);
}

/// Log security violation using global logger
pub fn audit_security_violation(violation_type: &str, details: String) {
    global_audit_logger().log_security_violation(violation_type, details);
}

/// Log configuration change using global logger
pub fn audit_config_change(setting: &str, old_value: Option<String>, new_value: String, user_id: String) {
    global_audit_logger().log_config_change(setting, old_value, new_value, user_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audit_event_creation() {
        let event = AuditEvent::success(AuditCategory::Authentication, "login".to_string());
        assert_eq!(event.action, "login");
        assert_eq!(event.category, AuditCategory::Authentication);
        assert_eq!(event.severity, AuditSeverity::Info);
        assert!(event.success);
    }

    #[test]
    fn test_audit_event_builder() {
        let event = AuditEvent::success(AuditCategory::DataAccess, "read_file".to_string())
            .with_user("user123".to_string())
            .with_resource("config.json".to_string())
            .with_metadata("size".to_string(), "1024".to_string());

        assert_eq!(event.user_id, Some("user123".to_string()));
        assert_eq!(event.resource, Some("config.json".to_string()));
        assert_eq!(event.metadata.get("size"), Some(&"1024".to_string()));
    }

    #[test]
    fn test_audit_event_failure() {
        let event = AuditEvent::failure(
            AuditCategory::Authentication,
            "login".to_string(),
            "invalid_password".to_string(),
        );

        assert!(!event.success);
        assert_eq!(event.error_message, Some("invalid_password".to_string()));
        assert_eq!(event.severity, AuditSeverity::Warning);
    }

    #[test]
    fn test_audit_logger_creation() {
        let logger = AuditLogger::new();
        assert!(logger.config().enabled);
        assert_eq!(logger.config().max_memory_events, 10_000);
    }

    #[test]
    fn test_audit_config_presets() {
        let prod = AuditConfig::production();
        assert!(prod.log_to_file);
        assert!(!prod.log_to_console);
        assert!(!prod.include_sensitive_data);

        let dev = AuditConfig::development();
        assert!(!dev.log_to_file);
        assert!(dev.log_to_console);
        assert!(dev.include_sensitive_data);

        let security = AuditConfig::security();
        assert_eq!(security.min_severity, AuditSeverity::Warning);
    }

    #[test]
    fn test_audit_logging() {
        let logger = AuditLogger::new();
        let event = AuditEvent::success(AuditCategory::UserAction, "test_action".to_string());

        logger.log(event.clone());

        let events = logger.get_events(Some(1));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "test_action");
    }

    #[test]
    fn test_audit_logging_by_category() {
        let logger = AuditLogger::new();

        logger.log(AuditEvent::success(AuditCategory::Authentication, "login".to_string()));
        logger.log(AuditEvent::success(AuditCategory::DataAccess, "read".to_string()));
        logger.log(AuditEvent::success(AuditCategory::Authentication, "logout".to_string()));

        let auth_events = logger.get_events_by_category(AuditCategory::Authentication, None);
        assert_eq!(auth_events.len(), 2);

        let data_events = logger.get_events_by_category(AuditCategory::DataAccess, None);
        assert_eq!(data_events.len(), 1);
    }

    #[test]
    fn test_severity_filtering() {
        let config = AuditConfig {
            min_severity: AuditSeverity::Warning,
            ..AuditConfig::default()
        };
        let logger = AuditLogger::with_config(config);

        logger.log(AuditEvent::new(AuditSeverity::Info, AuditCategory::System, "info_event".to_string()));
        logger.log(AuditEvent::new(AuditSeverity::Warning, AuditCategory::System, "warn_event".to_string()));

        let events = logger.get_events(None);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "warn_event");
    }

    #[test]
    fn test_sensitive_data_filtering() {
        let config = AuditConfig {
            include_sensitive_data: false,
            ..AuditConfig::default()
        };
        let logger = AuditLogger::with_config(config);

        let event = AuditEvent::success(AuditCategory::Authentication, "login".to_string())
            .with_source_ip("192.168.1.100".to_string()) // fake IP fixture (synthetic, not real infrastructure)
            .with_metadata("password".to_string(), "secret123".to_string());

        logger.log(event);

        let events = logger.get_events(Some(1));
        assert_eq!(events[0].source_ip, Some("[FILTERED_PRIVATE_IP]".to_string()));
        assert_eq!(events[0].metadata.get("password"), Some(&"[FILTERED]".to_string()));
    }

    #[test]
    fn test_private_ip_detection() {
        assert!(is_private_ip("192.168.1.1"));
        assert!(is_private_ip("10.0.0.1"));
        assert!(is_private_ip("172.16.0.1"));
        assert!(is_private_ip("127.0.0.1"));
        assert!(is_private_ip("localhost"));

        assert!(!is_private_ip("8.8.8.8"));
        assert!(!is_private_ip("1.1.1.1"));
    }

    #[test]
    fn test_audit_log_line_format() {
        let event = AuditEvent::success(AuditCategory::Authentication, "login".to_string())
            .with_user("testuser".to_string())
            .with_source_ip("1.2.3.4".to_string());

        let log_line = event.to_log_line();
        assert!(log_line.contains("AUTH"));
        assert!(log_line.contains("login"));
        assert!(log_line.contains("testuser"));
        assert!(log_line.contains("1.2.3.4"));
        assert!(log_line.contains("Success: true"));
    }

    #[test]
    fn test_global_audit_functions() {
        audit_auth("test_login", Some("user123".to_string()), None, true);
        audit_data_access("read_config", "app.conf", Some("user123".to_string()));

        let logger = global_audit_logger();
        let events = logger.get_events(Some(2));
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn test_security_violation_logging() {
        let logger = AuditLogger::new();
        logger.log_security_violation("brute_force_attempt", "Multiple failed logins detected".to_string());

        let events = logger.get_events_by_severity(AuditSeverity::Critical, None);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].category, AuditCategory::Security);
    }

    #[test]
    fn test_event_storage_limits() {
        let config = AuditConfig {
            max_memory_events: 3,
            ..AuditConfig::default()
        };
        let logger = AuditLogger::with_config(config);

        // Add more events than the limit
        for i in 0..5 {
            logger.log(AuditEvent::success(
                AuditCategory::System,
                format!("event_{}", i),
            ));
        }

        let events = logger.get_events(None);
        assert_eq!(events.len(), 3);
        // Should keep the most recent events
        assert_eq!(events[0].action, "event_4");
        assert_eq!(events[1].action, "event_3");
        assert_eq!(events[2].action, "event_2");
    }
}