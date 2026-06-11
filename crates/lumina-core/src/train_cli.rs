//! FORGE-05: Training data curation CLI.
//! FORGE-07: Adds `train schedule` and `train mark-trained` subcommands.
//! EDGE-03: Adds `train skills` subcommands for skill review, edit, and deletion.
//! EDGE-04: Adds `train skills history` and `train skills rollback` subcommands.
//!
//! Subcommand: `lumina-core train <subcommand>`
//!
//! review [--limit N]               Show pending turns (default 10)
//! approve <id>                     Approve a turn
//! reject  <id>                     Reject a turn
//! edit    <id> <output>            Store corrected assistant output
//! export  <path>                   Export approved+edited turns to JSONL
//! stats                            Show dataset statistics
//! schedule                         Show retraining schedule check result (FORGE-07)
//! mark-trained                     Record training timestamp in vault (FORGE-07)
//! skills list                      List all stored skills
//! skills view <id>                 Show a skill by ID
//! skills edit <id> <procedure>     Update a skill's procedure (saves version)
//! skills delete <id>               Delete a skill
//! skills history <id>              Show all versions of a skill (EDGE-04)
//! skills rollback <id> <version>   Restore a skill to a previous version (EDGE-04)

use crate::error::{LuminaError, Result};
use crate::retrain_scheduler::RetrainScheduler;
use crate::skills::open_default_skill_engine;
use crate::training_store::TrainingStore;
use std::path::Path;

const DEFAULT_REVIEW_LIMIT: usize = 10;
const DEFAULT_SYSTEM_PROMPT: &str = "You are Lumina, a helpful AI assistant.";
const TRUNCATE_LEN: usize = 80;

/// Handle `lumina-core train <args>`.
/// Returns `true` when a train subcommand was dispatched (caller should exit cleanly).
pub fn handle_train_command(args: &[String]) -> bool {
    if args.len() < 2 || args[1] != "train" {
        return false;
    }

    let sub = args.get(2).map(|s| s.as_str()).unwrap_or("help");

    let result = match sub {
        "review" => cmd_review(args),
        "approve" => cmd_approve(args),
        "reject" => cmd_reject(args),
        "edit" => cmd_edit(args),
        "export" => cmd_export(args),
        "stats" => cmd_stats(),
        "schedule" => cmd_schedule(),
        "mark-trained" => cmd_mark_trained(),
        "skills" => cmd_skills(args),
        _ => {
            print_train_help();
            return true;
        }
    };

    if let Err(e) = result {
        eprintln!("train {}: {}", sub, e);
        std::process::exit(1);
    }

    true
}

// ── Subcommand implementations ────────────────────────────────────────────

fn cmd_review(args: &[String]) -> Result<()> {
    let limit = parse_limit_flag(args).unwrap_or(DEFAULT_REVIEW_LIMIT);

    let store = open_store()?;
    let pending = store.get_pending(limit)?;

    if pending.is_empty() {
        println!("No pending turns.");
        return Ok(());
    }

    println!("Pending turns ({} shown):\n", pending.len());
    for (id, turn) in &pending {
        let user = truncate(&turn.user_input, TRUNCATE_LEN);
        let asst = truncate(&turn.assistant_output, TRUNCATE_LEN);
        println!("ID {id}  model={model}  {esc}",
            id = id,
            model = turn.model_used,
            esc = if turn.escalated { "[escalated]" } else { "" }
        );
        println!("  USER: {user}");
        println!("  ASST: {asst}");
        println!();
    }

    Ok(())
}

fn cmd_approve(args: &[String]) -> Result<()> {
    let id = parse_id_arg(args, "approve")?;
    let store = open_store()?;
    store.mark_curated(id, "approved", None)?;
    println!("Turn {id} approved.");
    Ok(())
}

fn cmd_reject(args: &[String]) -> Result<()> {
    let id = parse_id_arg(args, "reject")?;
    let store = open_store()?;
    store.mark_curated(id, "rejected", None)?;
    println!("Turn {id} rejected.");
    Ok(())
}

fn cmd_edit(args: &[String]) -> Result<()> {
    let id = parse_id_arg(args, "edit")?;
    let new_output = args.get(4).ok_or_else(|| {
        LuminaError::Config("Usage: train edit <id> <new_output>".to_string())
    })?;
    let store = open_store()?;
    store.mark_curated(id, "edited", Some(new_output.as_str()))?;
    println!("Turn {id} edited.");
    Ok(())
}

fn cmd_export(args: &[String]) -> Result<()> {
    let path_str = args.get(3).ok_or_else(|| {
        LuminaError::Config("Usage: train export <output.jsonl>".to_string())
    })?;

    let system_prompt = std::env::var("LUMINA_SYSTEM_PROMPT")
        .unwrap_or_else(|_| DEFAULT_SYSTEM_PROMPT.to_string());

    let store = open_store()?;
    let count = store.export_jsonl(Path::new(path_str), &system_prompt)?;
    println!("Exported {count} turn(s) to {path_str}");
    Ok(())
}

fn cmd_stats() -> Result<()> {
    let store = open_store()?;
    let stats = store.stats()?;

    println!("Training dataset statistics:");
    println!("  Total:    {}", stats.total_turns);
    println!("  Pending:  {}", stats.pending);
    println!("  Approved: {}", stats.approved);
    println!("  Rejected: {}", stats.rejected);
    println!("  Edited:   {}", stats.edited);
    if let Some(oldest) = &stats.oldest {
        println!("  Oldest:   {oldest}");
    }
    if let Some(newest) = &stats.newest {
        println!("  Newest:   {newest}");
    }
    Ok(())
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn open_store() -> Result<TrainingStore> {
    TrainingStore::open_default().map_err(|e| {
        LuminaError::Config(format!(
            "Cannot open training database: {e}\n\
             Hint: start lumina-core once to initialise the vault and database."
        ))
    })
}

fn parse_id_arg(args: &[String], sub: &str) -> Result<i64> {
    let raw = args.get(3).ok_or_else(|| {
        LuminaError::Config(format!("Usage: train {sub} <id>"))
    })?;
    raw.parse::<i64>().map_err(|_| {
        LuminaError::Config(format!("Invalid ID '{raw}' — must be an integer"))
    })
}

fn parse_limit_flag(args: &[String]) -> Option<usize> {
    // Accept `--limit N` anywhere in the arg list
    for (i, arg) in args.iter().enumerate() {
        if arg == "--limit" {
            return args.get(i + 1)?.parse().ok();
        }
    }
    None
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}

fn cmd_schedule() -> Result<()> {
    let store = open_store()?;
    let stats = store.stats()?;
    let scheduler = RetrainScheduler::from_env();
    let decision = scheduler.check(&stats);

    println!("Retraining schedule check:");
    println!("  Should retrain: {}", decision.should_retrain);
    println!("  Reason:         {}", decision.reason);
    println!("  Curated count:  {}", decision.approved_count);
    if let Some(days) = decision.days_since_last {
        println!("  Days since last: {days}");
    } else {
        println!("  Days since last: never");
    }
    Ok(())
}

fn cmd_mark_trained() -> Result<()> {
    let scheduler = RetrainScheduler::from_env();
    scheduler.mark_trained()?;
    println!("Training timestamp recorded in vault.");
    Ok(())
}

/// EDGE-03: Skill management subcommands.
///
/// Usage: lumina-core train skills <list|view|edit|delete> [args]
fn cmd_skills(args: &[String]) -> Result<()> {
    let skill_sub = args.get(3).map(|s| s.as_str()).unwrap_or("list");

    let mut engine = open_default_skill_engine().ok_or_else(|| {
        LuminaError::Config(
            "Cannot open skills database. Run lumina-core once to initialise the vault."
                .to_string(),
        )
    })?;

    match skill_sub {
        "list" => {
            let skills = engine.list_skills()?;
            if skills.is_empty() {
                println!("No skills stored.");
                return Ok(());
            }
            println!("Stored skills ({} total):\n", skills.len());
            for skill in &skills {
                println!(
                    "ID {}  v{}  success={}  name={}",
                    skill.id, skill.version, skill.success_count, skill.name
                );
                println!("  {}", truncate(&skill.description, TRUNCATE_LEN));
                println!("  triggers: {}", skill.trigger_patterns.join(", "));
                println!();
            }
        }

        "view" => {
            let id = parse_id_arg(args, "skills view")?;
            let skill = engine.get_skill(id)?.ok_or_else(|| {
                LuminaError::Config(format!("No skill found with ID {id}"))
            })?;
            println!("Skill ID: {}", skill.id);
            println!("Name:     {}", skill.name);
            println!("Version:  {}", skill.version);
            println!("Success:  {}", skill.success_count);
            println!("Created:  {}", skill.created_at);
            if let Some(ref lu) = skill.last_used {
                println!("Last use: {}", lu);
            }
            println!();
            println!("Description:\n{}", skill.description);
            println!();
            println!("Trigger patterns:");
            for pat in &skill.trigger_patterns {
                println!("  - {}", pat);
            }
            println!();
            println!("Procedure:\n{}", skill.procedure);
            if !skill.tools_used.is_empty() {
                println!("\nTools: {}", skill.tools_used.join(", "));
            }
        }

        "edit" => {
            let id = parse_id_arg(args, "skills edit")?;
            let procedure = args.get(5).ok_or_else(|| {
                LuminaError::Config("Usage: train skills edit <id> <procedure>".to_string())
            })?;
            engine.update_skill_procedure(id, procedure)?;
            println!("Skill {id} procedure updated.");
        }

        "delete" => {
            let id = parse_id_arg(args, "skills delete")?;
            engine.delete_skill(id)?;
            println!("Skill {id} deleted.");
        }

        // EDGE-04: version history and rollback
        "history" => {
            let id = parse_id_arg(args, "skills history")?;
            let history = engine.get_history(id)?;
            if history.is_empty() {
                println!("No version history for skill {id}.");
                return Ok(());
            }
            println!("Version history for skill {} ({} entries):\n", id, history.len());
            for v in &history {
                println!(
                    "  v{}  saved={}",
                    v.version, v.created_at
                );
                println!("  {}\n", truncate(&v.procedure, TRUNCATE_LEN));
            }
        }

        "rollback" => {
            let id = parse_id_arg(args, "skills rollback")?;
            let version_str = args.get(5).ok_or_else(|| {
                LuminaError::Config(
                    "Usage: train skills rollback <id> <version>".to_string(),
                )
            })?;
            let version: i64 = version_str.parse().map_err(|_| {
                LuminaError::Config(format!(
                    "Invalid version '{}'. Must be a positive integer.",
                    version_str
                ))
            })?;
            engine.rollback(id, version)?;
            println!("Skill {id} rolled back to version {version}.");
        }

        _ => {
            eprintln!(
                "Usage: lumina-core train skills \
                 <list|view|edit|delete|history|rollback> [args]"
            );
        }
    }

    Ok(())
}

pub fn print_train_help() {
    eprintln!("Usage: lumina-core train <subcommand>");
    eprintln!();
    eprintln!("Subcommands:");
    eprintln!("  review [--limit N]               Show pending turns (default {DEFAULT_REVIEW_LIMIT})");
    eprintln!("  approve <id>                     Approve a turn by ID");
    eprintln!("  reject  <id>                     Reject a turn by ID");
    eprintln!("  edit    <id> <output>            Store corrected assistant output");
    eprintln!("  export  <path.jsonl>             Export approved+edited turns to JSONL");
    eprintln!("  stats                            Show dataset statistics");
    eprintln!("  schedule                         Show retraining schedule decision (FORGE-07)");
    eprintln!("  mark-trained                     Record training timestamp in vault (FORGE-07)");
    eprintln!("  skills list                      List all stored skills (EDGE-03)");
    eprintln!("  skills view <id>                 View a skill by ID (EDGE-03)");
    eprintln!("  skills edit <id> <procedure>     Update a skill's procedure (EDGE-03)");
    eprintln!("  skills delete <id>               Delete a skill (EDGE-03)");
    eprintln!();
    eprintln!("Environment:");
    eprintln!("  LUMINA_SYSTEM_PROMPT      System prompt prepended to export (default built-in)");
    eprintln!("  MIN_TRAINING_SAMPLES      Curated samples needed before retraining (default 50)");
    eprintln!("  TRAINING_INTERVAL_DAYS    Minimum days between training runs (default 7)");
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::training_store::{ConversationTurn, TrainingStore};
    use std::path::PathBuf;

    fn tmp_db(label: &str) -> PathBuf {
        let p = PathBuf::from(format!("/tmp/train_cli_test_{label}.db"));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn sample_turn() -> ConversationTurn {
        ConversationTurn {
            session_id: "s1".to_string(),
            user_id: "system".to_string(),
            user_input: "What time is it?".to_string(),
            assistant_output: "It is noon.".to_string(),
            system_prompt: None,
            model_used: "lumina-fast".to_string(),
            escalated: false,
            router_decision: "chat".to_string(),
            duration_ms: 50,
        }
    }

    fn key() -> Vec<u8> { vec![0u8; 32] }

    // handle_train_command returns false when "train" not in argv[1]
    #[test]
    fn test_not_a_train_command() {
        let args = vec!["lumina-core".to_string(), "vault".to_string()];
        assert!(!handle_train_command(&args));
    }

    // handle_train_command returns true for any train subcommand (even unknown → help)
    #[test]
    fn test_train_unknown_subcommand_returns_true() {
        let args = vec![
            "lumina-core".to_string(),
            "train".to_string(),
            "bogus".to_string(),
        ];
        assert!(handle_train_command(&args));
    }

    // truncate: short strings pass through unchanged
    #[test]
    fn test_truncate_short() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    // truncate: long strings get ellipsis
    #[test]
    fn test_truncate_long() {
        let s = "a".repeat(100);
        let t = truncate(&s, 10);
        assert!(t.ends_with('…'));
        // char count: 10 + 1 ellipsis = 11
        assert_eq!(t.chars().count(), 11);
    }

    // parse_limit_flag extracts --limit N
    #[test]
    fn test_parse_limit_flag() {
        let args = vec![
            "lumina-core".to_string(),
            "train".to_string(),
            "review".to_string(),
            "--limit".to_string(),
            "5".to_string(),
        ];
        assert_eq!(parse_limit_flag(&args), Some(5));
    }

    // parse_limit_flag returns None when flag absent
    #[test]
    fn test_parse_limit_flag_absent() {
        let args = vec!["lumina-core".to_string(), "train".to_string()];
        assert_eq!(parse_limit_flag(&args), None);
    }

    // parse_id_arg rejects non-integer
    #[test]
    fn test_parse_id_invalid() {
        let args = vec![
            "lumina-core".to_string(),
            "train".to_string(),
            "approve".to_string(),
            "notanumber".to_string(),
        ];
        assert!(parse_id_arg(&args, "approve").is_err());
    }

    // parse_id_arg parses valid integer
    #[test]
    fn test_parse_id_valid() {
        let args = vec![
            "lumina-core".to_string(),
            "train".to_string(),
            "approve".to_string(),
            "42".to_string(),
        ];
        assert_eq!(parse_id_arg(&args, "approve").unwrap(), 42);
    }

    // cmd_stats returns Ok on empty database
    #[test]
    fn test_stats_empty_db() {
        // Use TrainingStore directly to verify stats path doesn't panic on empty
        let path = tmp_db("stats_empty");
        let store = TrainingStore::open(&path, &key()).unwrap();
        let stats = store.stats().unwrap();
        assert_eq!(stats.total_turns, 0);
        let _ = std::fs::remove_file(&path);
    }

    // export with no curated turns writes nothing
    #[test]
    fn test_export_empty() {
        let db = tmp_db("export_empty");
        let out = PathBuf::from("/tmp/train_cli_export_empty.jsonl");
        let store = TrainingStore::open(&db, &key()).unwrap();
        store.insert_turn(&sample_turn()).unwrap();
        let count = store.export_jsonl(&out, "sys").unwrap();
        assert_eq!(count, 0);
        let _ = std::fs::remove_file(&db);
        let _ = std::fs::remove_file(&out);
    }

    // full approve → export roundtrip
    #[test]
    fn test_approve_then_export() {
        let db = tmp_db("approve_export");
        let out = PathBuf::from("/tmp/train_cli_approve_export.jsonl");
        let store = TrainingStore::open(&db, &key()).unwrap();
        let id = store.insert_turn(&sample_turn()).unwrap();
        store.mark_curated(id, "approved", None).unwrap();
        let count = store.export_jsonl(&out, "You are Lumina.").unwrap();
        assert_eq!(count, 1);
        let content = std::fs::read_to_string(&out).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(parsed["messages"][1]["content"], "What time is it?");
        let _ = std::fs::remove_file(&db);
        let _ = std::fs::remove_file(&out);
    }

    // ── EDGE-04 CLI tests ──────────────────────────────────────────────────

    fn skill_engine_tmp(label: &str) -> crate::skills::SkillEngine {
        let p = PathBuf::from(format!("/tmp/train_cli_skill_{label}.db"));
        let _ = std::fs::remove_file(&p);
        crate::skills::SkillEngine::new(&p, &key()).unwrap()
    }

    fn sample_skill() -> crate::skills::Skill {
        crate::skills::Skill {
            id: 0,
            name: "Test Skill".to_string(),
            description: "A test skill".to_string(),
            trigger_patterns: vec!["test".to_string()],
            procedure: "1. Do this\n2. Do that".to_string(),
            tools_used: vec![],
            success_count: 0,
            version: 1,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            last_used: None,
            embedding: None,
        }
    }

    #[test]
    fn test_skills_history_subcommand_shows_versions() {
        let mut engine = skill_engine_tmp("history_cli");
        let id = engine.store_skill(&sample_skill()).unwrap();

        // Update skill twice to build history
        engine.update_skill(id, "Improved v2").unwrap();
        engine.update_skill(id, "Improved v3").unwrap();

        let history = engine.get_history(id).unwrap();
        assert_eq!(history.len(), 2, "Should have v1 and v2 in history");
        assert_eq!(history[0].version, 1);
        assert_eq!(history[1].version, 2);

        let _ = std::fs::remove_file(format!("/tmp/train_cli_skill_history_cli.db"));
    }

    #[test]
    fn test_skills_rollback_subcommand() {
        let mut engine = skill_engine_tmp("rollback_cli");
        let id = engine.store_skill(&sample_skill()).unwrap();

        let orig_procedure = sample_skill().procedure;
        engine.update_skill(id, "Worse procedure").unwrap();

        // Rollback to v1 — also snapshots v2 into history
        engine.rollback(id, 1).unwrap();

        let skill = engine.get_skill(id).unwrap().unwrap();
        assert_eq!(skill.procedure, orig_procedure);
        assert_eq!(skill.version, 1);

        let _ = std::fs::remove_file(format!("/tmp/train_cli_skill_rollback_cli.db"));
    }

    #[test]
    fn test_skills_rollback_invalid_version_returns_error() {
        let mut engine = skill_engine_tmp("rollback_invalid_cli");
        let id = engine.store_skill(&sample_skill()).unwrap();

        let result = engine.rollback(id, 99);
        assert!(result.is_err(), "Should error when version not in history");

        let _ = std::fs::remove_file(format!("/tmp/train_cli_skill_rollback_invalid_cli.db"));
    }
}
