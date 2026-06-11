//! EDGE-03/04: Skills engine — procedural memory with version history.
//!
//! When Lumina completes a complex multi-step task (3+ tool calls or 5+ turns),
//! it autonomously generates a structured skill document. On future similar
//! tasks, the matching skill is loaded into the LLM context window.
//!
//! EDGE-04 adds versioning: every update saves the current procedure into
//! `skill_versions` before overwriting, enabling full history and rollback.
//!
//! Skills are stored in SQLCipher alongside training data, using the same
//! encryption infrastructure.

pub mod skill_store;
pub mod skill_generator;

use crate::error::{LuminaError, Result};
use skill_store::SkillStore;

/// Re-export SkillVersion so callers can use `skills::SkillVersion`.
pub use skill_store::SkillVersion;

// ── Public types ───────────────────────────────────────────────────────────

/// A reusable procedure document derived from a completed complex task.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Skill {
    pub id: i64,
    pub name: String,
    pub description: String,
    /// Keywords or phrases that activate this skill.
    pub trigger_patterns: Vec<String>,
    /// Step-by-step instructions.
    pub procedure: String,
    /// MCP tool names referenced in the procedure.
    pub tools_used: Vec<String>,
    /// Times this skill led to a successful outcome.
    pub success_count: i64,
    /// Version number (starts at 1, incremented on updates).
    pub version: i64,
    pub created_at: String,
    pub last_used: Option<String>,
    /// Optional embedding vector for semantic search.
    pub embedding: Option<Vec<f32>>,
}

// ── SkillEngine ────────────────────────────────────────────────────────────

/// Manages skill lookup, injection, and recording.
///
/// If the database path is not configured, all operations silently return
/// `Ok(None)` or `Ok(())` to avoid breaking the existing agent behavior.
pub struct SkillEngine {
    store: SkillStore,
    /// Minimum cosine similarity score to consider a skill a match.
    similarity_threshold: f32,
}

impl SkillEngine {
    /// Create a new SkillEngine backed by a SQLCipher database.
    pub fn new(db_path: &std::path::Path, encryption_key: &[u8]) -> Result<Self> {
        let store = SkillStore::new(db_path, encryption_key)?;
        Ok(Self {
            store,
            similarity_threshold: 0.75,
        })
    }

    /// Set a custom similarity threshold (default 0.75).
    pub fn with_threshold(mut self, threshold: f32) -> Self {
        self.similarity_threshold = threshold;
        self
    }

    /// Find the best matching skill for the given input.
    ///
    /// Matching strategy:
    /// 1. Keyword match: check trigger_patterns words against input (case-insensitive)
    /// 2. If embedding is available: compute cosine similarity
    /// 3. Return the skill with highest score above `similarity_threshold`
    /// 4. If multiple matches: prefer the one with highest `success_count`
    pub fn find_matching(&self, input: &str) -> Result<Option<Skill>> {
        let input_lower = input.to_lowercase();
        let all_skills = self.store.list()?;

        if all_skills.is_empty() {
            return Ok(None);
        }

        let mut best: Option<(f32, &Skill)> = None;

        for skill in &all_skills {
            let score = self.score_skill(skill, &input_lower);
            if score < self.similarity_threshold {
                continue;
            }
            match best {
                None => {
                    best = Some((score, skill));
                }
                Some((best_score, best_skill)) => {
                    // Prefer higher score; break ties by success_count
                    if score > best_score
                        || (score == best_score && skill.success_count > best_skill.success_count)
                    {
                        best = Some((score, skill));
                    }
                }
            }
        }

        Ok(best.map(|(_, s)| s.clone()))
    }

    /// Format a skill as a context block to inject into the system prompt.
    pub fn inject_into_context(&self, skill: &Skill) -> String {
        format!(
            "## Relevant Skill: {name}\n\
             {description}\n\
             \n\
             ### Procedure\n\
             {procedure}\n\
             \n\
             ### Tools\n\
             {tools}",
            name = skill.name,
            description = skill.description,
            procedure = skill.procedure,
            tools = skill.tools_used.join(", "),
        )
    }

    /// Record a successful use of a skill.
    pub fn record_success(&self, skill_id: i64) -> Result<()> {
        self.store.update_success(skill_id)
    }

    /// Returns true if the task was complex enough to warrant skill generation.
    ///
    /// Threshold: 3+ tool calls OR 5+ conversation turns.
    pub fn should_generate_skill(tool_call_count: usize, turn_count: usize) -> bool {
        tool_call_count >= 3 || turn_count >= 5
    }

    /// List all stored skills.
    pub fn list_skills(&self) -> Result<Vec<Skill>> {
        self.store.list()
    }

    /// Retrieve a skill by id.
    pub fn get_skill(&self, id: i64) -> Result<Option<Skill>> {
        self.store.get(id)
    }

    /// Delete a skill by id.
    pub fn delete_skill(&self, id: i64) -> Result<()> {
        self.store.delete(id)
    }

    /// Update a skill's procedure with version history (EDGE-04).
    ///
    /// Delegates to `update_skill` so the old version is always snapshotted
    /// into `skill_versions` before the procedure is overwritten.
    pub fn update_skill_procedure(&mut self, id: i64, procedure: &str) -> Result<()> {
        self.update_skill(id, procedure)
    }

    /// EDGE-04: Update a skill with version history.
    ///
    /// 1. Reads the current skill to get its procedure and version.
    /// 2. Atomically: snapshots the current procedure into `skill_versions` AND
    ///    writes the new procedure + incremented version to the `skills` table.
    ///    Both writes happen in a single SQLite transaction — a crash between them
    ///    cannot leave the database in an inconsistent state.
    ///
    /// Returns an error if the skill does not exist.
    pub fn update_skill(&mut self, id: i64, new_procedure: &str) -> Result<()> {
        let current = self
            .store
            .get(id)?
            .ok_or_else(|| LuminaError::Config(format!("skill {} not found", id)))?;

        // Atomic snapshot + update — no orphaned history rows on partial failure
        self.store.save_version_and_update(
            id,
            current.version,
            &current.procedure,
            new_procedure,
        )?;

        Ok(())
    }

    /// EDGE-04: Return all historical versions of a skill, oldest first.
    pub fn get_history(&self, id: i64) -> Result<Vec<SkillVersion>> {
        self.store.get_version_history(id)
    }

    /// EDGE-04: Roll a skill back to a previously saved version.
    ///
    /// Snapshots the current live procedure into `skill_versions` first (so it
    /// can be recovered if needed), then restores the target version.
    /// Both operations are transactional.
    pub fn rollback(&mut self, id: i64, version: i64) -> Result<()> {
        self.store.rollback_to_version(id, version)
    }

    /// Insert a skill generated by SkillGenerator (sets the DB id).
    pub fn store_skill(&self, skill: &Skill) -> Result<i64> {
        self.store.insert(skill)
    }

    // ── Private helpers ────────────────────────────────────────────────────

    /// Compute a match score for a skill against the input string.
    ///
    /// Returns a value in [0.0, 1.0]. Delegates to the module-level
    /// `keyword_score_for` to avoid duplicating the scoring formula.
    fn score_skill(&self, skill: &Skill, input_lower: &str) -> f32 {
        // Delegate to the shared keyword scoring function (also used by
        // find_matching_with_embedding) to avoid duplicating the formula.
        let keyword_score = keyword_score_for(skill, input_lower);

        // Embedding score is 0.0 here because no query embedding is available
        // in this code path. Use find_matching_with_embedding when an embedding
        // is available (e.g. after the Ollama embed call in the agent loop).
        let embedding_score = 0.0f32;

        keyword_score.max(embedding_score)
    }

    /// Find the best matching skill using both keyword and embedding similarity.
    ///
    /// `query_embedding` is the pre-computed embedding of the user's input.
    pub fn find_matching_with_embedding(
        &self,
        input: &str,
        query_embedding: &[f32],
    ) -> Result<Option<Skill>> {
        let input_lower = input.to_lowercase();
        let all_skills = self.store.list()?;

        if all_skills.is_empty() {
            return Ok(None);
        }

        let mut best: Option<(f32, &Skill)> = None;

        for skill in &all_skills {
            let keyword_score = keyword_score_for(skill, &input_lower);
            let embedding_score = skill
                .embedding
                .as_deref()
                .map(|emb| cosine_similarity(query_embedding, emb))
                .unwrap_or(0.0f32);

            // Combined score: max of keyword and embedding
            let score = keyword_score.max(embedding_score);

            if score < self.similarity_threshold {
                continue;
            }

            match best {
                None => {
                    best = Some((score, skill));
                }
                Some((best_score, best_skill)) => {
                    if score > best_score
                        || (score == best_score && skill.success_count > best_skill.success_count)
                    {
                        best = Some((score, skill));
                    }
                }
            }
        }

        Ok(best.map(|(_, s)| s.clone()))
    }
}

// ── Standalone helpers ─────────────────────────────────────────────────────

/// Cosine similarity between two vectors. Returns 0.0 if either is zero-length.
///
/// Standard dot product / (|a| * |b|).
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }

    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let mag_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let mag_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if mag_a == 0.0 || mag_b == 0.0 {
        return 0.0;
    }

    (dot / (mag_a * mag_b)).clamp(-1.0, 1.0)
}

/// Keyword match score for a skill against lowercased input.
fn keyword_score_for(skill: &Skill, input_lower: &str) -> f32 {
    if skill.trigger_patterns.is_empty() {
        return 0.0;
    }
    let matches = skill
        .trigger_patterns
        .iter()
        .filter(|pat| input_lower.contains(pat.to_lowercase().as_str()))
        .count();
    if matches == 0 {
        0.0
    } else {
        // Same scoring as score_skill: 0.8 base for any match, up to 1.0 for full match
        0.8 + 0.2 * (matches as f32 / skill.trigger_patterns.len() as f32)
    }
}

/// Open the default skills database (~/.lumina/skills.db).
///
/// Uses the same key infrastructure as the training store.
pub fn open_default_skill_engine() -> Option<SkillEngine> {
    let db_path = default_skills_db_path();
    let key = crate::training_store::get_or_create_training_key().ok()?;
    SkillEngine::new(&db_path, &key).ok()
}

/// Return the default skills database path: ~/.lumina/skills.db
pub fn default_skills_db_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".lumina")
        .join("skills.db")
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_key() -> Vec<u8> {
        vec![42u8; 32]
    }

    fn tmp_engine(name: &str) -> SkillEngine {
        let path = PathBuf::from(format!("/tmp/lumina_skill_engine_test_{}.db", name));
        let _ = std::fs::remove_file(&path);
        SkillEngine::new(&path, &test_key()).unwrap()
    }

    fn sample_skill_data() -> Skill {
        Skill {
            id: 0,
            name: "Log File Analysis".to_string(),
            description: "Analyze log files for errors and patterns".to_string(),
            trigger_patterns: vec![
                "log file".to_string(),
                "analyze logs".to_string(),
                "error pattern".to_string(),
            ],
            procedure: "1. Find log files\n2. Search for errors\n3. Summarize".to_string(),
            tools_used: vec!["shell_exec".to_string()],
            success_count: 5,
            version: 1,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            last_used: None,
            embedding: None,
        }
    }

    #[test]
    fn test_should_generate_skill_three_tools() {
        assert!(SkillEngine::should_generate_skill(3, 1));
        assert!(SkillEngine::should_generate_skill(5, 1));
        assert!(!SkillEngine::should_generate_skill(2, 4));
    }

    #[test]
    fn test_should_generate_skill_five_turns() {
        assert!(SkillEngine::should_generate_skill(1, 5));
        assert!(SkillEngine::should_generate_skill(0, 6));
        assert!(!SkillEngine::should_generate_skill(2, 4));
    }

    #[test]
    fn test_should_not_generate_below_threshold() {
        assert!(!SkillEngine::should_generate_skill(0, 0));
        assert!(!SkillEngine::should_generate_skill(2, 4));
    }

    #[test]
    fn test_find_matching_by_keyword() {
        let engine = tmp_engine("keyword_match");
        engine.store_skill(&sample_skill_data()).unwrap();

        let result = engine.find_matching("I need to analyze logs for errors").unwrap();
        assert!(result.is_some());
        let skill = result.unwrap();
        assert_eq!(skill.name, "Log File Analysis");

        // Cleanup
        let _ = std::fs::remove_file("/tmp/lumina_skill_engine_test_keyword_match.db");
    }

    #[test]
    fn test_find_no_match_below_threshold() {
        let engine = tmp_engine("no_match");
        engine.store_skill(&sample_skill_data()).unwrap();

        let result = engine.find_matching("completely unrelated topic about cooking").unwrap();
        assert!(result.is_none());

        let _ = std::fs::remove_file("/tmp/lumina_skill_engine_test_no_match.db");
    }

    #[test]
    fn test_find_empty_store_returns_none() {
        let engine = tmp_engine("empty");
        let result = engine.find_matching("log file analysis").unwrap();
        assert!(result.is_none());

        let _ = std::fs::remove_file("/tmp/lumina_skill_engine_test_empty.db");
    }

    #[test]
    fn test_inject_into_context_format() {
        let engine = tmp_engine("inject");
        let skill = sample_skill_data();

        let context = engine.inject_into_context(&skill);
        assert!(context.contains("## Relevant Skill: Log File Analysis"));
        assert!(context.contains("### Procedure"));
        assert!(context.contains("### Tools"));
        assert!(context.contains("shell_exec"));
        assert!(context.contains("1. Find log files"));

        let _ = std::fs::remove_file("/tmp/lumina_skill_engine_test_inject.db");
    }

    #[test]
    fn test_record_success_increments() {
        let engine = tmp_engine("record_success");
        let id = engine.store_skill(&sample_skill_data()).unwrap();

        engine.record_success(id).unwrap();
        engine.record_success(id).unwrap();

        let skill = engine.get_skill(id).unwrap().unwrap();
        // sample_skill_data has success_count = 5, plus 2 increments = 7
        assert_eq!(skill.success_count, 7);

        let _ = std::fs::remove_file("/tmp/lumina_skill_engine_test_record_success.db");
    }

    #[test]
    fn test_update_skill_procedure_increments_version() {
        let mut engine = tmp_engine("update_proc");
        let id = engine.store_skill(&sample_skill_data()).unwrap();

        engine.update_skill_procedure(id, "New improved procedure").unwrap();

        let skill = engine.get_skill(id).unwrap().unwrap();
        assert_eq!(skill.procedure, "New improved procedure");
        assert_eq!(skill.version, 2);

        let _ = std::fs::remove_file("/tmp/lumina_skill_engine_test_update_proc.db");
    }

    #[test]
    fn test_delete_skill() {
        let engine = tmp_engine("delete");
        let id = engine.store_skill(&sample_skill_data()).unwrap();
        engine.delete_skill(id).unwrap();

        let result = engine.get_skill(id).unwrap();
        assert!(result.is_none());

        let _ = std::fs::remove_file("/tmp/lumina_skill_engine_test_delete.db");
    }

    #[test]
    fn test_list_skills() {
        let engine = tmp_engine("list");
        engine.store_skill(&sample_skill_data()).unwrap();
        engine.store_skill(&sample_skill_data()).unwrap();

        let skills = engine.list_skills().unwrap();
        assert_eq!(skills.len(), 2);

        let _ = std::fs::remove_file("/tmp/lumina_skill_engine_test_list.db");
    }

    #[test]
    fn test_cosine_similarity_identical() {
        let a = vec![1.0f32, 0.0, 0.0];
        let b = vec![1.0f32, 0.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = vec![1.0f32, 0.0];
        let b = vec![0.0f32, 1.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_opposite() {
        let a = vec![1.0f32, 0.0];
        let b = vec![-1.0f32, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - (-1.0)).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_empty_returns_zero() {
        let a: Vec<f32> = vec![];
        let b: Vec<f32> = vec![1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert_eq!(sim, 0.0);
    }

    #[test]
    fn test_cosine_similarity_mismatched_length_returns_zero() {
        let a = vec![1.0f32, 0.0];
        let b = vec![1.0f32, 0.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert_eq!(sim, 0.0);
    }

    #[test]
    fn test_find_matching_with_embedding_semantic() {
        let engine = tmp_engine("embedding_match");
        let mut skill = sample_skill_data();
        // Embedding pointing in direction [1, 0]
        skill.embedding = Some(vec![1.0f32, 0.0]);
        engine.store_skill(&skill).unwrap();

        // Query embedding also [1, 0] → cosine = 1.0 > threshold
        let query_emb = vec![1.0f32, 0.0];
        let result = engine
            .find_matching_with_embedding("random unmatched query", &query_emb)
            .unwrap();
        assert!(result.is_some());

        let _ = std::fs::remove_file("/tmp/lumina_skill_engine_test_embedding_match.db");
    }

    #[test]
    fn test_find_matching_with_embedding_no_match() {
        let engine = tmp_engine("embedding_no_match");
        let mut skill = sample_skill_data();
        // Embedding pointing in direction [1, 0]
        skill.embedding = Some(vec![1.0f32, 0.0]);
        engine.store_skill(&skill).unwrap();

        // Query embedding orthogonal → cosine = 0.0 < threshold
        let query_emb = vec![0.0f32, 1.0];
        let result = engine
            .find_matching_with_embedding("random unmatched query", &query_emb)
            .unwrap();
        assert!(result.is_none());

        let _ = std::fs::remove_file("/tmp/lumina_skill_engine_test_embedding_no_match.db");
    }

    #[test]
    fn test_multiple_matches_highest_success_count_wins() {
        let engine = tmp_engine("multi_match");

        let mut skill_a = sample_skill_data();
        skill_a.name = "Skill A".to_string();
        skill_a.success_count = 2;

        let mut skill_b = sample_skill_data();
        skill_b.name = "Skill B".to_string();
        skill_b.success_count = 10;

        engine.store_skill(&skill_a).unwrap();
        engine.store_skill(&skill_b).unwrap();

        // Both have same trigger patterns, so same keyword score
        let result = engine
            .find_matching("I need to analyze log file errors")
            .unwrap();
        assert!(result.is_some());
        // Skill B has higher success_count, but list() returns by success_count DESC
        // so the first best match should be Skill B
        let skill = result.unwrap();
        // Both match equally; highest success_count wins
        assert_eq!(skill.name, "Skill B");

        let _ = std::fs::remove_file("/tmp/lumina_skill_engine_test_multi_match.db");
    }

    #[test]
    fn test_skill_context_prepended_to_system_prompt() {
        let engine = tmp_engine("context_prepend");
        let skill = sample_skill_data();
        let context = engine.inject_into_context(&skill);
        let system_prompt = "You are Lumina.";
        let combined = format!("{}\n\n{}", context, system_prompt);

        assert!(combined.starts_with("## Relevant Skill:"));
        assert!(combined.contains("You are Lumina."));

        let _ = std::fs::remove_file("/tmp/lumina_skill_engine_test_context_prepend.db");
    }

    // ── EDGE-04 SkillEngine tests ──────────────────────────────────────────

    #[test]
    fn test_update_skill_creates_version_entry() {
        let mut engine = tmp_engine("update_creates_version");
        let id = engine.store_skill(&sample_skill_data()).unwrap();

        // First update should snapshot v1 into history and set v2
        engine
            .update_skill(id, "Improved procedure step-by-step")
            .unwrap();

        let history = engine.get_history(id).unwrap();
        assert_eq!(history.len(), 1, "Expected one history entry after first update");
        assert_eq!(history[0].version, 1);
        assert!(
            history[0].procedure.contains("Find log files"),
            "History should contain original procedure"
        );

        let current = engine.get_skill(id).unwrap().unwrap();
        assert_eq!(current.version, 2);
        assert_eq!(current.procedure, "Improved procedure step-by-step");

        let _ = std::fs::remove_file("/tmp/lumina_skill_engine_test_update_creates_version.db");
    }

    #[test]
    fn test_update_skill_multiple_times_preserves_all_versions() {
        let mut engine = tmp_engine("multi_version");
        let id = engine.store_skill(&sample_skill_data()).unwrap();

        engine.update_skill(id, "Procedure v2").unwrap();
        engine.update_skill(id, "Procedure v3").unwrap();
        engine.update_skill(id, "Procedure v4").unwrap();

        let history = engine.get_history(id).unwrap();
        // v1, v2, v3 should be in history (v4 is live)
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].version, 1);
        assert_eq!(history[1].version, 2);
        assert_eq!(history[2].version, 3);

        let current = engine.get_skill(id).unwrap().unwrap();
        assert_eq!(current.version, 4);
        assert_eq!(current.procedure, "Procedure v4");

        let _ = std::fs::remove_file("/tmp/lumina_skill_engine_test_multi_version.db");
    }

    #[test]
    fn test_rollback_restores_previous_version() {
        let mut engine = tmp_engine("rollback_engine");
        let id = engine.store_skill(&sample_skill_data()).unwrap();

        let original_procedure = sample_skill_data().procedure;
        engine.update_skill(id, "Worse procedure v2").unwrap();

        // Rollback to v1 — also snapshots v2 into history
        engine.rollback(id, 1).unwrap();

        let rolled_back = engine.get_skill(id).unwrap().unwrap();
        assert_eq!(rolled_back.procedure, original_procedure);
        assert_eq!(rolled_back.version, 1);

        let _ = std::fs::remove_file("/tmp/lumina_skill_engine_test_rollback_engine.db");
    }

    #[test]
    fn test_get_history_empty_for_new_skill() {
        let engine = tmp_engine("no_history");
        let id = engine.store_skill(&sample_skill_data()).unwrap();

        let history = engine.get_history(id).unwrap();
        assert!(history.is_empty(), "New skill should have no version history");

        let _ = std::fs::remove_file("/tmp/lumina_skill_engine_test_no_history.db");
    }

    #[test]
    fn test_update_skill_nonexistent_returns_error() {
        let mut engine = tmp_engine("update_nonexistent");
        let result = engine.update_skill(9999, "some procedure");
        assert!(result.is_err(), "Updating nonexistent skill should return error");

        let _ = std::fs::remove_file("/tmp/lumina_skill_engine_test_update_nonexistent.db");
    }

    #[test]
    fn test_rollback_nonexistent_version_returns_error() {
        let mut engine = tmp_engine("rollback_bad_version");
        let id = engine.store_skill(&sample_skill_data()).unwrap();

        let result = engine.rollback(id, 999);
        assert!(result.is_err(), "Rollback to nonexistent version should error");

        let _ = std::fs::remove_file(
            "/tmp/lumina_skill_engine_test_rollback_bad_version.db",
        );
    }
}
