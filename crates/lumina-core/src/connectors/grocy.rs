//! Grocy service connector
//!
//! Grocy is a self-hosted grocery and household management system.
//! This connector exposes three MCP tools:
//!
//! | Tool name           | Permission  | Description                           |
//! |---------------------|-------------|---------------------------------------|
//! | `grocy_stock`       | ReadOnly    | Query current stock levels            |
//! | `grocy_add`         | ReadWrite   | Add/update a stock item               |
//! | `grocy_shopping_list` | ReadWrite | Manage the shopping list              |
//!
//! # Required environment variables
//! | Variable      | Description                                   |
//! |---------------|-----------------------------------------------|
//! | `GROCY_URL`   | Base URL of the Grocy instance (no trailing /) |
//! | `GROCY_API_KEY` | Grocy API token                             |
//!
//! Both variables must be present and non-empty for the connector to be active.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::connectors::{build_egress, InfisicalCredentialProvider, ServiceConnector};
use crate::egress_inspector::EgressInspector;
use crate::error::{LuminaError, Result};
use crate::tool_types::{ToolDefinition, ToolPermission, ToolResult};

// ─────────────────────────────────────────────────────────────────────────────
// Env var names (no values hardcoded here — all resolved at runtime)
// ─────────────────────────────────────────────────────────────────────────────

const ENV_GROCY_URL: &str = "GROCY_URL";
const ENV_GROCY_API_KEY: &str = "GROCY_API_KEY";

// ─────────────────────────────────────────────────────────────────────────────
// GrocyConnector
// ─────────────────────────────────────────────────────────────────────────────

/// Connector for the Grocy household management service.
pub struct GrocyConnector {
    /// Resolved base URL (e.g. `http://192.0.2.50:9283`).
    /// `None` when the env var is absent or empty.
    base_url: Option<String>,
    /// API key token.
    /// `None` when the env var is absent or empty.
    api_key: Option<String>,
    /// Egress inspector — every outbound request is pre-checked.
    egress: EgressInspector,
    /// Reused across all requests — avoids allocating a new connection pool
    /// and TLS context on every health check or tool call.
    client: reqwest::Client,
}

impl GrocyConnector {
    /// Build a connector by reading credentials from the environment.
    ///
    /// Construction never fails; missing vars simply result in
    /// `is_configured() == false`.
    pub fn from_env() -> Self {
        let creds = InfisicalCredentialProvider::new();
        let base_url = creds.get(ENV_GROCY_URL);
        let api_key = creds.get(ENV_GROCY_API_KEY);

        // Build the egress allowlist from the configured URL's host (if present).
        let egress = build_egress(&base_url);
        let client = reqwest::Client::new();

        Self { base_url, api_key, egress, client }
    }

    /// Retrieve the base URL, returning a config error if not set.
    fn url(&self) -> Result<&str> {
        self.base_url.as_deref().ok_or_else(|| {
            LuminaError::Config(format!("{} is not set", ENV_GROCY_URL))
        })
    }

    /// Retrieve the API key, returning a config error if not set.
    fn key(&self) -> Result<&str> {
        self.api_key.as_deref().ok_or_else(|| {
            LuminaError::Config(format!("{} is not set", ENV_GROCY_API_KEY))
        })
    }
}

#[async_trait]
impl ServiceConnector for GrocyConnector {
    fn name(&self) -> &str {
        "grocy"
    }

    fn is_configured(&self) -> bool {
        self.base_url.is_some() && self.api_key.is_some()
    }

    async fn health_check(&self) -> Result<bool> {
        let base = self.url()?;
        let endpoint = format!("{}/api/system/info", base);

        self.egress.inspect(&endpoint, "grocy_health_check")
            .map_err(LuminaError::from)?;

        let resp = self.client
            .get(&endpoint)
            .header("GROCY-API-KEY", self.key()?)
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
                "grocy_stock".to_string(),
                "Query current stock levels in Grocy. Returns product names, quantities, \
                 and expiry information. Optionally filter by product name or barcode."
                    .to_string(),
                json!({
                    "type": "object",
                    "properties": {
                        "product_name": {
                            "type": "string",
                            "description": "Optional product name filter (case-insensitive substring match)"
                        },
                        "barcode": {
                            "type": "string",
                            "description": "Optional EAN/UPC barcode to look up"
                        },
                        "only_low_stock": {
                            "type": "boolean",
                            "description": "When true, return only items below their minimum stock quantity"
                        }
                    },
                    "additionalProperties": false
                }),
            ),
            ToolDefinition::read_write(
                "grocy_add".to_string(),
                "Add or update stock for a product in Grocy. Creates a stock entry \
                 recording the quantity added and optional price/location."
                    .to_string(),
                json!({
                    "type": "object",
                    "required": ["product_id", "quantity"],
                    "properties": {
                        "product_id": {
                            "type": "integer",
                            "description": "Grocy internal product ID"
                        },
                        "quantity": {
                            "type": "number",
                            "description": "Amount to add to stock (must be positive)"
                        },
                        "price": {
                            "type": "number",
                            "description": "Unit price paid (optional)"
                        },
                        "location_id": {
                            "type": "integer",
                            "description": "Storage location ID (optional)"
                        },
                        "best_before_date": {
                            "type": "string",
                            "description": "Best-before date in YYYY-MM-DD format (optional)"
                        }
                    },
                    "additionalProperties": false
                }),
            ),
            ToolDefinition::new(
                "grocy_shopping_list".to_string(),
                "Manage the Grocy shopping list. Supports listing current items, \
                 adding an item, or clearing completed items."
                    .to_string(),
                ToolPermission::ReadWrite,
                json!({
                    "type": "object",
                    "required": ["action"],
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": ["list", "add", "clear_done"],
                            "description": "Operation to perform on the shopping list"
                        },
                        "product_id": {
                            "type": "integer",
                            "description": "Product ID (required for 'add' action)"
                        },
                        "quantity": {
                            "type": "number",
                            "description": "Quantity to add (required for 'add' action)"
                        },
                        "note": {
                            "type": "string",
                            "description": "Optional free-text note to attach to the item"
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
            "grocy_stock" => {
                let base = self.url()?;
                let endpoint = format!("{}/api/stock", base);
                self.egress.inspect(&endpoint, tool_name)
                    .map_err(LuminaError::from)?;
                let resp = self.client
                    .get(&endpoint)
                    .header("GROCY-API-KEY", self.key()?)
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
                        format!("Grocy returned HTTP {}", r.status()),
                    )),
                    Err(e) => Ok(ToolResult::error(
                        tool_call_id.to_string(),
                        tool_name.to_string(),
                        format!("Request failed: {}", e),
                    )),
                }
            }
            "grocy_add" => {
                let base = self.url()?;
                let product_id = args.get("product_id")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let quantity = args.get("quantity")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let endpoint = format!("{}/api/stock/products/{}/add", base, product_id);
                self.egress.inspect(&endpoint, tool_name)
                    .map_err(LuminaError::from)?;
                let body = json!({"amount": quantity});
                let resp = self.client
                    .post(&endpoint)
                    .header("GROCY-API-KEY", self.key()?)
                    .json(&body)
                    .send()
                    .await;
                match resp {
                    Ok(r) if r.status().is_success() => {
                        Ok(ToolResult::success(
                            tool_call_id.to_string(),
                            tool_name.to_string(),
                            format!("Added {} units of product {}", quantity, product_id),
                        ))
                    }
                    Ok(r) => Ok(ToolResult::error(
                        tool_call_id.to_string(),
                        tool_name.to_string(),
                        format!("Grocy returned HTTP {}", r.status()),
                    )),
                    Err(e) => Ok(ToolResult::error(
                        tool_call_id.to_string(),
                        tool_name.to_string(),
                        format!("Request failed: {}", e),
                    )),
                }
            }
            "grocy_shopping_list" => {
                let base = self.url()?;
                let endpoint = format!("{}/api/shoppinglist", base);
                self.egress.inspect(&endpoint, tool_name)
                    .map_err(LuminaError::from)?;
                let resp = self.client
                    .get(&endpoint)
                    .header("GROCY-API-KEY", self.key()?)
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
                        format!("Grocy returned HTTP {}", r.status()),
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
                format!("GrocyConnector does not provide tool '{}'", tool_name),
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
        std::env::set_var(ENV_GROCY_URL, "http://grocy.local:9283");
        std::env::set_var(ENV_GROCY_API_KEY, "test-key-abc123");
    }

    fn clear_creds() {
        std::env::remove_var(ENV_GROCY_URL);
        std::env::remove_var(ENV_GROCY_API_KEY);
    }

    #[test]
    fn test_grocy_connector_is_configured_when_both_vars_set() {
        let _g = ENV_LOCK.lock().unwrap();
        set_valid_creds();
        let c = GrocyConnector::from_env();
        assert!(c.is_configured());
        clear_creds();
    }

    #[test]
    #[serial]
    fn test_grocy_connector_not_configured_when_url_missing() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var(ENV_GROCY_URL);
        std::env::set_var(ENV_GROCY_API_KEY, "somekey");
        let c = GrocyConnector::from_env();
        assert!(!c.is_configured());
        clear_creds();
    }

    #[test]
    #[serial]
    fn test_grocy_connector_not_configured_when_key_missing() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var(ENV_GROCY_URL, "http://grocy.local");
        std::env::remove_var(ENV_GROCY_API_KEY);
        let c = GrocyConnector::from_env();
        assert!(!c.is_configured());
        clear_creds();
    }

    #[test]
    fn test_grocy_name() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_creds();
        let c = GrocyConnector::from_env();
        assert_eq!(c.name(), "grocy");
    }

    #[test]
    fn test_grocy_tools_count() {
        let _g = ENV_LOCK.lock().unwrap();
        set_valid_creds();
        let c = GrocyConnector::from_env();
        let tools = c.tools();
        assert_eq!(tools.len(), 3, "GrocyConnector must expose exactly 3 tools");
        clear_creds();
    }

    #[test]
    fn test_grocy_tool_names() {
        let _g = ENV_LOCK.lock().unwrap();
        set_valid_creds();
        let c = GrocyConnector::from_env();
        let tools = c.tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"grocy_stock"));
        assert!(names.contains(&"grocy_add"));
        assert!(names.contains(&"grocy_shopping_list"));
        clear_creds();
    }

    #[test]
    fn test_grocy_tool_permissions() {
        let _g = ENV_LOCK.lock().unwrap();
        set_valid_creds();
        let c = GrocyConnector::from_env();
        for tool in c.tools() {
            match tool.name.as_str() {
                "grocy_stock" => assert_eq!(tool.permission, ToolPermission::ReadOnly),
                "grocy_add" | "grocy_shopping_list" => {
                    assert_eq!(tool.permission, ToolPermission::ReadWrite)
                }
                _ => {}
            }
        }
        clear_creds();
    }

    #[test]
    #[serial]
    fn test_grocy_tools_schema_has_no_credentials() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var(ENV_GROCY_URL, "http://super-secret-grocy.internal");
        std::env::set_var(ENV_GROCY_API_KEY, "topsecret-apikey-xyz");
        let c = GrocyConnector::from_env();
        for tool in c.tools() {
            let s = tool.argument_schema.to_string();
            assert!(!s.contains("super-secret-grocy"), "URL must not appear in schema");
            assert!(!s.contains("topsecret-apikey"), "API key must not appear in schema");
        }
        clear_creds();
    }

    #[tokio::test]
    async fn test_execute_unknown_tool_returns_error_result() {
        let _g = ENV_LOCK.lock().unwrap();
        set_valid_creds();
        let connector = GrocyConnector::from_env();
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
