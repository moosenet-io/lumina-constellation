//! Shared LLM seam for sleep-time consolidation (DPROMPT-03/04/13/16).
//!
//! Consolidation steps (knowledge digest, personality vector, opinions,
//! retroactive extraction) need a language-model call.  Rather than each
//! module reaching into [`crate::chord`] directly — which would make them
//! impossible to unit-test without a live Chord proxy — they depend on this
//! small synchronous trait.  Production wires a [`ChordGenerator`]; tests pass
//! a mock that returns canned text.
//!
//! Consolidation runs on a background (blocking) task during sleep-time, so a
//! synchronous seam is appropriate; the real implementation blocks on the
//! async Chord call via the current Tokio runtime handle.

use crate::error::{LuminaError, Result};
use std::sync::Arc;

/// A synchronous text-generation seam.
pub trait LlmGenerator: Send + Sync {
    /// Generate a completion for `system_prompt` + `user_prompt`.
    ///
    /// `model_hint` is an abstract tier ("lumina-fast" / "lumina-deep"); the
    /// implementation maps it to a concrete backend model.  Returns the raw
    /// completion text (callers enforce their own length budgets).
    fn generate(&self, model_hint: &str, system_prompt: &str, user_prompt: &str) -> Result<String>;
}

/// Test/double generator returning a fixed string (or echoing the prompt).
#[derive(Debug, Clone, Default)]
pub struct MockGenerator {
    pub canned: Option<String>,
}

impl MockGenerator {
    pub fn returning(s: impl Into<String>) -> Self {
        MockGenerator { canned: Some(s.into()) }
    }
}

impl LlmGenerator for MockGenerator {
    fn generate(&self, _model_hint: &str, _system_prompt: &str, user_prompt: &str) -> Result<String> {
        Ok(self.canned.clone().unwrap_or_else(|| user_prompt.to_string()))
    }
}

/// Default concrete model alias for the "lumina-deep" tier.
pub const DEFAULT_DEEP_MODEL: &str = "lumina-deep";
/// Default concrete model alias for the "lumina-fast" tier.
pub const DEFAULT_FAST_MODEL: &str = "lumina-fast";

/// Production [`LlmGenerator`] backed by the async [`ChordClient`].
///
/// Consolidation runs on a blocking background task during sleep-time, but the
/// builders that drive it ([`super::knowledge_digest`], [`super::personality_vector`],
/// [`crate::engram::retroactive`]) take a *synchronous* seam. `ChordGenerator`
/// bridges the two by running the async Chord call to completion on the current
/// Tokio runtime via [`tokio::task::block_in_place`] +
/// `Handle::current().block_on(..)`.
///
/// * Requires a multi-threaded Tokio runtime (`block_in_place` panics on a
///   current-thread runtime); the sleep-time orchestrator is always invoked
///   from the scheduler's multi-thread runtime.
/// * If **no** runtime is active (e.g. a unit test on a bare thread) `generate`
///   returns a graceful `Err` instead of panicking, so callers degrade rather
///   than crash.
///
/// `model_hint` ("lumina-deep" / "lumina-fast") is mapped to a concrete model
/// string; unknown hints fall through to the deep model (quality default).
#[derive(Clone)]
pub struct ChordGenerator {
    client: Arc<crate::chord::ChordClient>,
    /// Concrete model alias for the "lumina-deep" tier.
    deep_model: String,
    /// Concrete model alias for the "lumina-fast" tier.
    fast_model: String,
}

impl ChordGenerator {
    /// Wrap a `ChordClient` with the default deep/fast model aliases.
    pub fn new(client: Arc<crate::chord::ChordClient>) -> Self {
        ChordGenerator {
            client,
            deep_model: DEFAULT_DEEP_MODEL.to_string(),
            fast_model: DEFAULT_FAST_MODEL.to_string(),
        }
    }

    /// Override the concrete model aliases each tier maps to.
    pub fn with_models(
        client: Arc<crate::chord::ChordClient>,
        deep_model: impl Into<String>,
        fast_model: impl Into<String>,
    ) -> Self {
        ChordGenerator {
            client,
            deep_model: deep_model.into(),
            fast_model: fast_model.into(),
        }
    }

    /// Map an abstract tier hint to a concrete model alias.
    fn resolve_model(&self, model_hint: &str) -> &str {
        match model_hint {
            "lumina-fast" => &self.fast_model,
            // "lumina-deep" and anything unrecognised default to the deep model.
            _ => &self.deep_model,
        }
    }
}

impl LlmGenerator for ChordGenerator {
    fn generate(&self, model_hint: &str, system_prompt: &str, user_prompt: &str) -> Result<String> {
        let model = self.resolve_model(model_hint).to_string();
        let messages = vec![
            crate::chord::ChatMessage::text("system", system_prompt),
            crate::chord::ChatMessage::text("user", user_prompt),
        ];

        // Acquire the current runtime handle; absence is a graceful error.
        let handle = tokio::runtime::Handle::try_current().map_err(|_| {
            LuminaError::Chord(
                "ChordGenerator::generate called without an active Tokio runtime".into(),
            )
        })?;

        let client = Arc::clone(&self.client);
        // block_in_place keeps from starving the runtime while we wait on the
        // async Chord call. Requires a multi-thread runtime.
        let result = tokio::task::block_in_place(move || {
            handle.block_on(async move { client.chat_completion_with_model(&model, messages).await })
        });

        result.map(|z| z.as_str().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_returns_canned() {
        let g = MockGenerator::returning("hello");
        assert_eq!(g.generate("lumina-fast", "sys", "user").unwrap(), "hello");
    }

    #[test]
    fn mock_echoes_user_when_no_canned() {
        let g = MockGenerator::default();
        assert_eq!(g.generate("lumina-deep", "sys", "the user prompt").unwrap(), "the user prompt");
    }

    fn chord_gen() -> ChordGenerator {
        let client = Arc::new(crate::chord::ChordClient::new(
            "http://localhost:0".into(),
            "test-secret".into(),
        ));
        ChordGenerator::with_models(client, "deep-alias", "fast-alias")
    }

    #[test]
    fn chord_generator_maps_model_hints() {
        let g = chord_gen();
        assert_eq!(g.resolve_model("lumina-fast"), "fast-alias");
        assert_eq!(g.resolve_model("lumina-deep"), "deep-alias");
        // Unknown hint falls through to the deep (quality) model.
        assert_eq!(g.resolve_model("something-else"), "deep-alias");
    }

    #[test]
    fn chord_generator_errors_without_runtime() {
        // Called from a bare test thread → no Tokio runtime → graceful Err,
        // never a panic.
        let g = chord_gen();
        let err = g.generate("lumina-deep", "sys", "user").unwrap_err();
        assert!(matches!(err, LuminaError::Chord(_)));
    }
}
