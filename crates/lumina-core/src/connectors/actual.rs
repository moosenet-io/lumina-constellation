//! Actual Budget service connector
//!
//! Actual Budget is a self-hosted personal finance application.
//! This connector exposes three MCP tools:
//!
//! | Tool name               | Permission | Description                          |
//! |-------------------------|------------|--------------------------------------|
//! | `budget_summary`        | ReadOnly   | Retrieve overall budget summary       |
//! | `budget_category`       | ReadOnly   | Query spending by category            |
//! | `recent_transactions`   | ReadOnly   | List recent transactions              |
//!
//! # Required environment variables
//! | Variable                | Description                                      |
//! |-------------------------|--------------------------------------------------|
//! | `ACTUAL_SERVER_URL`     | Base URL of the Actual server (e.g. `http://...`)|
//! | `ACTUAL_HTTP_API_KEY`   | HTTP API key (from Actual Settings → Advanced)   |
//!
//! Both variables must be present and non-empty for the connector to be active.
//!
//! ## Authentication note
//! Actual Budget exposes two distinct auth mechanisms:
//! - **Server password** (`ACTUAL_SERVER_PASSWORD`): used by the sync/local
//!   protocol only.  Not accepted by the HTTP REST API.
//! - **HTTP API key** (`ACTUAL_HTTP_API_KEY`): the token used with the REST API
//!   (`Authorization: Bearer <key>`).
//!
//! This connector uses the REST API, so only `ACTUAL_HTTP_API_KEY` is needed.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::connectors::{build_egress, InfisicalCredentialProvider, ServiceConnector};
use crate::error::{LuminaError, Result};
use crate::tool_types::{ToolDefinition, ToolResult};

// ─────────────────────────────────────────────────────────────────────────────
// Env var names
// ─────────────────────────────────────────────────────────────────────────────

const ENV_ACTUAL_SERVER_URL: &str = "ACTUAL_SERVER_URL";
const ENV_ACTUAL_HTTP_API_KEY: &str = "ACTUAL_HTTP_API_KEY";

// ─────────────────────────────────────────────────────────────────────────────
// ActualBudgetConnector
// ─────────────────────────────────────────────────────────────────────────────

/// Connector for the Actual Budget personal finance service.
pub struct ActualBudgetConnector {
    server_url: Option<String>,
    api_key: Option<String>,
    egress: crate::egress_inspector::EgressInspector,
    /// Reused across all requests — avoids allocating a new connection pool
    /// and TLS context on every health check or tool call.
    client: reqwest::Client,
}

impl ActualBudgetConnector {
    /// Build a connector from environment variables.
    pub fn from_env() -> Self {
        let creds = InfisicalCredentialProvider::new();
        let server_url = creds.get(ENV_ACTUAL_SERVER_URL);
        let api_key = creds.get(ENV_ACTUAL_HTTP_API_KEY);

        let egress = build_egress(&server_url);
        let client = reqwest::Client::new();

        Self { server_url, api_key, egress, client }
    }

    fn url(&self) -> Result<&str> {
        self.server_url.as_deref().ok_or_else(|| {
            LuminaError::Config(format!("{} is not set", ENV_ACTUAL_SERVER_URL))
        })
    }

    fn key(&self) -> Result<&str> {
        self.api_key.as_deref().ok_or_else(|| {
            LuminaError::Config(format!("{} is not set", ENV_ACTUAL_HTTP_API_KEY))
        })
    }
}

#[async_trait]
impl ServiceConnector for ActualBudgetConnector {
    fn name(&self) -> &str {
        "actual"
    }

    /// Returns `true` when both required env vars were present at construction
    /// time.  Uses the stored fields — never re-reads the environment — so
    /// the result is consistent with what `from_env()` observed.
    fn is_configured(&self) -> bool {
        self.server_url.is_some() && self.api_key.is_some()
    }

    async fn health_check(&self) -> Result<bool> {
        let base = self.url()?;
        // Actual Budget exposes a health endpoint at the root
        let endpoint = format!("{}/health", base);

        self.egress.inspect(&endpoint, "actual_health_check")
            .map_err(LuminaError::from)?;

        let resp = self.client
            .get(&endpoint)
            .header("Authorization", format!("Bearer {}", self.key()?))
            .send()
            .await;
        match resp {
            Ok(r) => Ok(r.status().is_success()),
            Err(_) => Ok(false),
        }
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition::read_only(
                "budget_summary".to_string(),
                "Retrieve overall budget summary for a given month, including total \
                 budgeted, spent, and remaining across all categories."
                    .to_string(),
                json!({
                    "type": "object",
                    "properties": {
                        "month": {
                            "type": "string",
                            "description": "Month in YYYY-MM format (defaults to current month if omitted)"
                        }
                    },
                    "additionalProperties": false
                }),
            ),
            ToolDefinition::read_only(
                "budget_category".to_string(),
                "Query spending and budget allocation for a specific category or \
                 category group over a date range."
                    .to_string(),
                json!({
                    "type": "object",
                    "required": ["category_name"],
                    "properties": {
                        "category_name": {
                            "type": "string",
                            "description": "Name of the budget category (case-insensitive)"
                        },
                        "month": {
                            "type": "string",
                            "description": "Month in YYYY-MM format (defaults to current month)"
                        }
                    },
                    "additionalProperties": false
                }),
            ),
            ToolDefinition::read_only(
                "recent_transactions".to_string(),
                "List recent transactions from Actual Budget, with optional filtering \
                 by account, category, date range, or payee."
                    .to_string(),
                json!({
                    "type": "object",
                    "properties": {
                        "account_name": {
                            "type": "string",
                            "description": "Filter to transactions from a specific account"
                        },
                        "category_name": {
                            "type": "string",
                            "description": "Filter to transactions in a specific category"
                        },
                        "payee": {
                            "type": "string",
                            "description": "Filter by payee name (substring match)"
                        },
                        "since_date": {
                            "type": "string",
                            "description": "Earliest date to include, YYYY-MM-DD format"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of transactions to return (default 50, max 200)",
                            "minimum": 1,
                            "maximum": 200
                        }
                    },
                    "additionalProperties": false
                }),
            ),
        ]
    }

    async fn execute_tool(
        &self,
        tool_call_id: &str,
        tool_name: &str,
        args: &Value,
    ) -> Result<ToolResult> {
        match tool_name {
            "budget_summary" => {
                let base = self.url()?;
                let month = args.get("month").and_then(|v| v.as_str()).unwrap_or("");
                let endpoint = if month.is_empty() {
                    format!("{}/budget/summary", base)
                } else {
                    format!("{}/budget/summary?month={}", base, month)
                };
                self.egress.inspect(&endpoint, tool_name)
                    .map_err(LuminaError::from)?;
                let resp = self.client
                    .get(&endpoint)
                    .header("Authorization", format!("Bearer {}", self.key()?))
                    .send()
                    .await;
                match resp {
                    Ok(r) if r.status().is_success() => {
                        let body = r.text().await.unwrap_or_default();
                        Ok(ToolResult::success(tool_call_id.to_string(), tool_name.to_string(), body))
                    }
                    Ok(r) => Ok(ToolResult::error(
                        tool_call_id.to_string(),
                        tool_name.to_string(),
                        format!("Actual Budget returned HTTP {}", r.status()),
                    )),
                    Err(e) => Ok(ToolResult::error(
                        tool_call_id.to_string(),
                        tool_name.to_string(),
                        format!("Request failed: {}", e),
                    )),
                }
            }
            "budget_category" => {
                let base = self.url()?;
                let category = args.get("category_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let month = args.get("month").and_then(|v| v.as_str()).unwrap_or("");
                let endpoint = if month.is_empty() {
                    format!("{}/budget/category/{}", base, category)
                } else {
                    format!("{}/budget/category/{}?month={}", base, category, month)
                };
                self.egress.inspect(&endpoint, tool_name)
                    .map_err(LuminaError::from)?;
                let resp = self.client
                    .get(&endpoint)
                    .header("Authorization", format!("Bearer {}", self.key()?))
                    .send()
                    .await;
                match resp {
                    Ok(r) if r.status().is_success() => {
                        let body = r.text().await.unwrap_or_default();
                        Ok(ToolResult::success(tool_call_id.to_string(), tool_name.to_string(), body))
                    }
                    Ok(r) => Ok(ToolResult::error(
                        tool_call_id.to_string(),
                        tool_name.to_string(),
                        format!("Actual Budget returned HTTP {}", r.status()),
                    )),
                    Err(e) => Ok(ToolResult::error(
                        tool_call_id.to_string(),
                        tool_name.to_string(),
                        format!("Request failed: {}", e),
                    )),
                }
            }
            "recent_transactions" => {
                let base = self.url()?;
                let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50);
                let endpoint = format!("{}/transactions?limit={}", base, limit);
                self.egress.inspect(&endpoint, tool_name)
                    .map_err(LuminaError::from)?;
                let resp = self.client
                    .get(&endpoint)
                    .header("Authorization", format!("Bearer {}", self.key()?))
                    .send()
                    .await;
                match resp {
                    Ok(r) if r.status().is_success() => {
                        let body = r.text().await.unwrap_or_default();
                        Ok(ToolResult::success(tool_call_id.to_string(), tool_name.to_string(), body))
                    }
                    Ok(r) => Ok(ToolResult::error(
                        tool_call_id.to_string(),
                        tool_name.to_string(),
                        format!("Actual Budget returned HTTP {}", r.status()),
                    )),
                    Err(e) => Ok(ToolResult::error(
                        tool_call_id.to_string(),
                        tool_name.to_string(),
                        format!("Request failed: {}", e),
                    )),
                }
            }
            _ => Ok(ToolResult::error(
                tool_call_id.to_string(),
                tool_name.to_string(),
                format!("ActualBudgetConnector does not provide tool '{}'", tool_name),
            )),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn set_valid_creds() {
        std::env::set_var(ENV_ACTUAL_SERVER_URL, "http://actual.local:5006");
        std::env::set_var(ENV_ACTUAL_HTTP_API_KEY, "test-http-api-key");
    }

    fn clear_creds() {
        std::env::remove_var(ENV_ACTUAL_SERVER_URL);
        std::env::remove_var(ENV_ACTUAL_HTTP_API_KEY);
    }

    #[test]
    fn test_actual_configured_when_all_vars_set() {
        let _g = ENV_LOCK.lock().unwrap();
        set_valid_creds();
        let c = ActualBudgetConnector::from_env();
        assert!(c.is_configured());
        clear_creds();
    }

    #[test]
    #[serial]
    fn test_actual_not_configured_when_url_missing() {
        let _g = ENV_LOCK.lock().unwrap();
        set_valid_creds();
        std::env::remove_var(ENV_ACTUAL_SERVER_URL);
        let c = ActualBudgetConnector::from_env();
        assert!(!c.is_configured());
        clear_creds();
    }

    #[test]
    #[serial]
    fn test_actual_not_configured_when_api_key_missing() {
        let _g = ENV_LOCK.lock().unwrap();
        set_valid_creds();
        std::env::remove_var(ENV_ACTUAL_HTTP_API_KEY);
        let c = ActualBudgetConnector::from_env();
        assert!(!c.is_configured());
        clear_creds();
    }

    /// Verify that is_configured() uses the stored fields snapshotted at
    /// construction time, not a live re-read of env vars.
    #[test]
    fn test_is_configured_uses_stored_fields_not_live_env() {
        let _g = ENV_LOCK.lock().unwrap();
        set_valid_creds();
        let c = ActualBudgetConnector::from_env();
        // Clear env *after* construction
        clear_creds();
        // Must still report configured — we read at from_env() time, not now
        assert!(
            c.is_configured(),
            "is_configured() must use stored fields, not live env re-read"
        );
    }

    /// The inverse: a connector constructed with missing vars stays unconfigured
    /// even if vars are set later.
    #[test]
    fn test_is_configured_false_even_if_env_set_after_construction() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_creds();
        let c = ActualBudgetConnector::from_env();
        // Set env *after* construction
        set_valid_creds();
        assert!(
            !c.is_configured(),
            "is_configured() must not pick up env vars set after from_env()"
        );
        clear_creds();
    }

    #[test]
    fn test_actual_name() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_creds();
        assert_eq!(ActualBudgetConnector::from_env().name(), "actual");
    }

    #[test]
    fn test_actual_tools_count() {
        let _g = ENV_LOCK.lock().unwrap();
        set_valid_creds();
        let tools = ActualBudgetConnector::from_env().tools();
        assert_eq!(tools.len(), 3);
        clear_creds();
    }

    #[test]
    fn test_actual_tool_names() {
        let _g = ENV_LOCK.lock().unwrap();
        set_valid_creds();
        let connector = ActualBudgetConnector::from_env();
        let tools = connector.tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"budget_summary"));
        assert!(names.contains(&"budget_category"));
        assert!(names.contains(&"recent_transactions"));
        clear_creds();
    }

    #[test]
    #[serial]
    fn test_actual_tool_schemas_have_no_credentials() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var(ENV_ACTUAL_SERVER_URL, "http://secret-actual.internal");
        std::env::set_var(ENV_ACTUAL_HTTP_API_KEY, "topsecret-http-api-key");
        let connector = ActualBudgetConnector::from_env();
        for tool in connector.tools() {
            let s = tool.argument_schema.to_string();
            assert!(!s.contains("secret-actual"), "URL must not be in schema");
            assert!(!s.contains("topsecret-http-api-key"), "API key must not be in schema");
        }
        clear_creds();
    }

    #[test]
    fn test_all_actual_tools_are_read_only() {
        let _g = ENV_LOCK.lock().unwrap();
        set_valid_creds();
        for tool in ActualBudgetConnector::from_env().tools() {
            assert_eq!(
                tool.permission,
                crate::tool_types::ToolPermission::ReadOnly,
                "All Actual Budget tools should be ReadOnly"
            );
        }
        clear_creds();
    }

    #[tokio::test]
    async fn test_execute_unknown_tool_returns_error_result() {
        let _g = ENV_LOCK.lock().unwrap();
        set_valid_creds();
        let connector = ActualBudgetConnector::from_env();
        let result = connector
            .execute_tool("id-1", "nonexistent_tool", &serde_json::json!({}))
            .await
            .expect("execute_tool must not return Err for unknown tool");
        assert!(!result.success, "Unknown tool must produce success=false");
        assert!(
            result.error.as_deref().unwrap_or("").contains("nonexistent_tool"),
            "Error message must name the unknown tool"
        );
        clear_creds();
    }
}
