//! LubeLogger service connector
//!
//! LubeLogger is a self-hosted vehicle maintenance tracker.
//! This connector exposes three MCP tools:
//!
//! | Tool name         | Permission | Description                               |
//! |-------------------|------------|-------------------------------------------|
//! | `vehicle_status`  | ReadOnly   | Get status summary for one or all vehicles|
//! | `maintenance_due` | ReadOnly   | List overdue or upcoming maintenance items|
//! | `log_fuel`        | ReadWrite  | Log a fuel-up event for a vehicle         |
//!
//! # Required environment variables
//! | Variable              | Description                                    |
//! |-----------------------|------------------------------------------------|
//! | `LUBELOGGER_URL`      | Base URL of the LubeLogger instance            |
//! | `LUBELOGGER_API_KEY`  | API key for authentication                     |
//!
//! Both variables must be present and non-empty for the connector to be active.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::connectors::{build_egress, InfisicalCredentialProvider, ServiceConnector};
use crate::egress_inspector::EgressInspector;
use crate::error::{LuminaError, Result};
use crate::tool_types::{ToolDefinition, ToolPermission, ToolResult};

// ─────────────────────────────────────────────────────────────────────────────
// Env var names
// ─────────────────────────────────────────────────────────────────────────────

const ENV_LUBELOGGER_URL: &str = "LUBELOGGER_URL";
const ENV_LUBELOGGER_API_KEY: &str = "LUBELOGGER_API_KEY";

// ─────────────────────────────────────────────────────────────────────────────
// LubeLoggerConnector
// ─────────────────────────────────────────────────────────────────────────────

/// Connector for the LubeLogger vehicle maintenance service.
pub struct LubeLoggerConnector {
    base_url: Option<String>,
    api_key: Option<String>,
    egress: EgressInspector,
    /// Reused across all requests — avoids allocating a new connection pool
    /// and TLS context on every health check or tool call.
    client: reqwest::Client,
}

impl LubeLoggerConnector {
    /// Build a connector from environment variables.
    pub fn from_env() -> Self {
        let creds = InfisicalCredentialProvider::new();
        let base_url = creds.get(ENV_LUBELOGGER_URL);
        let api_key = creds.get(ENV_LUBELOGGER_API_KEY);
        let egress = build_egress(&base_url);
        let client = reqwest::Client::new();
        Self { base_url, api_key, egress, client }
    }

    fn url(&self) -> Result<&str> {
        self.base_url.as_deref().ok_or_else(|| {
            LuminaError::Config(format!("{} is not set", ENV_LUBELOGGER_URL))
        })
    }

    fn key(&self) -> Result<&str> {
        self.api_key.as_deref().ok_or_else(|| {
            LuminaError::Config(format!("{} is not set", ENV_LUBELOGGER_API_KEY))
        })
    }
}

#[async_trait]
impl ServiceConnector for LubeLoggerConnector {
    fn name(&self) -> &str {
        "lubelogger"
    }

    fn is_configured(&self) -> bool {
        self.base_url.is_some() && self.api_key.is_some()
    }

    async fn health_check(&self) -> Result<bool> {
        let base = self.url()?;
        // LubeLogger API root returns vehicle list when authenticated
        let endpoint = format!("{}/api/vehicles", base);

        self.egress.inspect(&endpoint, "lubelogger_health_check")
            .map_err(LuminaError::from)?;

        let resp = self.client
            .get(&endpoint)
            .header("X-API-Key", self.key()?)
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
                "vehicle_status".to_string(),
                "Get a status summary for one or all vehicles tracked in LubeLogger, \
                 including mileage, last service date, and any active reminders."
                    .to_string(),
                json!({
                    "type": "object",
                    "properties": {
                        "vehicle_id": {
                            "type": "integer",
                            "description": "LubeLogger vehicle ID (omit to return all vehicles)"
                        }
                    },
                    "additionalProperties": false
                }),
            ),
            ToolDefinition::read_only(
                "maintenance_due".to_string(),
                "List maintenance items that are overdue or coming due within the \
                 specified number of days or miles."
                    .to_string(),
                json!({
                    "type": "object",
                    "properties": {
                        "vehicle_id": {
                            "type": "integer",
                            "description": "Restrict results to a single vehicle (omit for all)"
                        },
                        "within_days": {
                            "type": "integer",
                            "description": "Include items due within this many days (default 30)",
                            "minimum": 0,
                            "maximum": 365
                        },
                        "within_miles": {
                            "type": "integer",
                            "description": "Include items due within this many miles (default 500)",
                            "minimum": 0
                        },
                        "include_overdue": {
                            "type": "boolean",
                            "description": "Include already-overdue items (default true)"
                        }
                    },
                    "additionalProperties": false
                }),
            ),
            ToolDefinition::new(
                "log_fuel".to_string(),
                "Log a fuel-up event for a vehicle in LubeLogger, recording date, \
                 odometer reading, litres/gallons filled, and price paid."
                    .to_string(),
                ToolPermission::ReadWrite,
                json!({
                    "type": "object",
                    "required": ["vehicle_id", "odometer", "fuel_amount"],
                    "properties": {
                        "vehicle_id": {
                            "type": "integer",
                            "description": "LubeLogger vehicle ID"
                        },
                        "odometer": {
                            "type": "number",
                            "description": "Current odometer reading at fill-up"
                        },
                        "fuel_amount": {
                            "type": "number",
                            "description": "Amount of fuel added (in the vehicle's configured unit)"
                        },
                        "cost": {
                            "type": "number",
                            "description": "Total cost of the fill-up (optional)"
                        },
                        "date": {
                            "type": "string",
                            "description": "Date of fill-up in YYYY-MM-DD format (defaults to today)"
                        },
                        "notes": {
                            "type": "string",
                            "description": "Optional notes (e.g. station name, fuel grade)"
                        },
                        "is_full_tank": {
                            "type": "boolean",
                            "description": "Whether this was a full tank fill-up (default true)"
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
            "vehicle_status" => {
                let base = self.url()?;
                let endpoint = format!("{}/api/vehicles", base);
                self.egress.inspect(&endpoint, tool_name)
                    .map_err(LuminaError::from)?;
                let resp = self.client
                    .get(&endpoint)
                    .header("X-API-Key", self.key()?)
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
                        format!("LubeLogger returned HTTP {}", r.status()),
                    )),
                    Err(e) => Ok(ToolResult::error(
                        tool_call_id.to_string(),
                        tool_name.to_string(),
                        format!("Request failed: {}", e),
                    )),
                }
            }
            "maintenance_due" => {
                let base = self.url()?;
                let endpoint = format!("{}/api/reminders", base);
                self.egress.inspect(&endpoint, tool_name)
                    .map_err(LuminaError::from)?;
                let resp = self.client
                    .get(&endpoint)
                    .header("X-API-Key", self.key()?)
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
                        format!("LubeLogger returned HTTP {}", r.status()),
                    )),
                    Err(e) => Ok(ToolResult::error(
                        tool_call_id.to_string(),
                        tool_name.to_string(),
                        format!("Request failed: {}", e),
                    )),
                }
            }
            "log_fuel" => {
                let base = self.url()?;
                let vehicle_id = args.get("vehicle_id")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let endpoint = format!("{}/api/vehicles/{}/gasrecords/add", base, vehicle_id);
                self.egress.inspect(&endpoint, tool_name)
                    .map_err(LuminaError::from)?;
                let resp = self.client
                    .post(&endpoint)
                    .header("X-API-Key", self.key()?)
                    .json(args)
                    .send()
                    .await;
                match resp {
                    Ok(r) if r.status().is_success() => {
                        Ok(ToolResult::success(
                            tool_call_id.to_string(),
                            tool_name.to_string(),
                            "Fuel log entry created".to_string(),
                        ))
                    }
                    Ok(r) => Ok(ToolResult::error(
                        tool_call_id.to_string(),
                        tool_name.to_string(),
                        format!("LubeLogger returned HTTP {}", r.status()),
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
                format!("LubeLoggerConnector does not provide tool '{}'", tool_name),
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
        std::env::set_var(ENV_LUBELOGGER_URL, "http://lubelogger.local:8080");
        std::env::set_var(ENV_LUBELOGGER_API_KEY, "ll-test-key");
    }

    fn clear_creds() {
        std::env::remove_var(ENV_LUBELOGGER_URL);
        std::env::remove_var(ENV_LUBELOGGER_API_KEY);
    }

    #[test]
    fn test_lubelogger_configured_when_both_vars_set() {
        let _g = ENV_LOCK.lock().unwrap();
        set_valid_creds();
        assert!(LubeLoggerConnector::from_env().is_configured());
        clear_creds();
    }

    #[test]
    #[serial]
    fn test_lubelogger_not_configured_when_url_missing() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var(ENV_LUBELOGGER_URL);
        std::env::set_var(ENV_LUBELOGGER_API_KEY, "some-key");
        assert!(!LubeLoggerConnector::from_env().is_configured());
        clear_creds();
    }

    #[test]
    #[serial]
    fn test_lubelogger_not_configured_when_key_missing() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var(ENV_LUBELOGGER_URL, "http://lubelogger.local");
        std::env::remove_var(ENV_LUBELOGGER_API_KEY);
        assert!(!LubeLoggerConnector::from_env().is_configured());
        clear_creds();
    }

    #[test]
    fn test_lubelogger_name() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_creds();
        assert_eq!(LubeLoggerConnector::from_env().name(), "lubelogger");
    }

    #[test]
    fn test_lubelogger_tools_count() {
        let _g = ENV_LOCK.lock().unwrap();
        set_valid_creds();
        assert_eq!(LubeLoggerConnector::from_env().tools().len(), 3);
        clear_creds();
    }

    #[test]
    fn test_lubelogger_tool_names() {
        let _g = ENV_LOCK.lock().unwrap();
        set_valid_creds();
        let connector = LubeLoggerConnector::from_env();
        let tools = connector.tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"vehicle_status"));
        assert!(names.contains(&"maintenance_due"));
        assert!(names.contains(&"log_fuel"));
        clear_creds();
    }

    #[test]
    fn test_lubelogger_log_fuel_is_read_write() {
        let _g = ENV_LOCK.lock().unwrap();
        set_valid_creds();
        let tool = LubeLoggerConnector::from_env()
            .tools()
            .into_iter()
            .find(|t| t.name == "log_fuel")
            .unwrap();
        assert_eq!(tool.permission, ToolPermission::ReadWrite);
        clear_creds();
    }

    #[test]
    #[serial]
    fn test_lubelogger_tool_schemas_have_no_credentials() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var(ENV_LUBELOGGER_URL, "http://secret-lube.internal");
        std::env::set_var(ENV_LUBELOGGER_API_KEY, "top-secret-lube-key");
        let connector = LubeLoggerConnector::from_env();
        for tool in connector.tools() {
            let s = tool.argument_schema.to_string();
            assert!(!s.contains("secret-lube"), "URL must not be in schema");
            assert!(!s.contains("top-secret-lube-key"), "API key must not be in schema");
        }
        clear_creds();
    }

    #[tokio::test]
    async fn test_execute_unknown_tool_returns_error_result() {
        let _g = ENV_LOCK.lock().unwrap();
        set_valid_creds();
        let connector = LubeLoggerConnector::from_env();
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
