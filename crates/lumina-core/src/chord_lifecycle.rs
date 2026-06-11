//! FORGE-04: Single-VRAM lifecycle — swap and restore GPU model around deep requests.
//!
//! When the router escalates to the 120B model:
//!   1. POST /api/lifecycle/swap  — loads 120B, evicts 20B
//!   2. Run the LLM request
//!   3. POST /api/lifecycle/restore — reloads 20B
//!
//! Both calls are best-effort: failures are logged but never abort the LLM request.

use crate::error::{LuminaError, Result};
use reqwest::Client;
use std::time::Duration;

pub struct LifecycleClient {
    client: Client,
    control_url: String,
    api_key: String,
    // Actual Ollama model name (not a Chord alias) loaded on swap.
    pub(crate) deep_model: String,
    engine: String,
}

impl LifecycleClient {
    pub fn new(
        control_url: String,
        api_key: String,
        deep_model: String,
        engine: String,
        swap_timeout: Duration,
    ) -> Self {
        let client = Client::builder()
            .timeout(swap_timeout)
            .build()
            .expect("Failed to create lifecycle HTTP client");
        Self { client, control_url, api_key, deep_model, engine }
    }

    /// Build from environment. Returns `None` when either `CHORD_CONTROL_URL` or
    /// `CHORD_API_KEY` is unset, disabling the lifecycle feature entirely.
    pub fn from_env() -> Option<Self> {
        let control_url = std::env::var("CHORD_CONTROL_URL")
            .ok()
            .filter(|s| !s.is_empty())?;
        let api_key = std::env::var("CHORD_API_KEY")
            .ok()
            .filter(|s| !s.is_empty())?;

        let deep_model = std::env::var("CHORD_DEEP_MODEL")
            .unwrap_or_else(|_| "gpt-oss:120b".to_string());
        let engine = std::env::var("CHORD_SWAP_ENGINE")
            .unwrap_or_else(|_| "ollama_gpu".to_string());
        let timeout_secs: u64 = std::env::var("CHORD_SWAP_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30);

        Some(Self::new(
            control_url,
            api_key,
            deep_model,
            engine,
            Duration::from_secs(timeout_secs),
        ))
    }

    /// `POST /api/lifecycle/swap` — loads the deep model into VRAM.
    ///
    /// Returns `Ok(())` on 2xx, `Err` on network failure or non-2xx status.
    pub async fn swap_to_deep(&self) -> Result<()> {
        let url = format!("{}/api/lifecycle/swap", self.control_url);
        let body = serde_json::json!({
            "model": self.deep_model,
            "engine": self.engine,
        });

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| LuminaError::Chord(format!("lifecycle swap: {}", e)))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            Err(LuminaError::Chord(format!("lifecycle swap HTTP {}", resp.status())))
        }
    }

    /// `POST /api/lifecycle/restore` — signals Chord to reload the default 20B model.
    ///
    /// Returns `Ok(())` on 2xx, `Err` on network failure or non-2xx status.
    pub async fn restore(&self) -> Result<()> {
        let url = format!("{}/api/lifecycle/restore", self.control_url);

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await
            .map_err(|e| LuminaError::Chord(format!("lifecycle restore: {}", e)))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            Err(LuminaError::Chord(format!("lifecycle restore HTTP {}", resp.status())))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use httpmock::prelude::*;
    use serde_json::json;

    fn clear_lifecycle_env() {
        std::env::remove_var("CHORD_CONTROL_URL");
        std::env::remove_var("CHORD_API_KEY");
        std::env::remove_var("CHORD_DEEP_MODEL");
        std::env::remove_var("CHORD_SWAP_ENGINE");
        std::env::remove_var("CHORD_SWAP_TIMEOUT_SECS");
    }

    #[test]
    #[serial]
    fn test_from_env_none_when_url_missing() {
        clear_lifecycle_env();
        std::env::set_var("CHORD_API_KEY", "test-key");
        assert!(LifecycleClient::from_env().is_none());
        clear_lifecycle_env();
    }

    #[test]
    #[serial]
    fn test_from_env_none_when_key_missing() {
        clear_lifecycle_env();
        std::env::set_var("CHORD_CONTROL_URL", "http://localhost:8090");
        assert!(LifecycleClient::from_env().is_none());
        clear_lifecycle_env();
    }

    #[test]
    #[serial]
    fn test_from_env_none_when_both_missing() {
        clear_lifecycle_env();
        assert!(LifecycleClient::from_env().is_none());
    }

    #[test]
    #[serial]
    fn test_from_env_some_with_defaults() {
        // Inline — avoids race with parallel clear_lifecycle_env() callers
        std::env::set_var("CHORD_CONTROL_URL", "http://localhost:8090");
        std::env::set_var("CHORD_API_KEY", "test-key");
        std::env::remove_var("CHORD_DEEP_MODEL");
        std::env::remove_var("CHORD_SWAP_ENGINE");
        std::env::remove_var("CHORD_SWAP_TIMEOUT_SECS");

        let lc = LifecycleClient::from_env();
        assert!(lc.is_some());
        let lc = lc.unwrap();
        assert_eq!(lc.deep_model, "gpt-oss:120b");
        assert_eq!(lc.engine, "ollama_gpu");
        clear_lifecycle_env();
    }

    #[test]
    #[serial]
    fn test_from_env_custom_model_and_engine() {
        std::env::set_var("CHORD_CONTROL_URL", "http://localhost:8090");
        std::env::set_var("CHORD_API_KEY", "test-key");
        std::env::set_var("CHORD_DEEP_MODEL", "custom:200b");
        std::env::set_var("CHORD_SWAP_ENGINE", "ollama_cpu");

        let lc = LifecycleClient::from_env().unwrap();
        assert_eq!(lc.deep_model, "custom:200b");
        assert_eq!(lc.engine, "ollama_cpu");
        clear_lifecycle_env();
    }

    #[tokio::test]
    async fn test_swap_to_deep_success() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/api/lifecycle/swap")
                .header("Authorization", "Bearer test-key")
                .json_body(json!({"model": "gpt-oss:120b", "engine": "ollama_gpu"}));
            then.status(200);
        });

        let lc = LifecycleClient::new(
            server.base_url(),
            "test-key".to_string(),
            "gpt-oss:120b".to_string(),
            "ollama_gpu".to_string(),
            Duration::from_secs(10),
        );

        let result = lc.swap_to_deep().await;
        assert!(result.is_ok(), "swap should succeed on 200: {:?}", result);
        mock.assert();
    }

    #[tokio::test]
    async fn test_swap_to_deep_server_error() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/api/lifecycle/swap");
            then.status(503);
        });

        let lc = LifecycleClient::new(
            server.base_url(),
            "key".to_string(),
            "gpt-oss:120b".to_string(),
            "ollama_gpu".to_string(),
            Duration::from_secs(10),
        );

        let result = lc.swap_to_deep().await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("503"), "should include status code");
        mock.assert();
    }

    #[tokio::test]
    async fn test_restore_success() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/api/lifecycle/restore")
                .header("Authorization", "Bearer key");
            then.status(200);
        });

        let lc = LifecycleClient::new(
            server.base_url(),
            "key".to_string(),
            "gpt-oss:120b".to_string(),
            "ollama_gpu".to_string(),
            Duration::from_secs(10),
        );

        assert!(lc.restore().await.is_ok());
        mock.assert();
    }

    #[tokio::test]
    async fn test_restore_server_error() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/api/lifecycle/restore");
            then.status(500);
        });

        let lc = LifecycleClient::new(
            server.base_url(),
            "key".to_string(),
            "gpt-oss:120b".to_string(),
            "ollama_gpu".to_string(),
            Duration::from_secs(10),
        );

        assert!(lc.restore().await.is_err());
        mock.assert();
    }

    #[tokio::test]
    async fn test_swap_sends_correct_body_for_custom_model() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/api/lifecycle/swap")
                .json_body(json!({"model": "custom:99b", "engine": "ollama_cpu"}));
            then.status(200);
        });

        let lc = LifecycleClient::new(
            server.base_url(),
            "key".to_string(),
            "custom:99b".to_string(),
            "ollama_cpu".to_string(),
            Duration::from_secs(10),
        );

        lc.swap_to_deep().await.unwrap();
        mock.assert();
    }
}
