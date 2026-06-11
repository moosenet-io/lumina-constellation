//! FORGE-02: Three-layer model router.
//!
//! Decides per-turn whether to use the fast model (20B personality) or the
//! deep model (120B reasoning). Safety principle: when in doubt, escalate.
//!
//! Layer 1: User override  (/deep, /quick, natural language)
//! Layer 2: Category rules  (deterministic, no LLM call)
//! Layer 3: Confidence fallback  (re-routes if fast model is uncertain)
//!
//! FORGE-04: Single-VRAM policy — swap/restore lifecycle wraps every deep call.

use crate::chord::{ChatMessage, ChordClient};
use crate::chord_lifecycle::LifecycleClient;
use crate::error::{LuminaError, Result};
use crate::router_rules;
use crate::secure_string::ZeroizingString;
use crate::users::UserRole;

/// Routing decision produced by Layer 1 + 2.
#[derive(Debug, Clone)]
pub struct RouteDecision {
    /// Model alias to use (e.g. "lumina-fast" or "lumina-deep").
    pub model: String,
    /// Input with any override command stripped.
    pub cleaned_input: String,
    /// Which layer made the decision.
    pub layer: &'static str,
    /// True if Layer 3 (confidence fallback) should be suppressed for this request.
    pub layer3_disabled: bool,
}

/// Final result returned after all three layers complete.
pub struct RouterResult {
    pub model_used: String,
    pub escalated: bool,
    pub decision_layer: String,
    pub response: ZeroizingString,
}

/// Three-layer model router.
pub struct ModelRouter {
    pub fast_model: String,
    pub deep_model: String,
    pub escalation_token_threshold: usize,
    /// Optional lifecycle client for single-VRAM swap/restore around deep calls.
    /// None when CHORD_CONTROL_URL / CHORD_API_KEY are not set (lifecycle disabled).
    lifecycle: Option<LifecycleClient>,
}

impl Default for ModelRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelRouter {
    /// Create a router with no lifecycle (used in tests and stdin mode).
    pub fn new() -> Self {
        Self {
            fast_model: "lumina-fast".to_string(),
            deep_model: "lumina-deep".to_string(),
            escalation_token_threshold: 500,
            lifecycle: None,
        }
    }

    /// Build from environment variables, falling back to defaults.
    ///
    /// Reads: LUMINA_FAST_MODEL, LUMINA_DEEP_MODEL, LUMINA_ESCALATION_THRESHOLD,
    ///        CHORD_CONTROL_URL, CHORD_API_KEY (+ optional CHORD_DEEP_MODEL,
    ///        CHORD_SWAP_ENGINE, CHORD_SWAP_TIMEOUT_SECS) for lifecycle.
    pub fn from_env() -> Self {
        Self {
            fast_model: std::env::var("LUMINA_FAST_MODEL")
                .unwrap_or_else(|_| "lumina-fast".to_string()),
            deep_model: std::env::var("LUMINA_DEEP_MODEL")
                .unwrap_or_else(|_| "lumina-deep".to_string()),
            escalation_token_threshold: std::env::var("LUMINA_ESCALATION_THRESHOLD")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(500),
            lifecycle: LifecycleClient::from_env(),
        }
    }

    /// Layer 1 + Layer 2: Determine which model to use and clean the input.
    ///
    /// Layer 1 (highest priority): slash commands and natural language overrides.
    /// Layer 2: deterministic category rules.
    pub fn route(&self, input: &str) -> Result<RouteDecision> {
        let trimmed = input.trim();

        // --- Layer 1: slash command overrides ---
        if trimmed.starts_with("/deep") {
            let rest = trimmed.trim_start_matches("/deep").trim().to_string();
            if rest.is_empty() {
                return Err(LuminaError::Config(
                    "Please include a message after /deep".to_string(),
                ));
            }
            return Ok(RouteDecision {
                model: self.deep_model.clone(),
                cleaned_input: rest,
                layer: "override",
                layer3_disabled: false,
            });
        }

        if trimmed.starts_with("/quick") {
            let rest = trimmed.trim_start_matches("/quick").trim().to_string();
            if rest.is_empty() {
                return Err(LuminaError::Config(
                    "Please include a message after /quick".to_string(),
                ));
            }
            return Ok(RouteDecision {
                model: self.fast_model.clone(),
                cleaned_input: rest,
                layer: "override",
                layer3_disabled: true, // User explicitly chose fast; skip Layer 3
            });
        }

        let lower = trimmed.to_lowercase();

        // --- Layer 1: natural language overrides ---
        if router_rules::is_natural_deep_override(&lower) {
            return Ok(RouteDecision {
                model: self.deep_model.clone(),
                cleaned_input: trimmed.to_string(),
                layer: "override",
                layer3_disabled: false,
            });
        }

        if router_rules::is_natural_fast_override(&lower) {
            return Ok(RouteDecision {
                model: self.fast_model.clone(),
                cleaned_input: trimmed.to_string(),
                layer: "override",
                layer3_disabled: true,
            });
        }

        // --- Layer 2: category rules ---
        if router_rules::needs_deep_reasoning(trimmed, self.escalation_token_threshold) {
            return Ok(RouteDecision {
                model: self.deep_model.clone(),
                cleaned_input: trimmed.to_string(),
                layer: "category",
                layer3_disabled: false,
            });
        }

        // Default: fast model
        Ok(RouteDecision {
            model: self.fast_model.clone(),
            cleaned_input: trimmed.to_string(),
            layer: "category",
            layer3_disabled: false,
        })
    }

    /// Layer 3: Check if the fast-model response shows uncertainty.
    ///
    /// Returns true if confidence fallback should trigger (escalate to deep model).
    /// Always returns false if layer3 is disabled (e.g. /quick override).
    pub fn should_escalate_on_confidence(&self, response: &str, layer3_disabled: bool) -> bool {
        if layer3_disabled {
            return false;
        }
        router_rules::has_uncertainty_markers(response)
    }

    /// Full routing pipeline: Layers 1 + 2 + 3 with single-VRAM lifecycle.
    ///
    /// Makes one or two calls to ChordClient and returns the final response
    /// with routing metadata. When the deep model is needed, calls the
    /// lifecycle swap/restore endpoints around the LLM request.
    ///
    /// Lifecycle errors are non-fatal: swap failure falls through to whichever
    /// model is currently loaded; restore failure is logged and the 120B stays
    /// in VRAM until the next 20B request triggers a natural swap.
    pub async fn process(&self, input: &str, chord: &ChordClient, system_prompt: &str) -> Result<RouterResult> {
        let decision = self.route(input)?;
        let initial_model = decision.model.clone();
        let layer = decision.layer;
        let layer3_disabled = decision.layer3_disabled;
        let cleaned = decision.cleaned_input.clone();

        let need_deep_first = initial_model == self.deep_model;

        // Swap to 120B if the first call needs the deep model
        if need_deep_first {
            self.swap_for_deep("L1/L2").await;
        }

        // First LLM call — if it fails and we swapped, restore before propagating
        let first_result = chord.chat_completion_with_model(
            &initial_model,
            build_messages(system_prompt, &cleaned),
        ).await;
        if first_result.is_err() && need_deep_first {
            self.restore_after_deep().await;
        }
        let response: ZeroizingString = first_result?;

        // Layer 3: confidence fallback
        if self.should_escalate_on_confidence(&response, layer3_disabled) {
            eprintln!(
                "router: Layer 3 confidence fallback → {}",
                self.deep_model
            );

            // Only swap if we weren't already on deep
            if !need_deep_first {
                self.swap_for_deep("L3").await;
            }

            let deep_result = chord.chat_completion_with_model(
                &self.deep_model,
                build_messages(system_prompt, &cleaned),
            ).await;
            // Always restore after any deep call (success or failure)
            self.restore_after_deep().await;
            let deep_response = deep_result?;

            return Ok(RouterResult {
                model_used: self.deep_model.clone(),
                escalated: true,
                decision_layer: "confidence-fallback".to_string(),
                response: deep_response,
            });
        }

        // Restore if the first call was on the deep model (and no L3 escalation)
        if need_deep_first {
            self.restore_after_deep().await;
        }

        let escalated = initial_model == self.deep_model;
        Ok(RouterResult {
            model_used: initial_model,
            escalated,
            decision_layer: layer.to_string(),
            response,
        })
    }

    /// Like `process()` but accepts a pre-built messages array (with conversation history).
    /// Preserves L1/L2 model routing, L3 confidence fallback, and single-VRAM lifecycle.
    ///
    /// Used by `process_turn_with_session` (P1-05) where the caller has already prepended
    /// system prompt + history window + user message into `messages`.
    pub async fn process_with_messages(
        &self,
        user_input_for_routing: &str,
        messages: Vec<ChatMessage>,
        chord: &ChordClient,
    ) -> Result<RouterResult> {
        let decision = self.route(user_input_for_routing)?;
        self.process_with_messages_and_decision(decision, messages, chord).await
    }

    /// P2-16: Like `process_with_messages` but enforces per-user deep-model budget.
    ///
    /// If the user's deep budget is exhausted (checked via `UserCostTracker`),
    /// the deep model is **silently downgraded** to the fast model — the request
    /// is NOT rejected. Pass `None` for `user_id`/`user_role` to skip the check.
    pub async fn process_with_messages_for_user(
        &self,
        user_input_for_routing: &str,
        messages: Vec<ChatMessage>,
        chord: &ChordClient,
        user_id: Option<&str>,
        user_role: Option<&UserRole>,
    ) -> Result<RouterResult> {
        use crate::users::cost_caps::{BudgetStatus, UserCostTracker, today_utc};

        let mut decision = self.route(user_input_for_routing)?;

        // If a deep-model route was chosen, check the user's deep budget first.
        if decision.model == self.deep_model {
            if let (Some(uid), Some(role)) = (user_id, user_role) {
                let downgrade = match UserCostTracker::open_default() {
                    Ok(tracker) => {
                        match tracker.check_budget(uid, role, true, &today_utc()) {
                            Ok(BudgetStatus::DeepBudgetExhausted) => {
                                eprintln!(
                                    "router: deep budget exhausted for user {uid}; downgrading to {}",
                                    self.fast_model
                                );
                                true
                            }
                            Ok(_) => false,
                            Err(e) => {
                                eprintln!("router: cap check error (non-fatal): {e}");
                                false
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("router: cap tracker unavailable (non-fatal): {e}");
                        false
                    }
                };
                if downgrade {
                    decision.model = self.fast_model.clone();
                    decision.layer3_disabled = true; // skip L3 escalation too
                }
            }
        }

        // Delegate to the standard messages pipeline with the (possibly downgraded) decision.
        self.process_with_messages_and_decision(decision, messages, chord).await
    }

    /// Internal: process with an already-resolved routing decision.
    async fn process_with_messages_and_decision(
        &self,
        decision: RouteDecision,
        messages: Vec<ChatMessage>,
        chord: &ChordClient,
    ) -> Result<RouterResult> {
        let initial_model = decision.model.clone();
        let layer = decision.layer;
        let layer3_disabled = decision.layer3_disabled;

        let need_deep_first = initial_model == self.deep_model;

        if need_deep_first {
            self.swap_for_deep("L1/L2").await;
        }

        let first_result = chord.chat_completion_with_model(&initial_model, messages.clone()).await;
        if first_result.is_err() && need_deep_first {
            self.restore_after_deep().await;
        }
        let response: ZeroizingString = first_result?;

        if self.should_escalate_on_confidence(&response, layer3_disabled) {
            eprintln!("router: Layer 3 confidence fallback → {}", self.deep_model);
            if !need_deep_first {
                self.swap_for_deep("L3").await;
            }
            let deep_result = chord.chat_completion_with_model(&self.deep_model, messages).await;
            self.restore_after_deep().await;
            let deep_response = deep_result?;
            return Ok(RouterResult {
                model_used: self.deep_model.clone(),
                escalated: true,
                decision_layer: "confidence-fallback".to_string(),
                response: deep_response,
            });
        }

        if need_deep_first {
            self.restore_after_deep().await;
        }

        let escalated = initial_model == self.deep_model;
        Ok(RouterResult {
            model_used: initial_model,
            escalated,
            decision_layer: layer.to_string(),
            response,
        })
    }

    /// Swap to the deep model — best-effort, never aborts the request on failure.
    async fn swap_for_deep(&self, context: &str) {
        if let Some(lc) = &self.lifecycle {
            eprintln!("router: lifecycle swap → {} ({})", self.deep_model, context);
            if let Err(e) = lc.swap_to_deep().await {
                eprintln!("router: lifecycle swap failed ({}), using loaded model", e);
            }
        }
    }

    /// Restore the default 20B model — best-effort, non-fatal on failure.
    async fn restore_after_deep(&self) {
        if let Some(lc) = &self.lifecycle {
            if let Err(e) = lc.restore().await {
                eprintln!("router: lifecycle restore failed ({}), 120B stays loaded", e);
            }
        }
    }
}

/// Build the messages array: system prompt first, then the user message.
fn build_messages(system_prompt: &str, user_input: &str) -> Vec<ChatMessage> {
    vec![
        ChatMessage::text("system", system_prompt),
        ChatMessage::text("user", user_input),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn router() -> ModelRouter {
        ModelRouter::new()
    }

    // --- Layer 1 tests ---

    #[test]
    fn test_slash_deep_routes_to_deep_model() {
        let r = router().route("/deep hello world").unwrap();
        assert_eq!(r.model, "lumina-deep");
        assert_eq!(r.cleaned_input, "hello world");
        assert_eq!(r.layer, "override");
        assert!(!r.layer3_disabled);
    }

    #[test]
    fn test_slash_quick_routes_to_fast_model() {
        let r = router().route("/quick what time is it").unwrap();
        assert_eq!(r.model, "lumina-fast");
        assert_eq!(r.cleaned_input, "what time is it");
        assert_eq!(r.layer, "override");
        assert!(r.layer3_disabled);
    }

    #[test]
    fn test_slash_deep_empty_message_errors() {
        let result = router().route("/deep");
        assert!(result.is_err());
        let result2 = router().route("/deep   ");
        assert!(result2.is_err());
    }

    #[test]
    fn test_slash_quick_empty_message_errors() {
        let result = router().route("/quick");
        assert!(result.is_err());
    }

    #[test]
    fn test_natural_deep_override() {
        let r = router().route("think carefully about this architecture decision").unwrap();
        assert_eq!(r.model, "lumina-deep");
        assert_eq!(r.layer, "override");
    }

    #[test]
    fn test_natural_fast_override() {
        let r = router().route("quick answer please, what's 2+2").unwrap();
        assert_eq!(r.model, "lumina-fast");
        assert!(r.layer3_disabled);
    }

    #[test]
    fn test_deep_mid_sentence_not_override() {
        // "/deep" not at start should NOT be treated as override command
        let r = router().route("the ocean is deep and blue").unwrap();
        // Not an override (no slash command, no natural override phrase)
        assert_ne!(r.layer, "override"); // might be category or default
        assert_eq!(r.model, "lumina-fast"); // short casual input stays fast
    }

    // --- Layer 2 tests ---

    #[test]
    fn test_code_escalates() {
        let r = router().route("```rust\nfn main() {}\n```").unwrap();
        assert_eq!(r.model, "lumina-deep");
        assert_eq!(r.layer, "category");
    }

    #[test]
    fn test_multi_step_escalates() {
        let r = router().route("First install the package, then configure it, finally restart").unwrap();
        assert_eq!(r.model, "lumina-deep");
        assert_eq!(r.layer, "category");
    }

    #[test]
    fn test_reasoning_escalates() {
        let r = router().route("analyze the pros and cons of microservices").unwrap();
        assert_eq!(r.model, "lumina-deep");
        assert_eq!(r.layer, "category");
    }

    #[test]
    fn test_casual_stays_fast() {
        let r = router().route("hello how are you today").unwrap();
        assert_eq!(r.model, "lumina-fast");
    }

    #[test]
    fn test_long_input_escalates() {
        let long: String = "word ".repeat(450); // ~585 tokens > 500 threshold
        let r = router().route(&long).unwrap();
        assert_eq!(r.model, "lumina-deep");
    }

    #[test]
    fn test_short_input_stays_fast() {
        let r = router().route("ok thanks").unwrap();
        assert_eq!(r.model, "lumina-fast");
    }

    // --- Layer 3 tests ---

    #[test]
    fn test_layer3_triggers_on_uncertainty() {
        let router = router();
        assert!(router.should_escalate_on_confidence("I'm not sure about this", false));
        assert!(router.should_escalate_on_confidence("I think the answer might be X", false));
        assert!(router.should_escalate_on_confidence("It depends on your use case", false));
    }

    #[test]
    fn test_layer3_does_not_trigger_on_confident_response() {
        let router = router();
        assert!(!router.should_escalate_on_confidence("The answer is 42.", false));
        assert!(!router.should_escalate_on_confidence("Use Postgres for this workload.", false));
    }

    #[test]
    fn test_layer3_disabled_for_quick_override() {
        let router = router();
        // layer3_disabled = true → always returns false regardless of content
        assert!(!router.should_escalate_on_confidence("I'm not sure about this", true));
        assert!(!router.should_escalate_on_confidence("I think it depends", true));
    }

    // --- Layer 1 wins over Layer 2 conflict ---

    #[test]
    fn test_quick_override_beats_code_category() {
        // /quick + code-heavy message → Layer 1 wins, stays on fast model
        let r = router()
            .route("/quick write a function to sort a list in rust")
            .unwrap();
        assert_eq!(r.model, "lumina-fast");
        assert_eq!(r.layer, "override");
        assert!(r.layer3_disabled);
    }

    #[test]
    fn test_router_result_captures_metadata() {
        let decision = router().route("/deep explain why rust is safe").unwrap();
        assert_eq!(decision.model_used(), "lumina-deep");
        assert_eq!(decision.layer, "override");
    }

    // --- Custom model names ---

    #[test]
    fn test_custom_model_names() {
        let router = ModelRouter {
            fast_model: "custom-fast".to_string(),
            deep_model: "custom-deep".to_string(),
            escalation_token_threshold: 500,
            lifecycle: None,
        };
        let r = router.route("/deep hello").unwrap();
        assert_eq!(r.model, "custom-deep");

        let r2 = router.route("hello").unwrap();
        assert_eq!(r2.model, "custom-fast");
    }

    // --- Lifecycle integration tests ---

    #[tokio::test]
    async fn test_lifecycle_swap_and_restore_called_for_deep_route() {
        use httpmock::prelude::*;
        use serde_json::json;
        use crate::chord_lifecycle::LifecycleClient;
        use std::time::Duration;

        let chord_server = MockServer::start();
        let lc_server = MockServer::start();

        let chord_mock = chord_server.mock(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(200).json_body(json!({
                "choices": [{"message": {"role": "assistant", "content": "Deep answer."}}]
            }));
        });
        let swap_mock = lc_server.mock(|when, then| {
            when.method(POST).path("/api/lifecycle/swap");
            then.status(200);
        });
        let restore_mock = lc_server.mock(|when, then| {
            when.method(POST).path("/api/lifecycle/restore");
            then.status(200);
        });

        let chord = crate::chord::ChordClient::new(chord_server.base_url(), "".to_string());
        let lc = LifecycleClient::new(
            lc_server.base_url(),
            "key".to_string(),
            "gpt-oss:120b".to_string(),
            "ollama_gpu".to_string(),
            Duration::from_secs(5),
        );
        let router = ModelRouter {
            fast_model: "lumina-fast".to_string(),
            deep_model: "lumina-deep".to_string(),
            escalation_token_threshold: 500,
            lifecycle: Some(lc),
        };

        let result = router.process("/deep think about this", &chord, "You are Lumina.").await.unwrap();
        assert_eq!(result.model_used, "lumina-deep");
        assert!(result.escalated);
        chord_mock.assert();
        swap_mock.assert();
        restore_mock.assert();
    }

    #[tokio::test]
    async fn test_lifecycle_not_called_for_fast_route() {
        use httpmock::prelude::*;
        use serde_json::json;
        use crate::chord_lifecycle::LifecycleClient;
        use std::time::Duration;

        let chord_server = MockServer::start();
        let lc_server = MockServer::start();

        let chord_mock = chord_server.mock(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(200).json_body(json!({
                "choices": [{"message": {"role": "assistant", "content": "Fast answer here."}}]
            }));
        });
        // swap and restore should NOT be called
        let swap_mock = lc_server.mock(|when, then| {
            when.method(POST).path("/api/lifecycle/swap");
            then.status(200);
        });
        let restore_mock = lc_server.mock(|when, then| {
            when.method(POST).path("/api/lifecycle/restore");
            then.status(200);
        });

        let chord = crate::chord::ChordClient::new(chord_server.base_url(), "".to_string());
        let lc = LifecycleClient::new(
            lc_server.base_url(),
            "key".to_string(),
            "gpt-oss:120b".to_string(),
            "ollama_gpu".to_string(),
            Duration::from_secs(5),
        );
        let router = ModelRouter {
            fast_model: "lumina-fast".to_string(),
            deep_model: "lumina-deep".to_string(),
            escalation_token_threshold: 500,
            lifecycle: Some(lc),
        };

        let result = router.process("hello how are you", &chord, "You are Lumina.").await.unwrap();
        assert_eq!(result.model_used, "lumina-fast");
        assert!(!result.escalated);
        chord_mock.assert();
        // assert zero hits on swap/restore
        swap_mock.assert_hits(0);
        restore_mock.assert_hits(0);
    }

    #[tokio::test]
    async fn test_lifecycle_swap_failure_does_not_abort_request() {
        use httpmock::prelude::*;
        use serde_json::json;
        use crate::chord_lifecycle::LifecycleClient;
        use std::time::Duration;

        let chord_server = MockServer::start();
        let lc_server = MockServer::start();

        // Chord still responds normally
        let chord_mock = chord_server.mock(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(200).json_body(json!({
                "choices": [{"message": {"role": "assistant", "content": "Still responded."}}]
            }));
        });
        // Lifecycle swap returns 503
        let swap_mock = lc_server.mock(|when, then| {
            when.method(POST).path("/api/lifecycle/swap");
            then.status(503);
        });
        let restore_mock = lc_server.mock(|when, then| {
            when.method(POST).path("/api/lifecycle/restore");
            then.status(200);
        });

        let chord = crate::chord::ChordClient::new(chord_server.base_url(), "".to_string());
        let lc = LifecycleClient::new(
            lc_server.base_url(),
            "key".to_string(),
            "gpt-oss:120b".to_string(),
            "ollama_gpu".to_string(),
            Duration::from_secs(5),
        );
        let router = ModelRouter {
            fast_model: "lumina-fast".to_string(),
            deep_model: "lumina-deep".to_string(),
            escalation_token_threshold: 500,
            lifecycle: Some(lc),
        };

        // Swap fails but process() should still succeed
        let result = router.process("/deep answer this", &chord, "You are Lumina.").await;
        assert!(result.is_ok(), "swap failure must not abort the request");
        chord_mock.assert();
        swap_mock.assert();
        restore_mock.assert();
    }

    #[tokio::test]
    async fn test_lifecycle_restore_failure_is_non_fatal() {
        use httpmock::prelude::*;
        use serde_json::json;
        use crate::chord_lifecycle::LifecycleClient;
        use std::time::Duration;

        let chord_server = MockServer::start();
        let lc_server = MockServer::start();

        chord_server.mock(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(200).json_body(json!({
                "choices": [{"message": {"role": "assistant", "content": "Deep response."}}]
            }));
        });
        lc_server.mock(|when, then| {
            when.method(POST).path("/api/lifecycle/swap");
            then.status(200);
        });
        // Restore returns 500
        lc_server.mock(|when, then| {
            when.method(POST).path("/api/lifecycle/restore");
            then.status(500);
        });

        let chord = crate::chord::ChordClient::new(chord_server.base_url(), "".to_string());
        let lc = LifecycleClient::new(
            lc_server.base_url(),
            "key".to_string(),
            "gpt-oss:120b".to_string(),
            "ollama_gpu".to_string(),
            Duration::from_secs(5),
        );
        let router = ModelRouter {
            fast_model: "lumina-fast".to_string(),
            deep_model: "lumina-deep".to_string(),
            escalation_token_threshold: 500,
            lifecycle: Some(lc),
        };

        // Restore fails but result should still be Ok
        let result = router.process("/deep what is 2+2", &chord, "You are Lumina.").await;
        assert!(result.is_ok(), "restore failure must not abort the result");
    }

    // Helper for tests
    impl RouteDecision {
        fn model_used(&self) -> &str {
            &self.model
        }
    }
}
