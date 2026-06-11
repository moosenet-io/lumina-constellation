//! Jellyseerr service connector
//!
//! Jellyseerr is a self-hosted media request management application
//! (companion to Jellyfin/Plex).
//! This connector exposes three MCP tools:
//!
//! | Tool name        | Permission | Description                               |
//! |------------------|------------|-------------------------------------------|
//! | `media_request`  | ReadWrite  | Submit a request for a movie or TV series |
//! | `media_status`   | ReadOnly   | Check the status of a media request       |
//! | `trending_media` | ReadOnly   | List currently trending movies or shows   |
//!
//! # Required environment variables
//! | Variable              | Description                                    |
//! |-----------------------|------------------------------------------------|
//! | `JELLYSEERR_URL`      | Base URL of the Jellyseerr instance            |
//! | `JELLYSEERR_API_KEY`  | API key (from Jellyseerr Settings → API Key)   |
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

const ENV_JELLYSEERR_URL: &str = "JELLYSEERR_URL";
const ENV_JELLYSEERR_API_KEY: &str = "JELLYSEERR_API_KEY";

// ─────────────────────────────────────────────────────────────────────────────
// JellyseerrConnector
// ─────────────────────────────────────────────────────────────────────────────

/// Connector for the Jellyseerr media request management service.
pub struct JellyseerrConnector {
    base_url: Option<String>,
    api_key: Option<String>,
    egress: EgressInspector,
    /// Reused across all requests — avoids allocating a new connection pool
    /// and TLS context on every health check or tool call.
    client: reqwest::Client,
}

impl JellyseerrConnector {
    /// Build a connector from environment variables.
    pub fn from_env() -> Self {
        let creds = InfisicalCredentialProvider::new();
        let base_url = creds.get(ENV_JELLYSEERR_URL);
        let api_key = creds.get(ENV_JELLYSEERR_API_KEY);
        let egress = build_egress(&base_url);
        let client = reqwest::Client::new();
        Self { base_url, api_key, egress, client }
    }

    fn url(&self) -> Result<&str> {
        self.base_url.as_deref().ok_or_else(|| {
            LuminaError::Config(format!("{} is not set", ENV_JELLYSEERR_URL))
        })
    }

    fn key(&self) -> Result<&str> {
        self.api_key.as_deref().ok_or_else(|| {
            LuminaError::Config(format!("{} is not set", ENV_JELLYSEERR_API_KEY))
        })
    }
}

#[async_trait]
impl ServiceConnector for JellyseerrConnector {
    fn name(&self) -> &str {
        "jellyseerr"
    }

    fn is_configured(&self) -> bool {
        self.base_url.is_some() && self.api_key.is_some()
    }

    async fn health_check(&self) -> Result<bool> {
        let base = self.url()?;
        let endpoint = format!("{}/api/v1/status", base);

        self.egress.inspect(&endpoint, "jellyseerr_health_check")
            .map_err(LuminaError::from)?;

        let resp = self.client
            .get(&endpoint)
            .header("X-Api-Key", self.key()?)
            .send()
            .await;

        match resp {
            Ok(r) => Ok(r.status().is_success()),
            Err(_) => Ok(false),
        }
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition::new(
                "media_request".to_string(),
                "Submit a request to Jellyseerr for a movie or TV series. \
                 The media item will be queued for download once approved."
                    .to_string(),
                ToolPermission::ReadWrite,
                json!({
                    "type": "object",
                    "required": ["media_type", "tmdb_id"],
                    "properties": {
                        "media_type": {
                            "type": "string",
                            "enum": ["movie", "tv"],
                            "description": "Type of media to request"
                        },
                        "tmdb_id": {
                            "type": "integer",
                            "description": "The Movie Database (TMDB) ID for the title"
                        },
                        "seasons": {
                            "type": "array",
                            "items": {"type": "integer"},
                            "description": "For TV requests: specific season numbers to request \
                                           (omit to request all available seasons)"
                        },
                        "is_4k": {
                            "type": "boolean",
                            "description": "Request 4K version if available (default false)"
                        }
                    },
                    "additionalProperties": false
                }),
            ),
            ToolDefinition::read_only(
                "media_status".to_string(),
                "Check the current status of a media item in Jellyseerr — whether \
                 it has been requested, approved, available, or declined."
                    .to_string(),
                json!({
                    "type": "object",
                    "required": ["tmdb_id"],
                    "properties": {
                        "tmdb_id": {
                            "type": "integer",
                            "description": "The Movie Database (TMDB) ID for the title"
                        },
                        "media_type": {
                            "type": "string",
                            "enum": ["movie", "tv"],
                            "description": "Type of media (helps resolve ambiguous IDs)"
                        }
                    },
                    "additionalProperties": false
                }),
            ),
            ToolDefinition::read_only(
                "trending_media".to_string(),
                "List currently trending movies or TV shows from Jellyseerr, \
                 sourced from TMDB trending data."
                    .to_string(),
                json!({
                    "type": "object",
                    "properties": {
                        "media_type": {
                            "type": "string",
                            "enum": ["movie", "tv", "all"],
                            "description": "Filter by media type (default 'all')"
                        },
                        "page": {
                            "type": "integer",
                            "description": "Page number for pagination (default 1)",
                            "minimum": 1
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Number of results per page (default 20, max 50)",
                            "minimum": 1,
                            "maximum": 50
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
            "media_request" => {
                let base = self.url()?;
                let endpoint = format!("{}/api/v1/request", base);
                self.egress.inspect(&endpoint, tool_name)
                    .map_err(LuminaError::from)?;
                let resp = self.client
                    .post(&endpoint)
                    .header("X-Api-Key", self.key()?)
                    .json(args)
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
                        format!("Jellyseerr returned HTTP {}", r.status()),
                    )),
                    Err(e) => Ok(ToolResult::error(
                        tool_call_id.to_string(),
                        tool_name.to_string(),
                        format!("Request failed: {}", e),
                    )),
                }
            }
            "media_status" => {
                let base = self.url()?;
                let tmdb_id = args.get("tmdb_id").and_then(|v| v.as_u64()).unwrap_or(0);
                let endpoint = format!("{}/api/v1/movie/{}", base, tmdb_id);
                self.egress.inspect(&endpoint, tool_name)
                    .map_err(LuminaError::from)?;
                let resp = self.client
                    .get(&endpoint)
                    .header("X-Api-Key", self.key()?)
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
                        format!("Jellyseerr returned HTTP {}", r.status()),
                    )),
                    Err(e) => Ok(ToolResult::error(
                        tool_call_id.to_string(),
                        tool_name.to_string(),
                        format!("Request failed: {}", e),
                    )),
                }
            }
            "trending_media" => {
                let base = self.url()?;
                let endpoint = format!("{}/api/v1/discover/trending", base);
                self.egress.inspect(&endpoint, tool_name)
                    .map_err(LuminaError::from)?;
                let resp = self.client
                    .get(&endpoint)
                    .header("X-Api-Key", self.key()?)
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
                        format!("Jellyseerr returned HTTP {}", r.status()),
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
                format!("JellyseerrConnector does not provide tool '{}'", tool_name),
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
        std::env::set_var(ENV_JELLYSEERR_URL, "http://jellyseerr.local:5055");
        std::env::set_var(ENV_JELLYSEERR_API_KEY, "js-test-key-abc");
    }

    fn clear_creds() {
        std::env::remove_var(ENV_JELLYSEERR_URL);
        std::env::remove_var(ENV_JELLYSEERR_API_KEY);
    }

    #[test]
    fn test_jellyseerr_configured_when_both_vars_set() {
        let _g = ENV_LOCK.lock().unwrap();
        set_valid_creds();
        assert!(JellyseerrConnector::from_env().is_configured());
        clear_creds();
    }

    #[test]
    #[serial]
    fn test_jellyseerr_not_configured_when_url_missing() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var(ENV_JELLYSEERR_URL);
        std::env::set_var(ENV_JELLYSEERR_API_KEY, "somekey");
        assert!(!JellyseerrConnector::from_env().is_configured());
        clear_creds();
    }

    #[test]
    #[serial]
    fn test_jellyseerr_not_configured_when_key_missing() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var(ENV_JELLYSEERR_URL, "http://jellyseerr.local");
        std::env::remove_var(ENV_JELLYSEERR_API_KEY);
        assert!(!JellyseerrConnector::from_env().is_configured());
        clear_creds();
    }

    #[test]
    fn test_jellyseerr_name() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_creds();
        assert_eq!(JellyseerrConnector::from_env().name(), "jellyseerr");
    }

    #[test]
    fn test_jellyseerr_tools_count() {
        let _g = ENV_LOCK.lock().unwrap();
        set_valid_creds();
        assert_eq!(JellyseerrConnector::from_env().tools().len(), 3);
        clear_creds();
    }

    #[test]
    fn test_jellyseerr_tool_names() {
        let _g = ENV_LOCK.lock().unwrap();
        set_valid_creds();
        let connector = JellyseerrConnector::from_env();
        let tools = connector.tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"media_request"));
        assert!(names.contains(&"media_status"));
        assert!(names.contains(&"trending_media"));
        clear_creds();
    }

    #[test]
    fn test_jellyseerr_media_request_is_read_write() {
        let _g = ENV_LOCK.lock().unwrap();
        set_valid_creds();
        let tool = JellyseerrConnector::from_env()
            .tools()
            .into_iter()
            .find(|t| t.name == "media_request")
            .unwrap();
        assert_eq!(tool.permission, ToolPermission::ReadWrite);
        clear_creds();
    }

    #[test]
    #[serial]
    fn test_jellyseerr_tool_schemas_have_no_credentials() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var(ENV_JELLYSEERR_URL, "http://secret-jelly.internal");
        std::env::set_var(ENV_JELLYSEERR_API_KEY, "supersecret-jelly-key");
        let connector = JellyseerrConnector::from_env();
        for tool in connector.tools() {
            let s = tool.argument_schema.to_string();
            assert!(!s.contains("secret-jelly"), "URL must not be in schema");
            assert!(!s.contains("supersecret-jelly"), "API key must not be in schema");
        }
        clear_creds();
    }

    #[tokio::test]
    async fn test_execute_unknown_tool_returns_error_result() {
        let _g = ENV_LOCK.lock().unwrap();
        set_valid_creds();
        let connector = JellyseerrConnector::from_env();
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
