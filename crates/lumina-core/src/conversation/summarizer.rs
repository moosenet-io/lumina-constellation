//! CONV-05: progressive summarization of older conversation turns.
//!
//! When a session accumulates more than a threshold of verbatim turns, the
//! oldest half are compressed into a compact summary block by a *fast local*
//! model (default `qwen3:8b`) so older context is preserved in gist form while
//! recent turns stay verbatim. This is the pattern research consistently shows
//! beats both pure truncation and pure summarization.
//!
//! VRAM discipline: the summarizer must NOT use the personality model
//! (gpt-oss:20b) and must not force an eviction. It targets the CPU Ollama
//! endpoint when available (`OLLAMA_CPU_URL`), else the GPU endpoint where
//! `qwen3:8b` is already warm. Summarization runs in a background task, so it
//! never adds latency to the user's current turn; on any failure the buffer
//! simply falls back to FIFO eviction.

use crate::conversation::buffer::BufferEntry;
use crate::error::{LuminaError, Result};

/// Lower/upper sanity bounds (in approx tokens) for an accepted summary.
const MIN_SUMMARY_TOKENS: usize = 5;
const MAX_SUMMARY_TOKENS: usize = 500;

/// Build the summarization prompt from a run of turn-pairs.
pub fn build_prompt(turns: &[BufferEntry]) -> String {
    let mut transcript = String::new();
    for (i, t) in turns.iter().enumerate() {
        transcript.push_str(&format!("[Turn {}]\nUser: {}\nAssistant: {}\n",
            i + 1, t.user_message, t.assistant_response));
    }
    format!(
        "Summarize the following conversation exchange in 2-3 sentences. \
Preserve: specific names, numbers, decisions, action items, and tool results. \
Discard: pleasantries, filler, formatting details. Reply with only the summary.\n\n{transcript}"
    )
}

/// Validate a model's summary output (pure). Rejects empty/garbage (too short or
/// implausibly long) so the caller can fall back to FIFO eviction. Trims
/// surrounding whitespace and any `<think>...</think>` reasoning preamble that
/// reasoning models (qwen3) may emit.
pub fn validate_summary(raw: &str) -> Result<String> {
    let mut text = raw.trim();
    // Strip a leading <think>...</think> block if present.
    if let Some(end) = text.find("</think>") {
        text = text[end + "</think>".len()..].trim();
    }
    let text = text.trim();
    if text.is_empty() {
        return Err(LuminaError::Config("summary empty".into()));
    }
    let tokens = text.chars().count() / 4;
    if tokens < MIN_SUMMARY_TOKENS {
        return Err(LuminaError::Config(format!("summary too short ({tokens} tok)")));
    }
    if tokens > MAX_SUMMARY_TOKENS {
        return Err(LuminaError::Config(format!("summary too long ({tokens} tok)")));
    }
    Ok(text.to_string())
}

/// Call the local model to summarize `turns`. Async, non-blocking. Returns the
/// validated summary text, or an error (→ caller falls back to FIFO eviction).
///
/// `url` is an Ollama base URL (e.g. `http://host:11435`); `model` e.g.
/// `qwen3:8b`. POSTs to `{url}/api/generate` with `stream:false`.
pub async fn summarize_turns(turns: &[BufferEntry], url: &str, model: &str) -> Result<String> {
    if url.is_empty() {
        return Err(LuminaError::Config("summarizer URL not configured".into()));
    }
    let prompt = build_prompt(turns);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| LuminaError::Config(format!("reqwest build error: {e}")))?;
    let body = serde_json::json!({
        "model": model,
        "prompt": prompt,
        "stream": false,
        "options": { "num_predict": 220 },
    });
    let resp = client
        .post(format!("{}/api/generate", url.trim_end_matches('/')))
        .json(&body)
        .send()
        .await
        .map_err(|e| LuminaError::Config(format!("summarizer request failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(LuminaError::Config(format!("summarizer HTTP {}", resp.status())));
    }
    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| LuminaError::Config(format!("summarizer bad json: {e}")))?;
    let raw = json["response"].as_str().unwrap_or("");
    validate_summary(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(u: &str, a: &str) -> BufferEntry {
        // Build via the buffer's public surface to keep approx_tokens consistent.
        let mut b = crate::conversation::buffer::ConversationBuffer::new(10, 100_000, 1800);
        b.push("t", u, a, 1);
        b.get_context("t", 1).into_iter().next().unwrap()
    }

    #[test]
    fn build_prompt_includes_turns_and_instructions() {
        let p = build_prompt(&[turn("weather in SJ?", "65F clear"), turn("sharks score?", "4-1")]);
        assert!(p.contains("[Turn 1]"));
        assert!(p.contains("[Turn 2]"));
        assert!(p.contains("weather in SJ?"));
        assert!(p.contains("Preserve:"));
    }

    #[test]
    fn validate_accepts_reasonable_summary() {
        let s = validate_summary("  User asked about SJ weather (65F clear) and the Sharks score (4-1).  ").unwrap();
        assert!(s.starts_with("User asked"));
        assert!(!s.ends_with(' '));
    }

    #[test]
    fn validate_strips_think_block() {
        let s = validate_summary("<think>let me reason about this carefully</think>\nUser discussed the weather and the hockey score in detail today.").unwrap();
        assert!(!s.contains("<think>"));
        assert!(s.starts_with("User discussed"));
    }

    #[test]
    fn validate_rejects_empty_and_too_short() {
        assert!(validate_summary("").is_err());
        assert!(validate_summary("   ").is_err());
        assert!(validate_summary("ok").is_err()); // < MIN tokens
    }

    #[test]
    fn validate_rejects_too_long() {
        let huge = "word ".repeat(1200); // ~1500 tokens
        assert!(validate_summary(&huge).is_err());
    }
}
