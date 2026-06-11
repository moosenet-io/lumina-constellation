//! WEB-06: Service connector framework
//!
//! Provides a credential-gated, egress-checked plugin interface for external
//! service integrations. Each service connector reads its credentials from
//! environment variables — nothing is hardcoded.
//!
//! # Design
//! - [`ServiceConnector`] trait: uniform interface for all connectors
//! - [`ConnectorRegistry`]: discovers and holds configured connectors
//! - [`InfisicalCredentialProvider`]: resolves secrets from env (populated by
//!   Infisical fetch scripts in production)
//! - Individual connectors: Grocy, Actual Budget, LubeLogger, Jellyseerr
//!
//! # Egress safety
//! Every API call must be pre-approved by an [`EgressInspector`] instance. A
//! connector's `health_check` and any tool dispatch **must** call
//! `egress_inspector.inspect(url, tool_name)` before opening a connection.
//!
//! # Missing credentials
//! If a connector's required env vars are absent, `is_configured()` returns
//! `false` and the registry silently skips it. No error is surfaced — the
//! feature is simply unavailable until credentials are provisioned.
//!
//! # Deferred: Infisical vault cache
//! The spec calls for fetching credentials from Infisical at startup and
//! caching them in vault with periodic refresh (same pattern as Harmony's
//! Infisical backend). That work is deferred — the current implementation
//! reads from env vars that are pre-populated by `fetch-mcp-secrets.sh`.
//! Tracked as a follow-up task.

pub mod grocy;
pub mod actual;
pub mod lubelogger;
pub mod jellyseerr;

use std::collections::HashMap;
use async_trait::async_trait;
use serde_json::Value;
use crate::egress_inspector::EgressInspector;
use crate::error::Result;
use crate::tool_types::{ToolDefinition, ToolResult};

// ─────────────────────────────────────────────────────────────────────────────
// ServiceConnector trait
// ─────────────────────────────────────────────────────────────────────────────

/// A pluggable integration with an external service.
///
/// Implementors read credentials from environment variables only.  No URL,
/// API key, or secret may be hardcoded.
#[async_trait]
pub trait ServiceConnector: Send + Sync {
    /// Short, stable identifier (e.g. `"grocy"`, `"actual"`).
    fn name(&self) -> &str;

    /// Returns `true` when all required env vars are present and non-empty.
    ///
    /// The registry calls this during construction; connectors that return
    /// `false` are not registered.
    fn is_configured(&self) -> bool;

    /// Perform a lightweight connectivity check against the service.
    ///
    /// Must call the egress inspector before any outbound request.
    /// Returns `Ok(true)` on success, `Ok(false)` on reachability failure,
    /// and `Err(_)` only for unrecoverable errors (e.g. egress block).
    async fn health_check(&self) -> Result<bool>;

    /// Return the list of MCP tool definitions this connector exposes.
    ///
    /// Tool definitions must NOT embed credential values.  Schemas describe
    /// accepted arguments; actual credentials are injected at call time from
    /// the environment.
    fn tools(&self) -> Vec<ToolDefinition>;

    /// Execute a named tool with the given JSON arguments.
    ///
    /// The `tool_call_id` is an opaque correlation token passed through to the
    /// returned [`ToolResult`] so the caller can match responses to requests.
    ///
    /// Returns `Err` only for infrastructure failures (egress block, network
    /// error that cannot be represented as a soft failure).  Logical errors
    /// (unknown tool name, bad arguments, upstream 4xx) are returned as
    /// `Ok(ToolResult { success: false, … })`.
    async fn execute_tool(
        &self,
        tool_call_id: &str,
        tool_name: &str,
        args: &Value,
    ) -> Result<ToolResult>;
}

// ─────────────────────────────────────────────────────────────────────────────
// InfisicalCredentialProvider
// ─────────────────────────────────────────────────────────────────────────────

/// Resolves service credentials from environment variables.
///
/// In production the env is populated by `fetch-mcp-secrets.sh` which pulls
/// values from Infisical.  In tests the caller sets env vars directly.
///
/// # Security
/// This type deliberately does NOT store secrets in-memory beyond the
/// duration of a single lookup. Every call reads from the environment at the
/// moment it is invoked.
pub struct InfisicalCredentialProvider;

impl InfisicalCredentialProvider {
    /// Create a new provider (stateless — construction is free).
    pub fn new() -> Self {
        Self
    }

    /// Read a secret from an env var.
    ///
    /// Returns `Some(value)` when the var is set and non-empty.
    /// Returns `None` when the var is absent or empty (treat as unconfigured).
    pub fn get(&self, env_var: &str) -> Option<String> {
        std::env::var(env_var)
            .ok()
            .filter(|v| !v.is_empty())
    }

    /// Return `true` if all supplied env var names are present and non-empty.
    pub fn all_present(&self, vars: &[&str]) -> bool {
        vars.iter().all(|v| self.get(v).is_some())
    }
}

impl Default for InfisicalCredentialProvider {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ConnectorRegistry
// ─────────────────────────────────────────────────────────────────────────────

/// Holds all registered, configured service connectors.
///
/// Connectors are inserted by name and keyed by their [`ServiceConnector::name`].
/// Only connectors that return `is_configured() == true` should be inserted;
/// [`ConnectorRegistry::try_register`] enforces this automatically.
pub struct ConnectorRegistry {
    connectors: HashMap<String, Box<dyn ServiceConnector>>,
}

impl ConnectorRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            connectors: HashMap::new(),
        }
    }

    /// Register a connector unconditionally.
    ///
    /// Prefer [`try_register`] in application code so that unconfigured
    /// connectors are silently skipped.
    pub fn register(&mut self, connector: Box<dyn ServiceConnector>) {
        let name = connector.name().to_string();
        self.connectors.insert(name, connector);
    }

    /// Register `connector` only if `is_configured()` returns `true`.
    ///
    /// Returns `true` when the connector was registered, `false` when it was
    /// silently skipped due to missing credentials.
    pub fn try_register(&mut self, connector: Box<dyn ServiceConnector>) -> bool {
        if connector.is_configured() {
            let name = connector.name().to_string();
            self.connectors.insert(name, connector);
            true
        } else {
            false
        }
    }

    /// Return references to all currently registered connectors.
    pub fn configured_connectors(&self) -> Vec<&dyn ServiceConnector> {
        self.connectors.values().map(|c| c.as_ref()).collect()
    }

    /// Retrieve a single connector by name.
    pub fn get(&self, name: &str) -> Option<&dyn ServiceConnector> {
        self.connectors.get(name).map(|c| c.as_ref())
    }

    /// Run `health_check` on every registered connector.
    ///
    /// Returns a map of connector name → health status.  Errors are mapped to
    /// `false` so the caller always gets a complete picture.
    pub async fn health_check_all(&self) -> HashMap<String, bool> {
        let mut results = HashMap::new();
        for (name, connector) in &self.connectors {
            let healthy = connector.health_check().await.unwrap_or(false);
            results.insert(name.clone(), healthy);
        }
        results
    }

    /// Collect all tool definitions from all registered connectors.
    pub fn all_tools(&self) -> Vec<ToolDefinition> {
        self.connectors
            .values()
            .flat_map(|c| c.tools())
            .collect()
    }

    /// Number of registered connectors.
    pub fn len(&self) -> usize {
        self.connectors.len()
    }

    /// Returns `true` when no connectors are registered.
    pub fn is_empty(&self) -> bool {
        self.connectors.is_empty()
    }
}

impl Default for ConnectorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Shared egress helpers (used by all connector submodules)
// ─────────────────────────────────────────────────────────────────────────────

/// Build an [`EgressInspector`] that allows traffic only to the host found in
/// `server_url`.  Falls back to an empty (deny-all) inspector when the URL is
/// absent or unparseable.
pub(crate) fn build_egress(server_url: &Option<String>) -> EgressInspector {
    match server_url {
        Some(url) => {
            if let Some(host) = extract_host_from_url(url) {
                EgressInspector::new(vec![host])
            } else {
                EgressInspector::new(vec![])
            }
        }
        None => EgressInspector::new(vec![]),
    }
}

/// Extract the hostname from a URL string without pulling in a full URL parser.
///
/// Handles:
/// - Standard `scheme://host:port/path` → `host`
/// - No-port `scheme://host/path` → `host`
/// - IPv6 literals `scheme://[::1]:port/path` → `::1`
pub(crate) fn extract_host_from_url(url: &str) -> Option<String> {
    // Strip scheme (everything up to and including "://")
    let after_scheme = url.find("://").map(|i| &url[i + 3..])?;
    // Authority is everything before the first '/', '?', or '#'
    let authority = after_scheme
        .split(|c: char| c == '/' || c == '?' || c == '#')
        .next()
        .unwrap_or(after_scheme);

    // IPv6 literal: authority starts with '[', e.g. "[::1]:8080"
    if authority.starts_with('[') {
        let end = authority.find(']')?;
        let host = &authority[1..end];
        return if host.is_empty() { None } else { Some(host.to_string()) };
    }

    // Standard host[:port]
    let host = if authority.contains(':') {
        authority.split(':').next().unwrap_or(authority)
    } else {
        authority
    };
    if host.is_empty() { None } else { Some(host.to_string()) }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::sync::Mutex;

    /// Serialize env-var mutations across tests.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    // ── Stub connector for registry tests ────────────────────────────────────

    struct StubConnector {
        name: String,
        configured: bool,
    }

    impl StubConnector {
        fn new(name: &str, configured: bool) -> Self {
            Self { name: name.to_string(), configured }
        }
    }

    #[async_trait]
    impl ServiceConnector for StubConnector {
        fn name(&self) -> &str { &self.name }
        fn is_configured(&self) -> bool { self.configured }

        async fn health_check(&self) -> Result<bool> {
            Ok(self.configured)
        }

        fn tools(&self) -> Vec<ToolDefinition> {
            vec![ToolDefinition::read_only(
                format!("{}_tool", self.name),
                format!("A tool from {}", self.name),
                serde_json::json!({"type": "object", "properties": {}}),
            )]
        }

        async fn execute_tool(
            &self,
            tool_call_id: &str,
            tool_name: &str,
            _args: &Value,
        ) -> Result<ToolResult> {
            let expected = format!("{}_tool", self.name);
            if tool_name == expected {
                Ok(ToolResult::success(
                    tool_call_id.to_string(),
                    tool_name.to_string(),
                    format!("stub result from {}", self.name),
                ))
            } else {
                Ok(ToolResult::error(
                    tool_call_id.to_string(),
                    tool_name.to_string(),
                    format!("unknown tool: {}", tool_name),
                ))
            }
        }
    }

    // ── registry tests ────────────────────────────────────────────────────────

    #[test]
    fn test_registry_discovers_configured_connectors() {
        let mut registry = ConnectorRegistry::new();
        registry.try_register(Box::new(StubConnector::new("alpha", true)));
        registry.try_register(Box::new(StubConnector::new("beta", true)));

        let connectors = registry.configured_connectors();
        assert_eq!(connectors.len(), 2);
    }

    #[test]
    fn test_unconfigured_connector_not_registered() {
        let mut registry = ConnectorRegistry::new();
        registry.try_register(Box::new(StubConnector::new("unconfigured", false)));

        assert!(registry.is_empty(),
            "Unconfigured connector must not be registered");
    }

    #[test]
    fn test_each_connector_produces_tool_definitions() {
        let mut registry = ConnectorRegistry::new();
        registry.try_register(Box::new(StubConnector::new("svc_a", true)));
        registry.try_register(Box::new(StubConnector::new("svc_b", true)));

        let tools = registry.all_tools();
        // 1 tool per connector
        assert_eq!(tools.len(), 2);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"svc_a_tool"));
        assert!(names.contains(&"svc_b_tool"));
    }

    #[test]
    fn test_mixed_configured_unconfigured() {
        let mut registry = ConnectorRegistry::new();
        registry.try_register(Box::new(StubConnector::new("good", true)));
        registry.try_register(Box::new(StubConnector::new("bad", false)));

        assert_eq!(registry.len(), 1);
        assert!(registry.get("good").is_some());
        assert!(registry.get("bad").is_none());
    }

    // ── InfisicalCredentialProvider tests ────────────────────────────────────

    #[test]
    #[serial]
    fn test_credential_provider_returns_value_when_set() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("TEST_CRED_KEY", "secret_value");
        let provider = InfisicalCredentialProvider::new();
        assert_eq!(provider.get("TEST_CRED_KEY"), Some("secret_value".to_string()));
        std::env::remove_var("TEST_CRED_KEY");
    }

    #[test]
    #[serial]
    fn test_credential_provider_returns_none_when_absent() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("TEST_MISSING_KEY");
        let provider = InfisicalCredentialProvider::new();
        assert!(provider.get("TEST_MISSING_KEY").is_none());
    }

    #[test]
    #[serial]
    fn test_credential_provider_returns_none_when_empty() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("TEST_EMPTY_KEY", "");
        let provider = InfisicalCredentialProvider::new();
        assert!(provider.get("TEST_EMPTY_KEY").is_none(),
            "Empty env var should be treated as absent");
        std::env::remove_var("TEST_EMPTY_KEY");
    }

    #[test]
    #[serial]
    fn test_all_present_true_when_all_set() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("KEY_A", "v1");
        std::env::set_var("KEY_B", "v2");
        let provider = InfisicalCredentialProvider::new();
        assert!(provider.all_present(&["KEY_A", "KEY_B"]));
        std::env::remove_var("KEY_A");
        std::env::remove_var("KEY_B");
    }

    #[test]
    #[serial]
    fn test_all_present_false_when_any_missing() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("KEY_ONLY_A", "v1");
        std::env::remove_var("KEY_ONLY_B");
        let provider = InfisicalCredentialProvider::new();
        assert!(!provider.all_present(&["KEY_ONLY_A", "KEY_ONLY_B"]));
        std::env::remove_var("KEY_ONLY_A");
    }

    // ── grocy connector env var tests ─────────────────────────────────────────

    #[test]
    #[serial]
    fn test_grocy_connector_disabled_when_no_url() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("GROCY_URL");
        std::env::remove_var("GROCY_API_KEY");
        let connector = grocy::GrocyConnector::from_env();
        assert!(!connector.is_configured(),
            "GrocyConnector must be disabled when GROCY_URL is not set");
    }

    #[test]
    #[serial]
    fn test_grocy_connector_disabled_when_no_api_key() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("GROCY_URL", "http://grocy.local");
        std::env::remove_var("GROCY_API_KEY");
        let connector = grocy::GrocyConnector::from_env();
        assert!(!connector.is_configured(),
            "GrocyConnector must be disabled when GROCY_API_KEY is not set");
        std::env::remove_var("GROCY_URL");
    }

    // ── actual connector env var tests ────────────────────────────────────────

    #[test]
    #[serial]
    fn test_actual_connector_disabled_when_no_key() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("ACTUAL_SERVER_URL");
        std::env::remove_var("ACTUAL_HTTP_API_KEY");
        let connector = actual::ActualBudgetConnector::from_env();
        assert!(!connector.is_configured(),
            "ActualBudgetConnector must be disabled when credentials are absent");
    }

    // ── tool output sanitisation ──────────────────────────────────────────────

    #[test]
    #[serial]
    fn test_credentials_not_in_tool_outputs() {
        let _g = ENV_LOCK.lock().unwrap();
        // Set env vars with distinctive sentinel values
        std::env::set_var("GROCY_URL", "http://grocy-secret-url.local");
        std::env::set_var("GROCY_API_KEY", "secret-grocy-key-abc123");

        let connector = grocy::GrocyConnector::from_env();
        for tool in connector.tools() {
            let schema_str = tool.argument_schema.to_string();
            assert!(!schema_str.contains("grocy-secret-url"),
                "Tool schema must not leak GROCY_URL");
            assert!(!schema_str.contains("secret-grocy-key"),
                "Tool schema must not leak GROCY_API_KEY");
            assert!(!tool.description.contains("grocy-secret-url"),
                "Tool description must not leak GROCY_URL");
            assert!(!tool.description.contains("secret-grocy-key"),
                "Tool description must not leak GROCY_API_KEY");
        }

        std::env::remove_var("GROCY_URL");
        std::env::remove_var("GROCY_API_KEY");
    }

    // ── extract_host_from_url tests ───────────────────────────────────────────

    #[test]
    fn test_extract_host_standard_with_port() {
        assert_eq!(
            extract_host_from_url("http://grocy.local:9283/api"),
            Some("grocy.local".to_string())
        );
    }

    #[test]
    fn test_extract_host_standard_no_port() {
        assert_eq!(
            extract_host_from_url("https://actual.internal"),
            Some("actual.internal".to_string())
        );
    }

    #[test]
    fn test_extract_host_ipv4_with_port() {
        assert_eq!(
            extract_host_from_url("http://192.0.2.50:9283/api"),
            Some("192.0.2.50".to_string())
        );
    }

    #[test]
    fn test_extract_host_ipv6_literal_with_port() {
        // IPv6 brackets must be stripped; the host is the bare address
        assert_eq!(
            extract_host_from_url("http://[::1]:8080/api"),
            Some("::1".to_string()),
            "IPv6 bracket-stripping must produce bare address, not '['"
        );
    }

    #[test]
    fn test_extract_host_ipv6_literal_no_port() {
        assert_eq!(
            extract_host_from_url("http://[2001:db8::1]/path"),
            Some("2001:db8::1".to_string())
        );
    }

    // ── egress inspector validates service URLs ───────────────────────────────

    #[test]
    fn test_egress_allows_configured_host() {
        let url = Some("http://grocy.local:9283".to_string());
        let inspector = build_egress(&url);
        // Same host + path must pass
        assert!(
            inspector.inspect("http://grocy.local:9283/api/system/info", "test_tool").is_ok(),
            "EgressInspector must allow the configured host"
        );
    }

    #[test]
    fn test_egress_blocks_different_host() {
        let url = Some("http://grocy.local:9283".to_string());
        let inspector = build_egress(&url);
        // A completely different host must be blocked
        assert!(
            inspector.inspect("http://evil.example.com/steal", "test_tool").is_err(),
            "EgressInspector must block a host that is not in the allowlist"
        );
    }

    #[test]
    fn test_egress_deny_all_when_no_url() {
        let inspector = build_egress(&None);
        assert!(
            inspector.inspect("http://any-host.example.com/", "test_tool").is_err(),
            "EgressInspector must deny all traffic when no URL is configured"
        );
    }

    // ── execute_tool dispatch ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_stub_execute_known_tool_returns_success() {
        let stub = StubConnector::new("widget", true);
        let result = stub
            .execute_tool("call-1", "widget_tool", &serde_json::json!({}))
            .await
            .expect("execute_tool must not error for known tool");
        assert!(result.success, "Known tool must return success=true");
    }

    #[tokio::test]
    async fn test_stub_execute_unknown_tool_returns_error_result() {
        let stub = StubConnector::new("widget", true);
        let result = stub
            .execute_tool("call-2", "nonexistent_tool", &serde_json::json!({}))
            .await
            .expect("execute_tool must not error even for unknown tool");
        assert!(!result.success, "Unknown tool must return success=false");
    }
}
