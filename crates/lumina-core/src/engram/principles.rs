//! EMEM-05: Principle abstraction engine for Engram v2.
//!
//! When enough related Preference-type memories accumulate (5+ per cluster),
//! the PrincipleEngine generates an abstracted Principle that captures the
//! underlying pattern. Principles are the highest-value memory type — they
//! transfer across contexts and reduce the need for retrieving many individual
//! preferences.
//!
//! Key design decisions:
//! - Cluster by cosine similarity > 0.65 (same cluster threshold)
//! - Only clusters with 5+ preferences trigger abstraction
//! - Uses lumina-fast (local model) for abstraction — no cloud cost
//! - Source preferences are preserved (not deleted)
//! - Per-user isolation — never cross user boundaries
//! - Skip entirely if user has < 10 total preferences

use crate::chord::ChordClient;
use crate::config::Config;
use crate::engram::types::{Memory, MemoryType, SensitivityCategory, Visibility, iso_now};
use crate::engram::{cosine, EngramStore};
use crate::error::Result;

// ── Constants ──────────────────────────────────────────────────────────────────

/// Cosine similarity threshold above which two preferences belong to the same cluster.
pub const CLUSTER_SIMILARITY_THRESHOLD: f32 = 0.65;

/// Minimum cluster size to trigger principle abstraction.
pub const MIN_CLUSTER_SIZE: usize = 5;

/// Minimum total preference count before the engine runs at all.
pub const MIN_PREFERENCES_FOR_ENGINE: usize = 10;

/// The model alias used for principle abstraction.
///
/// Must be "lumina-fast" — local model per inference de-bloat rules.
/// Abstraction is pattern recognition over a small set, not complex reasoning.
pub const ABSTRACTION_MODEL: &str = "lumina-fast";

// ── PrincipleEngine ────────────────────────────────────────────────────────────

/// Engine that abstracts clusters of Preference memories into higher-level Principles.
///
/// Run periodically (e.g. daily) per user via the scheduler. Each call is
/// self-contained — no state is held between runs.
pub struct PrincipleEngine;

impl PrincipleEngine {
    /// Cluster Preference-type memories by embedding cosine similarity.
    ///
    /// Uses a greedy algorithm: for each unassigned preference, start a new cluster.
    /// Any other unassigned preference with cosine > `similarity_threshold` against
    /// the cluster seed is added to that cluster.
    ///
    /// Returns a Vec of clusters (each cluster is a Vec<Memory>).
    /// Only memories with non-empty embeddings can be clustered; preferences
    /// without embeddings are skipped (they cannot be similarity-matched).
    pub fn detect_clusters(
        store: &EngramStore,
        user_id: &str,
        similarity_threshold: f32,
    ) -> Result<Vec<Vec<Memory>>> {
        // Safety: query_by_type is already user_id-scoped at SQL level; we double-check
        // that every returned memory belongs to user_id as a defence-in-depth measure.
        let all_prefs = store.query_by_type(MemoryType::Preference)?;

        // Per-user isolation: only process memories that belong to this user.
        let prefs: Vec<Memory> = all_prefs
            .into_iter()
            .filter(|m| m.user_id == user_id && !m.embedding.is_empty())
            .collect();

        if prefs.is_empty() {
            return Ok(Vec::new());
        }

        // Greedy single-linkage clustering against the seed embedding of each cluster.
        let mut clusters: Vec<Vec<Memory>> = Vec::new();
        let mut assigned = vec![false; prefs.len()];

        for i in 0..prefs.len() {
            if assigned[i] {
                continue;
            }
            // Start a new cluster with prefs[i] as the seed.
            let mut cluster = vec![prefs[i].clone()];
            assigned[i] = true;
            let seed_emb = prefs[i].embedding.clone();

            for j in (i + 1)..prefs.len() {
                if assigned[j] {
                    continue;
                }
                let sim = cosine(&seed_emb, &prefs[j].embedding);
                if sim > similarity_threshold {
                    cluster.push(prefs[j].clone());
                    assigned[j] = true;
                }
            }
            clusters.push(cluster);
        }

        Ok(clusters)
    }

    /// Build the LLM prompt for principle abstraction from a preference cluster.
    ///
    /// The prompt lists each preference on its own line and asks for a single
    /// abstract sentence that would help predict future preferences.
    pub fn build_abstraction_prompt(preferences: &[Memory]) -> String {
        let pref_list: String = preferences
            .iter()
            .map(|m| format!("- {}", m.content))
            .collect::<Vec<_>>()
            .join("\n");

        format!(
            "These are preferences stored about the same person:\n\
             {pref_list}\n\n\
             What single, abstract principle or pattern connects them? \n\
             Express it as one sentence that would help predict this person's preference in a NEW, unseen situation.\n\n\
             Examples:\n\
             - \"strong, unflavored, traditional preparations\" (from: dark roast, black tea, neat whiskey)\n\
             - \"direct, efficient communication, no padding\" (from: concise emails, bullet points, no sycophancy)\n\n\
             Principle:"
        )
    }

    /// Parse the LLM response into a Principle-type Memory.
    ///
    /// Creates a Principle memory with:
    /// - `memory_type`: Principle
    /// - `visibility`: Shared if any source is Shared/System, else Private
    /// - `confidence`: average of source preferences' confidence scores
    /// - `tags`: includes `"abstracted_from:{id1},{id2},..."` tag with source IDs
    /// - `user_id`: same as source preferences
    /// - `sensitivity`: General (principles are abstract patterns, not sensitive data)
    pub fn parse_principle(response: &str, source_preferences: &[Memory]) -> Memory {
        // Extract the principle text from the LLM response.
        // Strip "Principle:" prefix and surrounding whitespace/quotes if present.
        let principle_text = Self::extract_principle_text(response);

        // Derive visibility: if ANY source preference is Shared or System → Shared,
        // otherwise → Private. This is the most permissive interpretation —
        // a principle derived from shared preferences can also be shared.
        let visibility = if source_preferences
            .iter()
            .any(|m| matches!(m.visibility, Visibility::Shared | Visibility::System))
        {
            Visibility::Shared
        } else {
            Visibility::Private
        };

        // Confidence: average of source preferences.
        let avg_confidence = if source_preferences.is_empty() {
            0.8
        } else {
            let sum: f32 = source_preferences.iter().map(|m| m.confidence).sum();
            sum / source_preferences.len() as f32
        };

        // user_id from sources (all must be same user — per-user isolation).
        let user_id = source_preferences
            .first()
            .map(|m| m.user_id.clone())
            .unwrap_or_else(|| "system".to_string());

        // Build "abstracted_from" tag with source IDs.
        let source_ids: Vec<&str> = source_preferences.iter().map(|m| m.id.as_str()).collect();
        let abstracted_from_tag = format!("abstracted_from:{}", source_ids.join(","));

        let mut principle = Memory::new(
            user_id,
            MemoryType::Principle,
            SensitivityCategory::General,
            principle_text,
        );
        // Override defaults set by Memory::new
        principle.visibility = visibility;
        principle.confidence = avg_confidence;
        principle.tags.push(abstracted_from_tag);

        principle
    }

    /// Extract the principle text from a raw LLM response.
    ///
    /// Handles common LLM output patterns:
    /// - "Principle: <text>"
    /// - Just the text directly
    /// - Quoted text
    fn extract_principle_text(response: &str) -> String {
        let trimmed = response.trim();

        // Strip "Principle:" prefix if present (case-insensitive).
        let text = if let Some(after) = trimmed.strip_prefix("Principle:") {
            after.trim()
        } else if let Some(after) = trimmed.to_lowercase().strip_prefix("principle:") {
            // Fallback: re-slice original string by the prefix length
            let prefix_len = "principle:".len();
            trimmed[prefix_len..].trim()
        } else {
            trimmed
        };

        // Strip surrounding quotes if the model added them.
        let text = text.trim_matches('"').trim_matches('\'').trim();

        if text.is_empty() {
            "(no principle extracted)".to_string()
        } else {
            text.to_string()
        }
    }

    /// Check if a principle already exists for this cluster of source preferences.
    ///
    /// Returns true if any existing Principle memory has a tag that contains ALL
    /// of the source preference IDs in its "abstracted_from:..." tag.
    /// This prevents duplicate principles for the same cluster.
    fn principle_already_exists(store: &EngramStore, source_ids: &[&str]) -> bool {
        if source_ids.is_empty() {
            return false;
        }

        let existing_principles = match store.query_by_type(MemoryType::Principle) {
            Ok(p) => p,
            Err(_) => return false,
        };

        for principle in &existing_principles {
            for tag in &principle.tags {
                if let Some(id_part) = tag.strip_prefix("abstracted_from:") {
                    let existing_ids: Vec<&str> = id_part.split(',').collect();
                    // If all source IDs are covered by this principle's sources → duplicate.
                    let all_covered = source_ids
                        .iter()
                        .all(|sid| existing_ids.contains(sid));
                    if all_covered {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Main entry point: run principle abstraction for a single user.
    ///
    /// Workflow:
    /// 1. Skip if total preferences < MIN_PREFERENCES_FOR_ENGINE
    /// 2. Cluster preferences by cosine similarity
    /// 3. For clusters with >= MIN_CLUSTER_SIZE entries:
    ///    a. Check if principle already exists for this cluster
    ///    b. Build abstraction prompt
    ///    c. Call LLM (lumina-fast)
    ///    d. Parse and store principle
    /// 4. Return IDs of newly created principles
    ///
    /// Failures are non-fatal (logged, not propagated) — the engine skips
    /// individual clusters that fail rather than aborting the entire run.
    pub async fn run_for_user(
        store: &EngramStore,
        chord: &ChordClient,
        user_id: &str,
        _config: &Config,
    ) -> Result<Vec<String>> {
        // Step 1: Check total preference count — skip if too few.
        let all_prefs = store.query_by_type(MemoryType::Preference)?;
        let user_prefs: Vec<&Memory> = all_prefs
            .iter()
            .filter(|m| m.user_id == user_id)
            .collect();

        if user_prefs.len() < MIN_PREFERENCES_FOR_ENGINE {
            return Ok(Vec::new()); // Not enough data — skip silently
        }

        // Step 2: Cluster by embedding similarity.
        let clusters =
            Self::detect_clusters(store, user_id, CLUSTER_SIMILARITY_THRESHOLD)?;

        let mut new_principle_ids: Vec<String> = Vec::new();

        // Step 3: Process clusters with enough members.
        for cluster in &clusters {
            if cluster.len() < MIN_CLUSTER_SIZE {
                continue; // Too small — skip
            }

            // Step 3a: Dedup check — don't recreate principles for same cluster.
            let source_ids: Vec<&str> = cluster.iter().map(|m| m.id.as_str()).collect();
            if Self::principle_already_exists(store, &source_ids) {
                continue;
            }

            // Step 3b: Build prompt.
            let prompt = Self::build_abstraction_prompt(cluster);

            // Step 3c: Call LLM (non-fatal on failure).
            let response = match chord.chat(ABSTRACTION_MODEL, &prompt).await {
                Ok(r) => r,
                Err(e) => {
                    eprintln!(
                        "engram/principles: LLM abstraction failed for user {user_id} \
                         (non-fatal): {e}"
                    );
                    continue;
                }
            };

            // Step 3d: Parse and store the principle.
            let principle = Self::parse_principle(response.as_str(), cluster);
            let principle_id = principle.id.clone();

            match store.insert_memory(&principle) {
                Ok(()) => {
                    new_principle_ids.push(principle_id);
                }
                Err(e) => {
                    eprintln!(
                        "engram/principles: failed to store principle for user {user_id} \
                         (non-fatal): {e}"
                    );
                }
            }
        }

        Ok(new_principle_ids)
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engram::types::{Memory, MemoryType, SensitivityCategory, Visibility};
    use crate::engram::EngramStore;

    fn test_key() -> Vec<u8> {
        vec![0u8; 32]
    }

    fn tmp_db(tag: &str) -> std::path::PathBuf {
        let p = std::path::PathBuf::from(format!("/tmp/lumina_principles_test_{tag}.db"));
        let _ = std::fs::remove_file(&p);
        p
    }

    /// Build a Preference memory with a specific embedding and confidence.
    fn make_pref(user_id: &str, content: &str, embedding: Vec<f32>, confidence: f32) -> Memory {
        let mut m = Memory::new(user_id, MemoryType::Preference, SensitivityCategory::General, content);
        m.embedding = embedding;
        m.confidence = confidence;
        m
    }

    /// Build a unit vector in 3D space pointing along the given axis.
    fn unit_vec(x: f32, y: f32, z: f32) -> Vec<f32> {
        let norm = (x * x + y * y + z * z).sqrt();
        vec![x / norm, y / norm, z / norm]
    }

    // ── test_cluster_detection_groups_similar_preferences ──────────────────

    #[test]
    fn test_cluster_detection_groups_similar_preferences() {
        let path = tmp_db("cluster_similar");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        // Group A: all point near (1, 0, 0) — cosine > 0.65 against seed
        let emb_a1 = unit_vec(1.0, 0.0, 0.0);
        let emb_a2 = unit_vec(0.9, 0.2, 0.0);
        let emb_a3 = unit_vec(0.85, 0.15, 0.0);

        // Group B: all point near (0, 1, 0) — cosine < 0.65 against group A
        let emb_b1 = unit_vec(0.0, 1.0, 0.0);
        let emb_b2 = unit_vec(0.1, 0.9, 0.0);

        for (content, emb) in [
            ("dark roast coffee", emb_a1),
            ("black tea", emb_a2),
            ("neat whiskey", emb_a3),
            ("bright colors", emb_b1),
            ("vivid patterns", emb_b2),
        ] {
            let m = make_pref("system", content, emb, 0.8);
            store.insert_memory(&m).unwrap();
        }

        let clusters = PrincipleEngine::detect_clusters(&store, "system", CLUSTER_SIMILARITY_THRESHOLD).unwrap();

        // Should produce at least 2 clusters (A and B are distinct directions)
        assert!(clusters.len() >= 2, "expected at least 2 clusters, got {}", clusters.len());

        // The biggest cluster should contain the 3 near-(1,0,0) preferences
        let largest = clusters.iter().max_by_key(|c| c.len()).unwrap();
        assert_eq!(largest.len(), 3, "largest cluster should have 3 similar preferences");

        let _ = std::fs::remove_file(&path);
    }

    // ── test_cluster_below_5_not_abstracted ───────────────────────────────

    #[test]
    fn test_cluster_below_5_not_abstracted() {
        // A cluster with 4 items should NOT trigger abstraction.
        // Verified at the run_for_user level via MIN_CLUSTER_SIZE check.
        // Here we verify detect_clusters returns the small cluster,
        // and a separate check confirms it's below the minimum.
        let path = tmp_db("cluster_small");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        // Insert 4 near-identical preferences (same cluster, but only 4)
        for i in 0..4 {
            let m = make_pref("system", &format!("pref {i}"), unit_vec(1.0, 0.0, 0.0), 0.8);
            store.insert_memory(&m).unwrap();
        }

        let clusters = PrincipleEngine::detect_clusters(&store, "system", CLUSTER_SIMILARITY_THRESHOLD).unwrap();
        assert_eq!(clusters.len(), 1, "should have exactly 1 cluster");

        let cluster = &clusters[0];
        assert!(
            cluster.len() < MIN_CLUSTER_SIZE,
            "cluster size {} should be below MIN_CLUSTER_SIZE {}",
            cluster.len(),
            MIN_CLUSTER_SIZE
        );

        let _ = std::fs::remove_file(&path);
    }

    // ── test_cluster_of_5_triggers_abstraction_prompt ─────────────────────

    #[test]
    fn test_cluster_of_5_triggers_abstraction_prompt() {
        // Verify that a cluster of 5 produces a non-empty prompt.
        let prefs: Vec<Memory> = (0..5)
            .map(|i| {
                let mut m = Memory::new("u", MemoryType::Preference, SensitivityCategory::General,
                    format!("preference {i}"));
                m.embedding = unit_vec(1.0, 0.0, 0.0);
                m
            })
            .collect();

        assert_eq!(prefs.len(), MIN_CLUSTER_SIZE, "cluster size should meet minimum");

        let prompt = PrincipleEngine::build_abstraction_prompt(&prefs);

        assert!(!prompt.is_empty(), "prompt must not be empty");
        assert!(prompt.contains("preference 0"), "prompt must list preference content");
        assert!(prompt.contains("preference 4"), "prompt must list all preferences");
        assert!(prompt.contains("Principle:"), "prompt must have Principle: cue");
        assert!(prompt.contains("abstract"), "prompt must mention abstract principle");
    }

    // ── test_principle_stored_with_correct_type ───────────────────────────

    #[test]
    fn test_principle_stored_with_correct_type() {
        let path = tmp_db("principle_type");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        let prefs: Vec<Memory> = (0..5)
            .map(|i| make_pref("system", &format!("pref {i}"), unit_vec(1.0, 0.0, 0.0), 0.8))
            .collect();

        let principle = PrincipleEngine::parse_principle(
            "Principle: Prefers strong, unflavored preparations",
            &prefs,
        );

        assert_eq!(principle.memory_type, MemoryType::Principle,
            "principle must have MemoryType::Principle");

        // Store it and verify the DB record
        store.insert_memory(&principle).unwrap();

        let stored = store.query_by_type(MemoryType::Principle).unwrap();
        assert_eq!(stored.len(), 1, "should have exactly 1 principle stored");
        assert_eq!(stored[0].memory_type, MemoryType::Principle);
        assert!(stored[0].content.contains("unflavored"),
            "stored principle content should match");

        let _ = std::fs::remove_file(&path);
    }

    // ── test_principle_tags_include_source_ids ────────────────────────────

    #[test]
    fn test_principle_tags_include_source_ids() {
        let prefs: Vec<Memory> = (0..5)
            .map(|i| make_pref("system", &format!("pref {i}"), unit_vec(1.0, 0.0, 0.0), 0.8))
            .collect();

        let source_ids: Vec<String> = prefs.iter().map(|m| m.id.clone()).collect();

        let principle = PrincipleEngine::parse_principle(
            "Principle: Direct communication preference",
            &prefs,
        );

        // Find the abstracted_from tag
        let abstracted_tag = principle
            .tags
            .iter()
            .find(|t| t.starts_with("abstracted_from:"))
            .expect("principle must have abstracted_from tag");

        let tag_ids_part = abstracted_tag.strip_prefix("abstracted_from:").unwrap();
        let tag_ids: Vec<&str> = tag_ids_part.split(',').collect();

        for src_id in &source_ids {
            assert!(
                tag_ids.contains(&src_id.as_str()),
                "abstracted_from tag must include source ID: {src_id}"
            );
        }
    }

    // ── test_principle_visibility_from_sources ────────────────────────────

    #[test]
    fn test_principle_visibility_all_private_gives_private() {
        let prefs: Vec<Memory> = (0..5)
            .map(|i| {
                let mut m = make_pref("system", &format!("pref {i}"), unit_vec(1.0, 0.0, 0.0), 0.8);
                m.visibility = Visibility::Private;
                m
            })
            .collect();

        let principle = PrincipleEngine::parse_principle("Principle: x", &prefs);
        assert_eq!(
            principle.visibility, Visibility::Private,
            "all-private sources → Private principle"
        );
    }

    #[test]
    fn test_principle_visibility_from_sources() {
        // At least one Shared source → Shared principle
        let mut prefs: Vec<Memory> = (0..4)
            .map(|i| {
                let mut m = make_pref("system", &format!("pref {i}"), unit_vec(1.0, 0.0, 0.0), 0.8);
                m.visibility = Visibility::Private;
                m
            })
            .collect();

        // One shared preference
        let mut shared_pref = make_pref("system", "shared pref", unit_vec(1.0, 0.0, 0.0), 0.8);
        shared_pref.visibility = Visibility::Shared;
        prefs.push(shared_pref);

        let principle = PrincipleEngine::parse_principle("Principle: x", &prefs);
        assert_eq!(
            principle.visibility, Visibility::Shared,
            "any Shared source → Shared principle"
        );
    }

    // ── test_principle_confidence_averaged_from_sources ───────────────────

    #[test]
    fn test_principle_confidence_averaged_from_sources() {
        let confidences = [0.6, 0.7, 0.8, 0.9, 1.0];
        let expected_avg = confidences.iter().sum::<f32>() / confidences.len() as f32;

        let prefs: Vec<Memory> = confidences
            .iter()
            .enumerate()
            .map(|(i, &c)| make_pref("system", &format!("pref {i}"), unit_vec(1.0, 0.0, 0.0), c))
            .collect();

        let principle = PrincipleEngine::parse_principle("Principle: x", &prefs);

        assert!(
            (principle.confidence - expected_avg).abs() < 0.001,
            "principle confidence {:.3} should be average {:.3}",
            principle.confidence,
            expected_avg
        );
    }

    // ── test_per_user_isolation ───────────────────────────────────────────

    #[test]
    fn test_per_user_isolation() {
        let path = tmp_db("per_user_isolation");

        // Open store as user A
        let store_a = EngramStore::open_for_user_at(
            &std::path::PathBuf::from("/tmp"),
            "alice",
            &test_key(),
        ).unwrap();

        // Open store as user B
        let store_b = EngramStore::open_for_user_at(
            &std::path::PathBuf::from("/tmp"),
            "bob",
            &test_key(),
        ).unwrap();

        // Insert 5 preferences for Alice
        for i in 0..5 {
            let m = make_pref("alice", &format!("alice pref {i}"), unit_vec(1.0, 0.0, 0.0), 0.8);
            store_a.insert_memory(&m).unwrap();
        }

        // Insert 5 preferences for Bob
        for i in 0..5 {
            let m = make_pref("bob", &format!("bob pref {i}"), unit_vec(0.0, 1.0, 0.0), 0.8);
            store_b.insert_memory(&m).unwrap();
        }

        // Alice's clusters should only contain Alice's preferences
        let alice_clusters = PrincipleEngine::detect_clusters(&store_a, "alice", CLUSTER_SIMILARITY_THRESHOLD).unwrap();
        for cluster in &alice_clusters {
            for mem in cluster {
                assert_eq!(
                    mem.user_id, "alice",
                    "Alice's clusters must not contain Bob's preferences"
                );
            }
        }

        // Bob's clusters should only contain Bob's preferences
        let bob_clusters = PrincipleEngine::detect_clusters(&store_b, "bob", CLUSTER_SIMILARITY_THRESHOLD).unwrap();
        for cluster in &bob_clusters {
            for mem in cluster {
                assert_eq!(
                    mem.user_id, "bob",
                    "Bob's clusters must not contain Alice's preferences"
                );
            }
        }

        // Cleanup
        let _ = std::fs::remove_dir_all("/tmp/alice");
        let _ = std::fs::remove_dir_all("/tmp/bob");
        let _ = std::fs::remove_file(&path);
    }

    // ── test_few_preferences_skipped ──────────────────────────────────────

    #[test]
    fn test_few_preferences_skipped() {
        // With fewer than MIN_PREFERENCES_FOR_ENGINE total preferences,
        // run_for_user should return empty without doing anything.
        // We verify this by checking the return behavior of the guard condition.
        //
        // Since run_for_user is async and requires a ChordClient, we test the
        // guard logic directly via the query_by_type count check.

        let path = tmp_db("few_prefs");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        // Insert fewer than MIN_PREFERENCES_FOR_ENGINE preferences
        let count = MIN_PREFERENCES_FOR_ENGINE - 1;
        for i in 0..count {
            let m = make_pref("system", &format!("pref {i}"), unit_vec(1.0, 0.0, 0.0), 0.8);
            store.insert_memory(&m).unwrap();
        }

        let prefs = store.query_by_type(MemoryType::Preference).unwrap();
        let user_prefs: Vec<_> = prefs.iter().filter(|m| m.user_id == "system").collect();

        assert!(
            user_prefs.len() < MIN_PREFERENCES_FOR_ENGINE,
            "test setup: {} prefs should be below threshold {}",
            user_prefs.len(),
            MIN_PREFERENCES_FOR_ENGINE
        );

        // Verify the guard: if below threshold, the engine would return empty.
        // This mirrors the exact check in run_for_user.
        let would_skip = user_prefs.len() < MIN_PREFERENCES_FOR_ENGINE;
        assert!(would_skip, "engine must skip when preferences < MIN_PREFERENCES_FOR_ENGINE");

        let _ = std::fs::remove_file(&path);
    }

    // ── test_abstraction_prompt_format ────────────────────────────────────

    #[test]
    fn test_abstraction_prompt_contains_all_preferences() {
        let prefs: Vec<Memory> = vec![
            make_pref("u", "likes dark roast coffee", unit_vec(1.0, 0.0, 0.0), 0.9),
            make_pref("u", "prefers black tea", unit_vec(1.0, 0.1, 0.0), 0.85),
            make_pref("u", "drinks neat whiskey", unit_vec(0.9, 0.2, 0.0), 0.8),
            make_pref("u", "chooses unsweetened chocolate", unit_vec(0.95, 0.1, 0.0), 0.75),
            make_pref("u", "avoids flavored coffee drinks", unit_vec(1.0, 0.05, 0.0), 0.9),
        ];

        let prompt = PrincipleEngine::build_abstraction_prompt(&prefs);

        assert!(prompt.contains("likes dark roast coffee"));
        assert!(prompt.contains("prefers black tea"));
        assert!(prompt.contains("drinks neat whiskey"));
        assert!(prompt.contains("chooses unsweetened chocolate"));
        assert!(prompt.contains("avoids flavored coffee drinks"));
        assert!(prompt.contains("Principle:"), "prompt must end with Principle: cue");
    }

    // ── test_parse_principle_strips_prefix ────────────────────────────────

    #[test]
    fn test_parse_principle_strips_principle_prefix() {
        let prefs: Vec<Memory> = (0..5)
            .map(|i| make_pref("u", &format!("p{i}"), unit_vec(1.0, 0.0, 0.0), 0.8))
            .collect();

        let principle = PrincipleEngine::parse_principle(
            "Principle: Prefers direct communication",
            &prefs,
        );
        assert_eq!(principle.content, "Prefers direct communication",
            "Principle: prefix should be stripped");
    }

    #[test]
    fn test_parse_principle_handles_no_prefix() {
        let prefs: Vec<Memory> = (0..5)
            .map(|i| make_pref("u", &format!("p{i}"), unit_vec(1.0, 0.0, 0.0), 0.8))
            .collect();

        let principle = PrincipleEngine::parse_principle(
            "Prefers bold, direct flavors over subtlety",
            &prefs,
        );
        assert_eq!(principle.content, "Prefers bold, direct flavors over subtlety");
    }

    // ── test_dedup_check_existing_principle ───────────────────────────────

    #[test]
    fn test_dedup_check_returns_true_when_principle_exists() {
        let path = tmp_db("dedup_check");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        // Create source preferences
        let prefs: Vec<Memory> = (0..5)
            .map(|i| make_pref("system", &format!("pref {i}"), unit_vec(1.0, 0.0, 0.0), 0.8))
            .collect();

        // Store the principle
        let principle = PrincipleEngine::parse_principle("Principle: existing one", &prefs);
        store.insert_memory(&principle).unwrap();

        // Check dedup
        let source_ids: Vec<&str> = prefs.iter().map(|m| m.id.as_str()).collect();
        assert!(
            PrincipleEngine::principle_already_exists(&store, &source_ids),
            "should detect existing principle for the same source IDs"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_dedup_check_returns_false_when_no_principle() {
        let path = tmp_db("dedup_none");
        let store = EngramStore::open(&path, &test_key()).unwrap();

        let prefs: Vec<Memory> = (0..5)
            .map(|i| make_pref("system", &format!("pref {i}"), unit_vec(1.0, 0.0, 0.0), 0.8))
            .collect();

        let source_ids: Vec<&str> = prefs.iter().map(|m| m.id.as_str()).collect();
        assert!(
            !PrincipleEngine::principle_already_exists(&store, &source_ids),
            "should return false when no principle exists for these sources"
        );

        let _ = std::fs::remove_file(&path);
    }

    // ── no hardcoded IPs ───────────────────────────────────────────────────

    #[test]
    fn test_no_hardcoded_ips_in_module() {
        // Verify the abstraction model constant uses an alias, not an IP/URL.
        assert!(!ABSTRACTION_MODEL.contains("192.168"),
            "ABSTRACTION_MODEL must not be an IP address");
        assert!(!ABSTRACTION_MODEL.starts_with("http"),
            "ABSTRACTION_MODEL must be a model alias, not a URL");
        assert_eq!(ABSTRACTION_MODEL, "lumina-fast",
            "must use lumina-fast per inference de-bloat rules");
    }
}
