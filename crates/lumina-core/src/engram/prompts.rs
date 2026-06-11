//! EMEM-04: LLM extraction prompt templates for Engram v2.
//!
//! Contains the memory extraction prompt that produces a JSON array of typed,
//! classified, provenanced memories from a single conversation turn.
//!
//! The extraction model is "lumina-fast" (local Qwen) — never a cloud model.
//! No hardcoded infrastructure values anywhere in this file.

/// The model alias used for memory extraction.
///
/// Must be "lumina-fast" — a cheap local model per inference de-bloat rules.
/// Extraction is structured JSON output (not synthesis or reasoning), so
/// the small local model is appropriate here.
pub const EXTRACTION_MODEL: &str = "lumina-fast";

/// Maximum memories to extract per turn (rate limit — prevents over-extraction
/// on dense conversations and keeps ingestion cost near-zero).
pub const MAX_MEMORIES_PER_TURN: usize = 5;

/// Maximum content length for a single memory. Content beyond this is truncated.
pub const MAX_MEMORY_CONTENT_CHARS: usize = 2000;

/// Cosine similarity threshold above which two memories are considered
/// "same topic" and checked for contradiction / supersession.
pub const CONTRADICTION_SIMILARITY_THRESHOLD: f32 = 0.85;

/// Build the extraction prompt for a single conversation turn.
///
/// The LLM is asked to produce a JSON array of memories. Each element has:
/// - `content`: the fact or observation (string)
/// - `type`: "episodic" | "semantic" | "preference"
/// - `sensitivity`: "health" | "finance" | "personal" | "work" | "household" | "general"
/// - `confidence`: 0.0–1.0
/// - `tags`: array of keyword strings
///
/// Returns `[]` if nothing is worth remembering.
pub fn extraction_prompt(user_msg: &str, assistant_msg: &str) -> String {
    format!(
        r#"From this conversation turn, extract any new information worth remembering about the user.
For each memory (max {max}), provide JSON:
{{"content": "...", "type": "episodic|semantic|preference", "sensitivity": "health|finance|personal|work|household|general", "confidence": 0.0-1.0, "tags": ["tag1"]}}
Return a JSON array. If nothing worth remembering, return [].

User: {user_msg}
Assistant: {assistant_msg}"#,
        max = MAX_MEMORIES_PER_TURN,
        user_msg = user_msg,
        assistant_msg = assistant_msg,
    )
}

/// A single raw extracted memory from the LLM, before type conversion.
///
/// Uses `serde` to deserialise the LLM's JSON output. Fields use the
/// string representations from the prompt schema.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RawExtractedMemory {
    /// The memory content.
    pub content: String,
    /// "episodic", "semantic", or "preference".
    #[serde(rename = "type")]
    pub memory_type: String,
    /// "health", "finance", "personal", "work", "household", or "general".
    pub sensitivity: String,
    /// 0.0–1.0 confidence score.
    pub confidence: f64,
    /// Keyword tags.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Parse the LLM response into a list of raw extracted memories.
///
/// Returns an empty Vec on any parse error (non-fatal per spec). The caller
/// logs the warning; ingestion simply skips the turn.
pub fn parse_extraction_response(llm_output: &str) -> Vec<RawExtractedMemory> {
    // Strip markdown fences if the model wrapped the JSON in ```json ... ```
    let cleaned = strip_json_fences(llm_output.trim());
    match serde_json::from_str::<Vec<RawExtractedMemory>>(cleaned) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("engram/ingest: failed to parse LLM extraction response: {e}");
            eprintln!("engram/ingest: raw output was: {cleaned}");
            Vec::new()
        }
    }
}

/// Remove markdown code fences from JSON output if present.
fn strip_json_fences(s: &str) -> &str {
    let s = s.trim();
    // Handle ```json ... ``` or ``` ... ```
    if let Some(inner) = s.strip_prefix("```json").or_else(|| s.strip_prefix("```")) {
        if let Some(inner2) = inner.strip_suffix("```") {
            return inner2.trim();
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extraction_prompt_contains_required_fields() {
        let prompt = extraction_prompt("I like dark roast coffee", "That's great, noted!");
        assert!(prompt.contains("episodic|semantic|preference"), "prompt must mention all types");
        assert!(prompt.contains("health|finance|personal|work|household|general"), "prompt must mention sensitivity");
        assert!(prompt.contains("confidence"), "prompt must mention confidence");
        assert!(prompt.contains("tags"), "prompt must mention tags");
        assert!(prompt.contains("I like dark roast coffee"), "user_msg must appear in prompt");
        assert!(prompt.contains("That's great, noted!"), "assistant_msg must appear in prompt");
        assert!(prompt.contains("JSON array"), "must request a JSON array");
    }

    #[test]
    fn test_extraction_prompt_max_limit_in_text() {
        let prompt = extraction_prompt("test", "test");
        assert!(
            prompt.contains(&MAX_MEMORIES_PER_TURN.to_string()),
            "prompt must include max memories limit: {prompt}"
        );
    }

    #[test]
    fn test_parse_extraction_response_valid_json() {
        let json = r#"[
            {"content": "likes dark roast coffee", "type": "preference", "sensitivity": "general", "confidence": 0.9, "tags": ["coffee"]},
            {"content": "is a senior manager", "type": "semantic", "sensitivity": "work", "confidence": 0.95, "tags": ["job", "title"]}
        ]"#;
        let memories = parse_extraction_response(json);
        assert_eq!(memories.len(), 2);
        assert_eq!(memories[0].content, "likes dark roast coffee");
        assert_eq!(memories[0].memory_type, "preference");
        assert_eq!(memories[0].sensitivity, "general");
        assert!((memories[0].confidence - 0.9).abs() < 0.001);
        assert_eq!(memories[0].tags, vec!["coffee"]);
        assert_eq!(memories[1].content, "is a senior manager");
    }

    #[test]
    fn test_parse_extraction_response_empty_array() {
        let memories = parse_extraction_response("[]");
        assert!(memories.is_empty(), "empty array should produce no memories");
    }

    #[test]
    fn test_parse_extraction_response_invalid_json_returns_empty() {
        let memories = parse_extraction_response("not json at all");
        assert!(memories.is_empty(), "invalid JSON should return empty Vec (non-fatal)");
    }

    #[test]
    fn test_parse_extraction_response_strips_markdown_fences() {
        let wrapped = "```json\n[{\"content\": \"test\", \"type\": \"semantic\", \"sensitivity\": \"general\", \"confidence\": 0.8, \"tags\": []}]\n```";
        let memories = parse_extraction_response(wrapped);
        assert_eq!(memories.len(), 1, "should parse through markdown fences");
        assert_eq!(memories[0].content, "test");
    }

    #[test]
    fn test_parse_extraction_response_strips_bare_fences() {
        let wrapped = "```\n[{\"content\": \"bare fence test\", \"type\": \"episodic\", \"sensitivity\": \"general\", \"confidence\": 0.7, \"tags\": []}]\n```";
        let memories = parse_extraction_response(wrapped);
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].content, "bare fence test");
    }

    #[test]
    fn test_parse_extraction_response_missing_tags_uses_default() {
        let json = r#"[{"content": "no tags", "type": "semantic", "sensitivity": "general", "confidence": 0.8}]"#;
        let memories = parse_extraction_response(json);
        assert_eq!(memories.len(), 1);
        assert!(memories[0].tags.is_empty(), "missing tags should default to empty vec");
    }

    #[test]
    fn test_max_memories_per_turn_constant() {
        assert_eq!(MAX_MEMORIES_PER_TURN, 5, "max must be 5 per spec");
    }

    #[test]
    fn test_max_content_chars_constant() {
        assert_eq!(MAX_MEMORY_CONTENT_CHARS, 2000, "max content must be 2000 per spec");
    }

    #[test]
    fn test_extraction_model_is_lumina_fast() {
        assert_eq!(EXTRACTION_MODEL, "lumina-fast", "must use lumina-fast, not a cloud model");
    }
}
