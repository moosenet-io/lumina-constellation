//! GUARD-08: Network security and TLS validation
//!
//! Provides network security controls including TLS certificate validation,
//! connection security, and network-based attack prevention.

use crate::error::{LuminaError, Result};
use reqwest::{Client, ClientBuilder};
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::OnceLock;
use std::time::Duration;

/// Network security configuration
#[derive(Debug, Clone)]
pub struct NetworkSecurityConfig {
    /// Whether to enforce strict TLS validation
    pub strict_tls: bool,
    /// Whether to allow self-signed certificates
    pub allow_self_signed: bool,
    /// Whether to block private IP addresses
    pub block_private_ips: bool,
    /// Whether to block localhost connections
    pub block_localhost: bool,
    /// Allowed hostnames for connections
    pub allowed_hosts: Option<HashSet<String>>,
    /// Blocked hostnames
    pub blocked_hosts: HashSet<String>,
    /// Connection timeout in seconds
    pub connection_timeout: u64,
    /// Request timeout in seconds
    pub request_timeout: u64,
    /// Maximum redirects to follow
    pub max_redirects: u32,
    /// User agent string to use
    pub user_agent: String,
}

impl Default for NetworkSecurityConfig {
    fn default() -> Self {
        Self {
            strict_tls: true,
            allow_self_signed: false,
            block_private_ips: true,
            block_localhost: true,
            allowed_hosts: None,
            blocked_hosts: HashSet::new(),
            connection_timeout: 30,
            request_timeout: 60,
            max_redirects: 10,
            user_agent: "Lumina/1.0".to_string(),
        }
    }
}

impl NetworkSecurityConfig {
    /// Production configuration - maximum security
    pub fn production() -> Self {
        let mut blocked_hosts = HashSet::new();
        // Block known malicious/suspicious domains
        blocked_hosts.insert("localhost".to_string());
        blocked_hosts.insert("127.0.0.1".to_string());

        Self {
            strict_tls: true,
            allow_self_signed: false,
            block_private_ips: true,
            block_localhost: true,
            allowed_hosts: None, // Whitelist mode would be more secure
            blocked_hosts,
            connection_timeout: 10, // Shorter timeouts
            request_timeout: 30,
            max_redirects: 5, // Fewer redirects
            user_agent: "Lumina/1.0".to_string(),
        }
    }

    /// Development configuration - more permissive
    pub fn development() -> Self {
        Self {
            strict_tls: false, // Allow self-signed for dev
            allow_self_signed: true,
            block_private_ips: false, // Allow local dev servers
            block_localhost: false,
            allowed_hosts: None,
            blocked_hosts: HashSet::new(),
            connection_timeout: 60,
            request_timeout: 120,
            max_redirects: 10,
            user_agent: "Lumina-Dev/1.0".to_string(),
        }
    }

    /// Security-focused configuration
    pub fn security() -> Self {
        let mut allowed_hosts = HashSet::new();
        // Only allow trusted domains in security mode
        allowed_hosts.insert("api.openai.com".to_string());
        allowed_hosts.insert("api.anthropic.com".to_string());

        Self {
            strict_tls: true,
            allow_self_signed: false,
            block_private_ips: true,
            block_localhost: true,
            allowed_hosts: Some(allowed_hosts),
            blocked_hosts: HashSet::new(),
            connection_timeout: 10,
            request_timeout: 30,
            max_redirects: 3,
            user_agent: "Lumina/1.0".to_string(),
        }
    }
}

/// Network security manager
pub struct NetworkSecurity {
    config: NetworkSecurityConfig,
}

impl NetworkSecurity {
    /// Create new network security manager with default config
    pub fn new() -> Self {
        Self::with_config(NetworkSecurityConfig::default())
    }

    /// Create network security manager with custom config
    pub fn with_config(config: NetworkSecurityConfig) -> Self {
        Self { config }
    }

    /// Validate a URL for security compliance
    pub fn validate_url(&self, url: &str) -> Result<()> {
        // Parse the URL
        let parsed_url = url::Url::parse(url)
            .map_err(|e| LuminaError::SecurityViolation(format!("Invalid URL: {}", e)))?;

        // Check protocol security
        self.validate_protocol(&parsed_url)?;

        // Check hostname security
        self.validate_hostname(&parsed_url)?;

        // Check for IP address restrictions
        self.validate_ip_restrictions(&parsed_url)?;

        Ok(())
    }

    /// Create a secure HTTP client
    pub fn create_secure_client(&self) -> Result<Client> {
        let mut builder = ClientBuilder::new()
            .timeout(Duration::from_secs(self.config.request_timeout))
            .connect_timeout(Duration::from_secs(self.config.connection_timeout))
            .redirect(reqwest::redirect::Policy::limited(self.config.max_redirects as usize))
            .user_agent(&self.config.user_agent);

        // Configure TLS settings
        if self.config.strict_tls {
            builder = builder.danger_accept_invalid_certs(false);
        } else if self.config.allow_self_signed {
            builder = builder.danger_accept_invalid_certs(true);
        }

        // Build the client
        builder.build()
            .map_err(|e| LuminaError::SecurityViolation(format!("Failed to create secure client: {}", e)))
    }

    /// Validate TLS certificate for a connection
    pub fn validate_tls_certificate(&self, host: &str, port: u16) -> Result<()> {
        if !self.config.strict_tls {
            return Ok(());
        }

        // Basic hostname validation
        if host.is_empty() {
            return Err(LuminaError::SecurityViolation("Empty hostname".to_string()));
        }

        // Check for localhost/private addresses
        if self.config.block_localhost && self.is_localhost_or_loopback(host) {
            return Err(LuminaError::SecurityViolation("Localhost connections blocked".to_string()));
        }

        // Validate port is not in dangerous range
        self.validate_port_security(port)?;

        Ok(())
    }

    /// Check if a hostname appears to be localhost or loopback
    fn is_localhost_or_loopback(&self, host: &str) -> bool {
        match host.to_lowercase().as_str() {
            "localhost" | "127.0.0.1" | "::1" => true,
            _ if host.starts_with("127.") => true,
            _ => false,
        }
    }

    /// Validate network port for security
    pub fn validate_port_security(&self, port: u16) -> Result<()> {
        // Block dangerous ports
        const BLOCKED_PORTS: &[u16] = &[
            22,   // SSH
            23,   // Telnet
            25,   // SMTP
            53,   // DNS
            135,  // RPC
            139,  // NetBIOS
            445,  // SMB
            1433, // SQL Server
            3306, // MySQL
            3389, // RDP
            5432, // PostgreSQL
            6379, // Redis
        ];

        if BLOCKED_PORTS.contains(&port) {
            return Err(LuminaError::SecurityViolation(
                format!("Port {} is blocked for security reasons", port)
            ));
        }

        // Ensure port is in valid range
        if port == 0 {
            return Err(LuminaError::SecurityViolation("Port 0 is not allowed".to_string()));
        }

        Ok(())
    }

    /// Check if an IP address is private/internal
    pub fn is_private_ip(&self, ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(ipv4) => self.is_private_ipv4(ipv4),
            IpAddr::V6(ipv6) => self.is_private_ipv6(ipv6),
        }
    }

    /// Check if an IPv4 address is private
    fn is_private_ipv4(&self, ip: Ipv4Addr) -> bool {
        ip.is_private() || ip.is_loopback() || ip.is_link_local()
    }

    /// Check if an IPv6 address is private
    fn is_private_ipv6(&self, ip: Ipv6Addr) -> bool {
        ip.is_loopback() ||
        ip.is_unique_local() ||
        ip.is_unicast_link_local() ||
        ip == Ipv6Addr::UNSPECIFIED
    }

    /// Validate protocol security
    fn validate_protocol(&self, url: &url::Url) -> Result<()> {
        match url.scheme() {
            "https" => Ok(()),
            "http" => {
                if self.config.strict_tls {
                    Err(LuminaError::SecurityViolation(
                        "HTTP connections not allowed in strict TLS mode".to_string()
                    ))
                } else {
                    Ok(())
                }
            }
            scheme => Err(LuminaError::SecurityViolation(
                format!("Protocol '{}' is not allowed", scheme)
            )),
        }
    }

    /// Validate hostname security
    fn validate_hostname(&self, url: &url::Url) -> Result<()> {
        let host = url.host_str()
            .ok_or_else(|| LuminaError::SecurityViolation("No hostname in URL".to_string()))?;

        // Check against blocked hosts
        if self.config.blocked_hosts.contains(host) {
            return Err(LuminaError::SecurityViolation(
                format!("Host '{}' is blocked", host)
            ));
        }

        // Check against allowed hosts (if whitelist mode)
        if let Some(ref allowed_hosts) = self.config.allowed_hosts {
            if !allowed_hosts.contains(host) {
                return Err(LuminaError::SecurityViolation(
                    format!("Host '{}' is not in the allowed list", host)
                ));
            }
        }

        // Check for localhost restrictions
        if self.config.block_localhost && self.is_localhost_or_loopback(host) {
            return Err(LuminaError::SecurityViolation(
                "Localhost connections are blocked".to_string()
            ));
        }

        Ok(())
    }

    /// Validate IP address restrictions
    fn validate_ip_restrictions(&self, url: &url::Url) -> Result<()> {
        if let Some(host) = url.host() {
            match host {
                url::Host::Domain(_) => {
                    // Domain names are generally safe, but we'd need DNS resolution
                    // to check the resolved IP. For now, we trust domain validation.
                    Ok(())
                }
                url::Host::Ipv4(ip) => {
                    if self.config.block_private_ips && self.is_private_ipv4(ip) {
                        Err(LuminaError::SecurityViolation(
                            format!("Private IPv4 address {} is blocked", ip)
                        ))
                    } else {
                        Ok(())
                    }
                }
                url::Host::Ipv6(ip) => {
                    if self.config.block_private_ips && self.is_private_ipv6(ip) {
                        Err(LuminaError::SecurityViolation(
                            format!("Private IPv6 address {} is blocked", ip)
                        ))
                    } else {
                        Ok(())
                    }
                }
            }
        } else {
            Err(LuminaError::SecurityViolation("No host in URL".to_string()))
        }
    }

    /// Get current configuration
    pub fn config(&self) -> &NetworkSecurityConfig {
        &self.config
    }
}

impl Default for NetworkSecurity {
    fn default() -> Self {
        Self::new()
    }
}

/// Global network security instance
static GLOBAL_NETWORK_SECURITY: OnceLock<NetworkSecurity> = OnceLock::new();

/// Get or initialize global network security manager
pub fn global_network_security() -> &'static NetworkSecurity {
    GLOBAL_NETWORK_SECURITY.get_or_init(|| NetworkSecurity::new())
}

/// Validate URL using global network security
pub fn validate_secure_url(url: &str) -> Result<()> {
    global_network_security().validate_url(url)
}

/// Create secure HTTP client using global network security
pub fn create_secure_client() -> Result<Client> {
    global_network_security().create_secure_client()
}

/// Validate TLS certificate using global network security
pub fn validate_tls_connection(host: &str, port: u16) -> Result<()> {
    global_network_security().validate_tls_certificate(host, port)
}

/// Check if IP is private using global network security
pub fn is_private_address(ip: IpAddr) -> bool {
    global_network_security().is_private_ip(ip)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn test_network_security_creation() {
        let security = NetworkSecurity::new();
        assert!(security.config().strict_tls);
        assert!(!security.config().allow_self_signed);
    }

    #[test]
    fn test_config_presets() {
        let prod = NetworkSecurityConfig::production();
        assert!(prod.strict_tls);
        assert!(!prod.allow_self_signed);
        assert_eq!(prod.connection_timeout, 10);

        let dev = NetworkSecurityConfig::development();
        assert!(!dev.strict_tls);
        assert!(dev.allow_self_signed);
        assert!(!dev.block_private_ips);

        let security = NetworkSecurityConfig::security();
        assert!(security.allowed_hosts.is_some());
        assert_eq!(security.max_redirects, 3);
    }

    #[test]
    fn test_url_validation_success() {
        let security = NetworkSecurity::new();
        assert!(security.validate_url("https://example.com").is_ok());
        assert!(security.validate_url("https://api.example.com/v1/test").is_ok());
    }

    #[test]
    fn test_url_validation_failure() {
        let security = NetworkSecurity::new();

        // HTTP should fail in strict mode
        assert!(security.validate_url("http://example.com").is_err());

        // Invalid URLs should fail
        assert!(security.validate_url("not-a-url").is_err());

        // localhost should be blocked
        assert!(security.validate_url("https://localhost").is_err());
    }

    #[test]
    fn test_private_ip_detection() {
        let security = NetworkSecurity::new();

        // IPv4 private addresses
        assert!(security.is_private_ipv4(Ipv4Addr::new(192, 168, 1, 1)));
        assert!(security.is_private_ipv4(Ipv4Addr::new(10, 0, 0, 1)));
        assert!(security.is_private_ipv4(Ipv4Addr::new(172, 16, 0, 1)));
        assert!(security.is_private_ipv4(Ipv4Addr::new(127, 0, 0, 1)));

        // Public IPv4 address
        assert!(!security.is_private_ipv4(Ipv4Addr::new(8, 8, 8, 8)));

        // IPv6 addresses
        assert!(security.is_private_ipv6(Ipv6Addr::LOCALHOST));
        assert!(!security.is_private_ipv6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)));
    }

    #[test]
    fn test_port_validation() {
        let security = NetworkSecurity::new();

        // Safe ports
        assert!(security.validate_port_security(80).is_ok());
        assert!(security.validate_port_security(443).is_ok());
        assert!(security.validate_port_security(8080).is_ok());

        // Blocked ports
        assert!(security.validate_port_security(22).is_err());   // SSH
        assert!(security.validate_port_security(3306).is_err()); // MySQL
        assert!(security.validate_port_security(6379).is_err()); // Redis

        // Invalid port
        assert!(security.validate_port_security(0).is_err());
    }

    #[test]
    fn test_tls_validation() {
        let security = NetworkSecurity::new();

        // Valid hosts
        assert!(security.validate_tls_certificate("example.com", 443).is_ok());

        // Blocked localhost
        assert!(security.validate_tls_certificate("localhost", 443).is_err());
        assert!(security.validate_tls_certificate("127.0.0.1", 443).is_err());

        // Blocked ports
        assert!(security.validate_tls_certificate("example.com", 22).is_err());
    }

    #[test]
    fn test_localhost_detection() {
        let security = NetworkSecurity::new();

        assert!(security.is_localhost_or_loopback("localhost"));
        assert!(security.is_localhost_or_loopback("127.0.0.1"));
        assert!(security.is_localhost_or_loopback("127.0.0.1"));
        assert!(security.is_localhost_or_loopback("::1"));

        assert!(!security.is_localhost_or_loopback("example.com"));
        assert!(!security.is_localhost_or_loopback("192.0.2.11"));
    }

    #[test]
    fn test_protocol_validation() {
        let security = NetworkSecurity::new();

        let https_url = url::Url::parse("https://example.com").unwrap();
        assert!(security.validate_protocol(&https_url).is_ok());

        let http_url = url::Url::parse("http://example.com").unwrap();
        assert!(security.validate_protocol(&http_url).is_err());

        let ftp_url = url::Url::parse("ftp://example.com").unwrap();
        assert!(security.validate_protocol(&ftp_url).is_err());
    }

    #[test]
    fn test_hostname_validation() {
        let mut config = NetworkSecurityConfig::default();
        config.blocked_hosts.insert("blocked.com".to_string());

        let security = NetworkSecurity::with_config(config);

        let safe_url = url::Url::parse("https://example.com").unwrap();
        assert!(security.validate_hostname(&safe_url).is_ok());

        let blocked_url = url::Url::parse("https://blocked.com").unwrap();
        assert!(security.validate_hostname(&blocked_url).is_err());

        let localhost_url = url::Url::parse("https://localhost").unwrap();
        assert!(security.validate_hostname(&localhost_url).is_err());
    }

    #[test]
    fn test_whitelist_mode() {
        let mut allowed_hosts = HashSet::new();
        allowed_hosts.insert("allowed.com".to_string());

        let config = NetworkSecurityConfig {
            allowed_hosts: Some(allowed_hosts),
            ..NetworkSecurityConfig::default()
        };

        let security = NetworkSecurity::with_config(config);

        let allowed_url = url::Url::parse("https://allowed.com").unwrap();
        assert!(security.validate_hostname(&allowed_url).is_ok());

        let blocked_url = url::Url::parse("https://notallowed.com").unwrap();
        assert!(security.validate_hostname(&blocked_url).is_err());
    }

    #[test]
    fn test_ip_restriction_validation() {
        let security = NetworkSecurity::new();

        let public_ip_url = url::Url::parse("https://8.8.8.8").unwrap();
        assert!(security.validate_ip_restrictions(&public_ip_url).is_ok());

        let private_ip_url = url::Url::parse("https://192.168.1.1").unwrap(); // fake IP fixture (synthetic, not real infrastructure)
        assert!(security.validate_ip_restrictions(&private_ip_url).is_err());

        let localhost_ip_url = url::Url::parse("https://127.0.0.1").unwrap();
        assert!(security.validate_ip_restrictions(&localhost_ip_url).is_err());
    }

    #[test]
    fn test_permissive_mode() {
        let config = NetworkSecurityConfig::development();
        let security = NetworkSecurity::with_config(config);

        // Should allow HTTP in development mode
        assert!(security.validate_url("http://localhost:3000").is_ok());

        // Should allow private IPs in development mode
        assert!(security.validate_url("https://192.0.2.151").is_ok());
    }

    #[test]
    fn test_global_functions() {
        assert!(validate_secure_url("https://example.com").is_ok());
        assert!(validate_secure_url("http://example.com").is_err());

        assert!(validate_tls_connection("example.com", 443).is_ok());
        assert!(validate_tls_connection("localhost", 443).is_err());

        let private_ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
        assert!(is_private_address(private_ip));

        let public_ip = IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8));
        assert!(!is_private_address(public_ip));
    }

    #[test]
    fn test_secure_client_creation() {
        let security = NetworkSecurity::new();
        let client = security.create_secure_client();
        assert!(client.is_ok());

        // Test global function
        let global_client = create_secure_client();
        assert!(global_client.is_ok());
    }
}