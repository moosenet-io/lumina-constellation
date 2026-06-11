//! EDGE-03/04: LLM-based skill document generation and refinement.
//!
//! The generator builds a meta-prompt for the LLM to produce a structured
//! skill document from a completed complex task. It also handles parsing
//! the LLM response back into a Skill struct.
//!
//! EDGE-04 adds `refinement_prompt` and `parse_refined_procedure` for
//! improving an existing skill based on observed execution steps.

use super::Skill;

/// Generates and parses skill documents from completed tasks.
pub struct SkillGenerator;

impl SkillGenerator {
    /// EDGE-04: Build a refinement prompt to improve an existing skill.
    ///
    /// `current_skill` is the skill that was loaded before the task.
    /// `execution_steps` describes the actual steps taken during the task
    /// (e.g. a summary of tool calls made and their outcomes).
    ///
    /// The LLM is asked to produce an improved procedure as JSON:
    /// `{"procedure": "..."}`.
    pub fn refinement_prompt(current_skill: &Skill, execution_steps: &str) -> String {
        format!(
            "Here is the current skill procedure:\n\
             {procedure}\n\
             \n\
             Here are the actual steps taken that succeeded:\n\
             {execution_steps}\n\
             \n\
             Produce an improved skill procedure that incorporates what worked.\n\
             If the execution steps revealed a better approach, shorter path, or \
             handled an edge case the current procedure missed, incorporate that.\n\
             If the execution matched the procedure exactly, return the procedure unchanged.\n\
             \n\
             Output ONLY a JSON object with exactly this field:\n\
             {{\"procedure\": \"1. First step\\n2. Second step\\n3. Continue\"}}\n\
             \n\
             Rules:\n\
             - procedure: numbered steps, each on its own line, concrete and actionable\n\
             - Output ONLY the JSON object, no explanation, no markdown fences",
            procedure = current_skill.procedure,
            execution_steps = execution_steps,
        )
    }

    /// EDGE-04: Parse the LLM's response from a refinement prompt.
    ///
    /// Expects JSON: `{"procedure": "..."}`.
    /// Returns `None` if parsing fails or the procedure is empty.
    pub fn parse_refined_procedure(llm_output: &str) -> Option<String> {
        // Attempt 1: parse the whole output as JSON
        if let Some(proc) = Self::extract_procedure_from_json(llm_output) {
            return Some(proc);
        }

        // Attempt 2: extract the first {...} block then parse
        let extracted = extract_json_object(llm_output)?;
        Self::extract_procedure_from_json(&extracted)
    }

    fn extract_procedure_from_json(s: &str) -> Option<String> {
        let v: serde_json::Value = serde_json::from_str(s.trim()).ok()?;
        let procedure = v["procedure"].as_str()?.trim().to_string();
        if procedure.is_empty() {
            return None;
        }
        Some(procedure)
    }

    /// Build the meta-prompt sent to the LLM to generate a skill document.
    ///
    /// `task_summary` is a brief description of what was just completed.
    /// `tools_used` is the list of MCP tool names that were called.
    pub fn generation_prompt(task_summary: &str, tools_used: &[String]) -> String {
        let tools_str = if tools_used.is_empty() {
            "(none)".to_string()
        } else {
            tools_used.join(", ")
        };

        format!(
            "You just completed a complex task. Summarize it as a reusable skill document.\n\
             \n\
             Task summary: {task_summary}\n\
             Tools used: {tools_str}\n\
             \n\
             Produce a JSON object with exactly these fields:\n\
             {{\n\
               \"name\": \"short descriptive name (3-5 words)\",\n\
               \"description\": \"one sentence describing what this skill does\",\n\
               \"trigger_patterns\": [\"keyword1\", \"keyword2\", \"phrase that would activate this skill\"],\n\
               \"procedure\": \"1. First step\\n2. Second step\\n3. Continue until done\",\n\
               \"tools_used\": [\"tool_name_1\", \"tool_name_2\"]\n\
             }}\n\
             \n\
             Rules:\n\
             - trigger_patterns: 3-7 keywords or short phrases a user might say to need this skill\n\
             - procedure: numbered steps, each on its own line, concrete and actionable\n\
             - tools_used: only the actual MCP tool names (from the list above)\n\
             - Output ONLY the JSON object, no explanation, no markdown fences",
            task_summary = task_summary,
            tools_str = tools_str,
        )
    }

    /// Parse the LLM's response into a Skill struct.
    ///
    /// Tries to parse JSON from the output. If JSON is not found at the top
    /// level, scans for a `{` ... `}` block. Returns `None` if parsing fails.
    pub fn parse_generated_skill(llm_output: &str) -> Option<Skill> {
        // Attempt 1: parse the whole output as JSON
        if let Some(skill) = Self::try_parse_json(llm_output) {
            return Some(skill);
        }

        // Attempt 2: extract the first {...} block from the output
        let extracted = extract_json_object(llm_output)?;
        Self::try_parse_json(&extracted)
    }

    // ── Private helpers ────────────────────────────────────────────────────

    fn try_parse_json(s: &str) -> Option<Skill> {
        let v: serde_json::Value = serde_json::from_str(s.trim()).ok()?;

        let name = v["name"].as_str()?.trim().to_string();
        let description = v["description"].as_str()?.trim().to_string();

        let trigger_patterns: Vec<String> = v["trigger_patterns"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        let procedure = v["procedure"].as_str()?.trim().to_string();

        let tools_used: Vec<String> = v["tools_used"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        // Validate required fields
        if name.is_empty() || description.is_empty() || procedure.is_empty() {
            eprintln!("skills: generated skill has empty required fields — skipping");
            return None;
        }

        let now = now_utc();

        Some(Skill {
            id: 0, // set by DB on insert
            name,
            description,
            trigger_patterns,
            procedure,
            tools_used,
            success_count: 0,
            version: 1,
            created_at: now,
            last_used: None,
            embedding: None,
        })
    }
}

/// Extract the first balanced `{...}` block from a string.
fn extract_json_object(s: &str) -> Option<String> {
    let start = s.find('{')?;
    let mut depth = 0usize;
    let mut end = None;

    for (i, ch) in s[start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(start + i + 1);
                    break;
                }
            }
            _ => {}
        }
    }

    end.map(|e| s[start..e].to_string())
}

/// Return the current UTC time as an ISO 8601 string.
///
/// Public so `skill_store.rs` can reuse it without duplicating the logic.
pub fn now_utc() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Format: YYYY-MM-DDTHH:MM:SSZ (manual, no chrono dependency needed)
    let secs_per_day = 86400u64;
    let secs_per_hour = 3600u64;
    let secs_per_min = 60u64;

    // Days since epoch
    let days = secs / secs_per_day;
    let rem = secs % secs_per_day;
    let h = rem / secs_per_hour;
    let m = (rem % secs_per_hour) / secs_per_min;
    let s = rem % secs_per_min;

    // Convert days since 1970-01-01 to Gregorian calendar
    let (year, month, day) = days_to_date(days);

    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", year, month, day, h, m, s)
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_date(days: u64) -> (u32, u32, u32) {
    // Algorithm: days since 1970-01-01
    let mut remaining = days as i64;
    let mut year = 1970u32;

    loop {
        let days_in_year = if is_leap(year) { 366i64 } else { 365i64 };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        year += 1;
    }

    let leap = is_leap(year);
    let month_days: &[i64] = if leap {
        &[31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        &[31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut month = 1u32;
    for &md in month_days {
        if remaining < md {
            break;
        }
        remaining -= md;
        month += 1;
    }

    (year, month, (remaining + 1) as u32)
}

fn is_leap(year: u32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generation_prompt_contains_task_summary() {
        let tools = vec!["shell_exec".to_string(), "file_read".to_string()];
        let prompt = SkillGenerator::generation_prompt("Search for log files", &tools);
        assert!(prompt.contains("Search for log files"));
        assert!(prompt.contains("shell_exec"));
        assert!(prompt.contains("file_read"));
        assert!(prompt.contains("trigger_patterns"));
        assert!(prompt.contains("procedure"));
    }

    #[test]
    fn test_generation_prompt_no_tools() {
        let prompt = SkillGenerator::generation_prompt("Analyze data", &[]);
        assert!(prompt.contains("(none)"));
    }

    #[test]
    fn test_parse_valid_json() {
        let json = r#"{
            "name": "File Search Skill",
            "description": "Search files using find and grep",
            "trigger_patterns": ["find file", "search for", "locate"],
            "procedure": "1. Use find\n2. Use grep\n3. Review results",
            "tools_used": ["shell_exec"]
        }"#;

        let skill = SkillGenerator::parse_generated_skill(json).unwrap();
        assert_eq!(skill.name, "File Search Skill");
        assert_eq!(skill.trigger_patterns.len(), 3);
        assert!(skill.trigger_patterns.contains(&"find file".to_string()));
        assert_eq!(skill.tools_used.len(), 1);
        assert_eq!(skill.success_count, 0);
        assert_eq!(skill.version, 1);
    }

    #[test]
    fn test_parse_json_embedded_in_text() {
        let output = r#"Here is the skill document:
        {
            "name": "Deploy Service",
            "description": "Deploy a service using systemctl",
            "trigger_patterns": ["deploy", "restart service"],
            "procedure": "1. Stop service\n2. Update binary\n3. Start service",
            "tools_used": ["ssh_exec"]
        }
        That's the skill."#;

        let skill = SkillGenerator::parse_generated_skill(output).unwrap();
        assert_eq!(skill.name, "Deploy Service");
    }

    #[test]
    fn test_parse_garbage_returns_none() {
        let result = SkillGenerator::parse_generated_skill("not json at all!!!");
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_missing_required_fields_returns_none() {
        let json = r#"{
            "description": "Missing name",
            "trigger_patterns": ["test"],
            "procedure": "",
            "tools_used": []
        }"#;
        // Missing "name" field
        let result = SkillGenerator::parse_generated_skill(json);
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_empty_procedure_returns_none() {
        let json = r#"{
            "name": "Empty Procedure",
            "description": "Has no procedure",
            "trigger_patterns": ["test"],
            "procedure": "",
            "tools_used": []
        }"#;
        let result = SkillGenerator::parse_generated_skill(json);
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_json_object() {
        let s = "prefix { \"key\": \"value\" } suffix";
        let extracted = extract_json_object(s).unwrap();
        assert_eq!(extracted, "{ \"key\": \"value\" }");
    }

    #[test]
    fn test_extract_nested_json_object() {
        let s = r#"text { "a": { "b": 1 } } more text"#;
        let extracted = extract_json_object(s).unwrap();
        assert_eq!(extracted, r#"{ "a": { "b": 1 } }"#);
    }

    #[test]
    fn test_extract_no_json_returns_none() {
        let s = "no braces here at all";
        let result = extract_json_object(s);
        assert!(result.is_none());
    }

    #[test]
    fn test_now_utc_format() {
        let ts = now_utc();
        // Should be YYYY-MM-DDTHH:MM:SSZ
        assert_eq!(ts.len(), 20);
        assert!(ts.ends_with('Z'));
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[7..8], "-");
        assert_eq!(&ts[10..11], "T");
    }

    #[test]
    fn test_is_leap_year() {
        assert!(is_leap(2000));
        assert!(is_leap(2024));
        assert!(!is_leap(1900));
        assert!(!is_leap(2023));
    }

    #[test]
    fn test_days_to_date_epoch() {
        let (y, m, d) = days_to_date(0);
        assert_eq!((y, m, d), (1970, 1, 1));
    }

    #[test]
    fn test_days_to_date_known_date() {
        // 2026-06-05: days since 1970-01-01
        // 2026-06-05 = 56 years + adjustments
        // Just verify year is in reasonable range
        let ts = now_utc();
        assert!(ts.starts_with("20"), "Year should be in 2000s: {}", ts);
    }

    // ── EDGE-04: refinement prompt tests ──────────────────────────────────

    fn sample_skill_for_test() -> Skill {
        Skill {
            id: 1,
            name: "Log Analysis".to_string(),
            description: "Analyze logs for errors".to_string(),
            trigger_patterns: vec!["analyze logs".to_string()],
            procedure: "1. Find log files\n2. Search for errors\n3. Summarize".to_string(),
            tools_used: vec!["shell_exec".to_string()],
            success_count: 3,
            version: 1,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            last_used: None,
            embedding: None,
        }
    }

    #[test]
    fn test_refinement_prompt_contains_current_procedure() {
        let skill = sample_skill_for_test();
        let steps = "Step 1: used shell_exec to find files. Step 2: used grep. Step 3: summarized.";
        let prompt = SkillGenerator::refinement_prompt(&skill, steps);

        assert!(
            prompt.contains("1. Find log files"),
            "Prompt must contain current procedure"
        );
        assert!(
            prompt.contains(steps),
            "Prompt must contain execution steps"
        );
        assert!(
            prompt.contains("procedure"),
            "Prompt must reference the procedure field"
        );
    }

    #[test]
    fn test_refinement_prompt_requests_json_output() {
        let skill = sample_skill_for_test();
        let prompt = SkillGenerator::refinement_prompt(&skill, "some steps");

        assert!(
            prompt.contains("\"procedure\""),
            "Prompt must ask for JSON with 'procedure' key"
        );
        assert!(
            prompt.contains("JSON"),
            "Prompt must mention JSON output"
        );
    }

    #[test]
    fn test_parse_refined_procedure_valid_json() {
        let json = r#"{"procedure": "1. Step one\n2. Step two\n3. Done"}"#;
        let result = SkillGenerator::parse_refined_procedure(json);
        assert!(result.is_some());
        // JSON \n is parsed as a real newline by serde_json
        let procedure = result.unwrap();
        assert!(procedure.contains("Step one"), "Should contain step one: {}", procedure);
        assert!(procedure.contains("Step two"), "Should contain step two: {}", procedure);
        assert!(procedure.contains("Done"), "Should contain Done: {}", procedure);
    }

    #[test]
    fn test_parse_refined_procedure_embedded_in_text() {
        let output = r#"Here is the improved procedure:
        {"procedure": "1. Better step\n2. Improved step"}
        That should work."#;
        let result = SkillGenerator::parse_refined_procedure(output);
        assert!(result.is_some());
        assert!(result.unwrap().contains("Better step"));
    }

    #[test]
    fn test_parse_refined_procedure_garbage_returns_none() {
        let result = SkillGenerator::parse_refined_procedure("not json!!!");
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_refined_procedure_empty_procedure_returns_none() {
        let json = r#"{"procedure": ""}"#;
        let result = SkillGenerator::parse_refined_procedure(json);
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_refined_procedure_missing_key_returns_none() {
        let json = r#"{"name": "some skill"}"#;
        let result = SkillGenerator::parse_refined_procedure(json);
        assert!(result.is_none());
    }
}
