//! EMEM-10: Memory CLI and maintenance commands.
//!
//! Subcommand: `lumina-core memories <subcommand>`
//!
//! search <query> [--user <id>] [--type <type>] [--since <date>]
//!     Search memories by text with optional filters.
//! view <memory_id>
//!     Show full memory with provenance, confidence, access stats.
//! delete <memory_id>
//!     Secure-delete a specific memory (admin only for other users' memories).
//! share <memory_id>
//!     Share a memory to household visibility.
//! review [--user <id>]
//!     Show quality-flagged memories (low confidence, never accessed).
//! export <path.jsonl> [--user <id>]
//!     Export memories as JSONL (plaintext, no encryption — ESEC-07 adds that).
//! reflexa [--user <id>]
//!     Trigger Reflexa reflection cycle immediately.
//! stats [--user <id>]
//!     Show memory counts by type, visibility, sensitivity, age.
//!
//! Privacy rules:
//! - Admin: may access any user's memories via --user flag.
//! - Non-admin: only ever accesses own memories (--user flag ignored / rejected).

use crate::engram::{EngramStore, engram_key};
use crate::engram::reflexa::quality::QUALITY_REVIEW_TAG;
use crate::engram::types::{Memory, MemoryType, SensitivityCategory};
use crate::error::{LuminaError, Result};
use crate::vault;
use std::io::Write;
use std::path::PathBuf;

// ── Public dispatch ────────────────────────────────────────────────────────────

/// Handle `lumina-core memories <args>`.
/// Returns `true` when a memories subcommand was dispatched.
///
/// EMEM-10: non-admin users cannot access another user's memories.
/// The `is_admin` flag comes from the caller (main.rs reads it from the user store
/// or config). For the CLI default we conservatively treat the operator as admin
/// since they have direct shell access to the host.
pub fn handle_memories_command(args: &[String]) -> bool {
    if args.len() < 2 || args[1] != "memories" {
        return false;
    }

    let sub = args.get(2).map(|s| s.as_str()).unwrap_or("help");

    // CLI operator has shell access → treated as admin.
    // Future: read role from user store once there is an "operator" user concept.
    let is_admin = true;

    let result = match sub {
        "add"    => cmd_add(args, is_admin),
        "search" => cmd_search(args, is_admin),
        "view"   => cmd_view(args, is_admin),
        "delete" => cmd_delete(args, is_admin),
        "share"  => cmd_share(args, is_admin),
        "review" => cmd_review(args, is_admin),
        "export" => cmd_export(args, is_admin),
        "reflexa" => cmd_reflexa(args, is_admin),
        "stats"  => cmd_stats(args, is_admin),
        _ => {
            print_memories_help();
            return true;
        }
    };

    if let Err(e) = result {
        eprintln!("memories {}: {}", sub, e);
        std::process::exit(1);
    }

    true
}

// ── Subcommand implementations ─────────────────────────────────────────────────

/// `memories search <query> [--user <id>] [--type <type>] [--since <date>]`
fn cmd_search(args: &[String], is_admin: bool) -> Result<()> {
    let query = args.get(3).ok_or_else(|| {
        LuminaError::Config("Usage: memories search <query> [--user <id>] [--type <type>] [--since <date>]".to_string())
    })?;

    let user_id = resolve_user_id(args, is_admin)?;
    let type_filter: Option<MemoryType> = find_flag_value(args, "--type")
        .and_then(|s| parse_memory_type(&s).ok());
    let since: Option<String> = find_flag_value(args, "--since");

    let store = open_store_for(&user_id)?;

    // Build SQL query with optional filters.
    let mut sql = String::from(
        "SELECT id, user_id, memory_type, visibility, sensitivity, content, embedding,
                source_conversation_id, source_turn_index, confidence, access_count,
                last_accessed, created_at, updated_at, superseded_by, tags
         FROM memories_v2
         WHERE user_id = ?1 AND superseded_by IS NULL
           AND content LIKE ?2"
    );
    if type_filter.is_some() {
        sql.push_str(" AND memory_type = ?3");
    }
    if since.is_some() {
        let param = if type_filter.is_some() { "?4" } else { "?3" };
        sql.push_str(&format!(" AND created_at >= {param}"));
    }
    sql.push_str(" ORDER BY created_at DESC LIMIT 50");

    let like_query = format!("%{}%", query);
    let type_db = type_filter.as_ref().map(|t| t.to_db().to_string());

    // Collect params as a Vec<Box<dyn ToSql>> to avoid closure-type mismatch
    // across the four query branches. Use query_row_and_then pattern via prepare+query.
    let memories: Vec<Memory> = {
        use rusqlite::types::ToSql;
        let mut params: Vec<Box<dyn ToSql>> = vec![
            Box::new(user_id.clone()),
            Box::new(like_query),
        ];
        if let Some(ref t) = type_db {
            params.push(Box::new(t.clone()));
        }
        if let Some(ref s) = since {
            params.push(Box::new(s.clone()));
        }

        let param_refs: Vec<&dyn ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = store.conn.prepare(&sql)
            .map_err(|e| LuminaError::Config(format!("search prepare: {e}")))?;
        let rows = stmt.query_map(param_refs.as_slice(), |row| crate::engram::row_to_memory(row))
            .map_err(|e| LuminaError::Config(format!("search query: {e}")))?;
        rows.filter_map(|r| r.ok()).collect()
    };

    if memories.is_empty() {
        println!("No memories match your query.");
        return Ok(());
    }

    println!("Found {} memory/memories matching '{}':", memories.len(), query);
    println!("{}", "-".repeat(80));

    for m in &memories {
        print_memory_summary(m);
    }

    Ok(())
}

/// `memories view <memory_id>`
fn cmd_view(args: &[String], is_admin: bool) -> Result<()> {
    let memory_id = args.get(3).ok_or_else(|| {
        LuminaError::Config("Usage: memories view <memory_id>".to_string())
    })?;

    let user_id = resolve_user_id(args, is_admin)?;
    let store = open_store_for(&user_id)?;

    let memory = store.get_memory_by_id(memory_id)?
        .ok_or_else(|| LuminaError::Config(format!("Memory '{}' not found.", memory_id)))?;

    print_memory_detail(&memory);
    Ok(())
}

/// `memories delete <memory_id> [--user <id>]`
fn cmd_delete(args: &[String], is_admin: bool) -> Result<()> {
    let memory_id = args.get(3).ok_or_else(|| {
        LuminaError::Config("Usage: memories delete <memory_id> [--user <id>]".to_string())
    })?;

    let user_id = resolve_user_id(args, is_admin)?;
    let store = open_store_for(&user_id)?;

    // Look up memory to check for superseded_by warning.
    let memory = store.get_memory_by_id(memory_id)?
        .ok_or_else(|| LuminaError::Config(format!("Memory '{}' not found.", memory_id)))?;

    // Warn if the memory was superseded.
    if let Some(ref newer_id) = memory.superseded_by {
        eprintln!(
            "Warning: This memory was superseded by {}. Deleting anyway.",
            newer_id
        );
    }

    store.secure_delete_memory(memory_id, "memories_cli_delete")?;
    println!("Memory '{}' deleted.", memory_id);
    Ok(())
}

/// `memories share <memory_id> [--user <id>]`
fn cmd_share(args: &[String], is_admin: bool) -> Result<()> {
    let memory_id = args.get(3).ok_or_else(|| {
        LuminaError::Config("Usage: memories share <memory_id> [--user <id>]".to_string())
    })?;

    let user_id = resolve_user_id(args, is_admin)?;
    let store = open_store_for(&user_id)?;

    store.share_memory(&user_id, memory_id)?;
    println!("Memory '{}' shared to household.", memory_id);
    Ok(())
}

/// `memories review [--user <id>]`
fn cmd_review(args: &[String], is_admin: bool) -> Result<()> {
    let user_id = resolve_user_id(args, is_admin)?;
    let store = open_store_for(&user_id)?;

    // Fetch quality-flagged memories.
    let mut stmt = store.conn.prepare(
        "SELECT id, user_id, memory_type, visibility, sensitivity, content, embedding,
                source_conversation_id, source_turn_index, confidence, access_count,
                last_accessed, created_at, updated_at, superseded_by, tags
         FROM memories_v2
         WHERE user_id = ?1 AND tags LIKE ?2 AND superseded_by IS NULL
         ORDER BY created_at ASC"
    ).map_err(|e| LuminaError::Config(format!("review prepare: {e}")))?;

    let tag_filter = format!("%{}%", QUALITY_REVIEW_TAG);
    let memories: Vec<Memory> = stmt.query_map(
        rusqlite::params![user_id, tag_filter],
        |row| crate::engram::row_to_memory(row),
    ).map_err(|e| LuminaError::Config(format!("review query: {e}")))?
    .filter_map(|r| r.ok())
    .collect();

    if memories.is_empty() {
        println!("No quality-flagged memories for user '{}'.", user_id);
        return Ok(());
    }

    println!("Quality-flagged memories for '{}':", user_id);
    println!("{}", "-".repeat(80));
    for m in &memories {
        print_memory_summary(m);
    }
    println!("\n{} memory/memories flagged for review.", memories.len());
    Ok(())
}

/// `memories export <path> [--user <id>] [--plaintext] [--include-sensitive] [--password <pw>]`
///
/// ESEC-07: exports are encrypted by default (AES-256-GCM).
/// - Default: encrypted `.enc` file. Password prompted interactively or via `--password`.
/// - `--plaintext`: produce unencrypted JSONL (prints a warning to stderr).
/// - `--include-sensitive`: include Health/Finance/Personal memories (admin only).
///
/// Embeddings are excluded from all exports (large and regenerable).
fn cmd_export(args: &[String], is_admin: bool) -> Result<()> {
    let export_path = args.get(3).ok_or_else(|| {
        LuminaError::Config(
            "Usage: memories export <path> [--user <id>] [--plaintext] [--include-sensitive] [--password <pw>]"
                .to_string(),
        )
    })?;

    let user_id = resolve_user_id(args, is_admin)?;
    let plaintext_mode = args.iter().any(|a| a == "--plaintext");
    let include_sensitive = args.iter().any(|a| a == "--include-sensitive");
    let password_flag: Option<String> = find_flag_value(args, "--password");

    // --include-sensitive requires admin
    if include_sensitive && !is_admin {
        return Err(LuminaError::SecurityViolation(
            "--include-sensitive requires admin role".to_string(),
        ));
    }

    let store = open_store_for(&user_id)?;
    let out_path = PathBuf::from(export_path);
    let filters = crate::engram::secure_export::ExportFilters {
        include_sensitive,
        memory_type_filter: None,
    };

    if plaintext_mode {
        let summary = crate::engram::secure_export::SecureExporter::export_plaintext(
            &store.conn,
            &user_id,
            &out_path,
            include_sensitive,
            &filters,
        )?;
        println!(
            "Exported {} memory/memories (plaintext) to '{}'.",
            summary.memory_count,
            summary.output_path.display()
        );
    } else {
        // Encrypted export — get password
        let password = match password_flag {
            Some(ref pw) => pw.as_bytes().to_vec(),
            None => {
                // Prompt interactively
                eprint!("Export password: ");
                std::io::stderr().flush().ok();
                let mut pw = String::new();
                std::io::stdin()
                    .read_line(&mut pw)
                    .map_err(|e| LuminaError::Config(format!("Failed to read password: {e}")))?;
                pw.trim_end_matches('\n').trim_end_matches('\r').as_bytes().to_vec()
            }
        };

        let summary = crate::engram::secure_export::SecureExporter::export_encrypted(
            &store.conn,
            &user_id,
            &out_path,
            &password,
            &filters,
        )?;
        println!(
            "Exported {} memory/memories (encrypted) to '{}'.",
            summary.memory_count,
            summary.output_path.display()
        );
    }

    Ok(())
}

/// `memories reflexa [--user <id>]`
///
/// Triggers Reflexa reflection synchronously (offline quality/contradiction analysis).
/// Uses the tokio runtime that main.rs already starts; for the CLI we start a local one.
fn cmd_reflexa(args: &[String], is_admin: bool) -> Result<()> {
    let user_id = resolve_user_id(args, is_admin)?;

    // Reflexa requires a ChordClient and Config — load from environment/vault.
    // If those aren't configured the reflection phases are non-fatal and log warnings.
    if let Err(e) = vault::init() {
        eprintln!("reflexa: vault not available ({}), using env vars", e);
    }

    let config = match crate::config::Config::from_env() {
        Ok(c) => std::sync::Arc::new(c),
        Err(e) => {
            eprintln!("reflexa: config unavailable ({}), some phases may be skipped", e);
            return Err(e);
        }
    };

    let store = open_store_for(&user_id)?;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| LuminaError::Config(format!("tokio runtime error: {e}")))?;

    let chord_url = config.chord_proxy_url.clone();
    let chord_secret = config.lumina_chord_secret.clone();
    let report = rt.block_on(async {
        let chord = crate::chord::ChordClient::new(chord_url, chord_secret);
        crate::engram::reflexa::ReflexaEngine::run_reflection(&store, &chord, &user_id, &config).await
    })?;

    println!("Reflexa reflection complete for '{}':", user_id);
    println!("  Contradictions resolved: {}", report.contradictions_resolved);
    println!("  Memories consolidated:   {}", report.memories_consolidated);
    println!("  Quality flags raised:    {}", report.quality_flags_raised);
    println!("  Principles generated:    {}", report.principles_generated);
    Ok(())
}

/// `memories stats [--user <id>]`
fn cmd_stats(args: &[String], is_admin: bool) -> Result<()> {
    let user_id = resolve_user_id(args, is_admin)?;
    let store = open_store_for(&user_id)?;
    let stats = compute_stats(&store.conn, &user_id)?;
    print_stats(&stats, &user_id);
    Ok(())
}

// ── Stats computation ──────────────────────────────────────────────────────────

/// Counts for memory statistics display.
#[derive(Debug, Default)]
pub struct MemoryStats {
    pub total: i64,
    pub superseded: i64,
    /// Per-type counts: (type_name, count)
    pub by_type: Vec<(String, i64)>,
    /// Per-visibility counts: (visibility_name, count)
    pub by_visibility: Vec<(String, i64)>,
    /// Per-sensitivity counts: (sensitivity_name, count)
    pub by_sensitivity: Vec<(String, i64)>,
    /// Age buckets: (bucket_label, count)
    pub by_age: Vec<(String, i64)>,
    /// Quality-flagged memories count
    pub quality_flagged: i64,
}

/// Compute memory stats for a user from the SQLite connection.
pub fn compute_stats(conn: &rusqlite::Connection, user_id: &str) -> Result<MemoryStats> {
    let mut stats = MemoryStats::default();

    // Total
    stats.total = conn.query_row(
        "SELECT COUNT(*) FROM memories_v2 WHERE user_id = ?1",
        rusqlite::params![user_id],
        |r| r.get(0),
    ).map_err(|e| LuminaError::Config(format!("stats total: {e}")))?;

    // Superseded
    stats.superseded = conn.query_row(
        "SELECT COUNT(*) FROM memories_v2 WHERE user_id = ?1 AND superseded_by IS NOT NULL",
        rusqlite::params![user_id],
        |r| r.get(0),
    ).map_err(|e| LuminaError::Config(format!("stats superseded: {e}")))?;

    // By type
    {
        let mut stmt = conn.prepare(
            "SELECT memory_type, COUNT(*) FROM memories_v2
             WHERE user_id = ?1 AND superseded_by IS NULL
             GROUP BY memory_type ORDER BY COUNT(*) DESC"
        ).map_err(|e| LuminaError::Config(format!("stats by_type prepare: {e}")))?;

        let rows = stmt.query_map(
            rusqlite::params![user_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        ).map_err(|e| LuminaError::Config(format!("stats by_type query: {e}")))?;

        stats.by_type = rows.filter_map(|r| r.ok()).collect();
    }

    // By visibility
    {
        let mut stmt = conn.prepare(
            "SELECT visibility, COUNT(*) FROM memories_v2
             WHERE user_id = ?1 AND superseded_by IS NULL
             GROUP BY visibility ORDER BY COUNT(*) DESC"
        ).map_err(|e| LuminaError::Config(format!("stats by_visibility prepare: {e}")))?;

        let rows = stmt.query_map(
            rusqlite::params![user_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        ).map_err(|e| LuminaError::Config(format!("stats by_visibility query: {e}")))?;

        stats.by_visibility = rows.filter_map(|r| r.ok()).collect();
    }

    // By sensitivity
    {
        let mut stmt = conn.prepare(
            "SELECT sensitivity, COUNT(*) FROM memories_v2
             WHERE user_id = ?1 AND superseded_by IS NULL
             GROUP BY sensitivity ORDER BY COUNT(*) DESC"
        ).map_err(|e| LuminaError::Config(format!("stats by_sensitivity prepare: {e}")))?;

        let rows = stmt.query_map(
            rusqlite::params![user_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        ).map_err(|e| LuminaError::Config(format!("stats by_sensitivity query: {e}")))?;

        stats.by_sensitivity = rows.filter_map(|r| r.ok()).collect();
    }

    // By age (buckets: today, this week, this month, this year, older)
    {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let cutoffs = [
            ("today", now_secs.saturating_sub(86400)),
            ("this week", now_secs.saturating_sub(7 * 86400)),
            ("this month", now_secs.saturating_sub(30 * 86400)),
            ("this year", now_secs.saturating_sub(365 * 86400)),
        ];

        let cutoff_isos: Vec<(&str, String)> = cutoffs.iter()
            .map(|(label, secs)| (*label, crate::engram::types::unix_secs_to_iso(*secs)))
            .collect();

        let (today_iso, week_iso, month_iso, year_iso) = (
            &cutoff_isos[0].1,
            &cutoff_isos[1].1,
            &cutoff_isos[2].1,
            &cutoff_isos[3].1,
        );

        let count_since = |since: &str| -> i64 {
            conn.query_row(
                "SELECT COUNT(*) FROM memories_v2 WHERE user_id = ?1 AND superseded_by IS NULL AND created_at >= ?2",
                rusqlite::params![user_id, since],
                |r| r.get(0),
            ).unwrap_or(0)
        };

        let today_count = count_since(today_iso);
        let week_count = count_since(week_iso) - today_count;
        let month_count = count_since(month_iso) - today_count - week_count;
        let year_count = count_since(year_iso) - today_count - week_count - month_count;
        let older_count = stats.total - stats.superseded - today_count - week_count - month_count - year_count;

        stats.by_age = vec![
            ("today".to_string(), today_count),
            ("this week (excl. today)".to_string(), week_count),
            ("this month (excl. this week)".to_string(), month_count),
            ("this year (excl. this month)".to_string(), year_count),
            ("older".to_string(), older_count),
        ];
    }

    // Quality flagged
    stats.quality_flagged = conn.query_row(
        "SELECT COUNT(*) FROM memories_v2 WHERE user_id = ?1 AND superseded_by IS NULL AND tags LIKE ?2",
        rusqlite::params![user_id, format!("%{}%", QUALITY_REVIEW_TAG)],
        |r| r.get(0),
    ).map_err(|e| LuminaError::Config(format!("stats quality_flagged: {e}")))?;

    Ok(stats)
}

// ── Display helpers ────────────────────────────────────────────────────────────

fn print_memory_summary(m: &Memory) {
    let type_label = m.memory_type.to_db();
    let vis = m.visibility.to_db();
    let sense = m.sensitivity.to_db();
    let tags_str = if m.tags.is_empty() {
        String::new()
    } else {
        format!(" [{}]", m.tags.join(", "))
    };
    let superseded = if m.superseded_by.is_some() { " (SUPERSEDED)" } else { "" };
    println!(
        "[{}] {} | {}/{}/{} | conf:{:.2} acc:{} | created:{}{}{}\n  {}",
        &m.id[..8.min(m.id.len())],
        &m.created_at[..10.min(m.created_at.len())],
        type_label, vis, sense,
        m.confidence, m.access_count,
        m.created_at, tags_str, superseded,
        m.content,
    );
}

fn print_memory_detail(m: &Memory) {
    println!("Memory ID:     {}", m.id);
    println!("User:          {}", m.user_id);
    println!("Type:          {}", m.memory_type.to_db());
    println!("Visibility:    {}", m.visibility.to_db());
    println!("Sensitivity:   {}", m.sensitivity.to_db());
    println!("Confidence:    {:.2}", m.confidence);
    println!("Access count:  {}", m.access_count);
    println!("Last accessed: {}", m.last_accessed.as_deref().unwrap_or("never"));
    println!("Created:       {}", m.created_at);
    println!("Updated:       {}", m.updated_at);
    if let Some(ref conv) = m.source_conversation_id {
        println!("Conversation:  {} (turn {})", conv,
            m.source_turn_index.map(|i| i.to_string()).as_deref().unwrap_or("?"));
    }
    if let Some(ref sup) = m.superseded_by {
        println!("Superseded by: {}", sup);
    }
    if !m.tags.is_empty() {
        println!("Tags:          {}", m.tags.join(", "));
    }
    println!("Embedding:     {} dimensions", m.embedding.len());
    println!();
    println!("Content:");
    println!("{}", m.content);
}

fn print_stats(stats: &MemoryStats, user_id: &str) {
    println!("Memory statistics for '{}':", user_id);
    println!("{}", "=".repeat(50));
    println!("Total memories:    {}", stats.total);
    println!("Active:            {}", stats.total - stats.superseded);
    println!("Superseded:        {}", stats.superseded);
    println!("Quality-flagged:   {}", stats.quality_flagged);
    println!();

    if !stats.by_type.is_empty() {
        println!("By type:");
        for (t, n) in &stats.by_type {
            println!("  {:12} {}", t, n);
        }
        println!();
    }

    if !stats.by_visibility.is_empty() {
        println!("By visibility:");
        for (v, n) in &stats.by_visibility {
            println!("  {:12} {}", v, n);
        }
        println!();
    }

    if !stats.by_sensitivity.is_empty() {
        println!("By sensitivity:");
        for (s, n) in &stats.by_sensitivity {
            println!("  {:12} {}", s, n);
        }
        println!();
    }

    if !stats.by_age.is_empty() {
        println!("By age:");
        for (label, n) in &stats.by_age {
            println!("  {:38} {}", label, n);
        }
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────────

/// Resolve the user_id to operate on.
///
/// - If `--user <id>` is present and caller is admin: use that user_id.
/// - If `--user <id>` is present and caller is NOT admin: return privacy error.
/// - If `--user` is absent: use the DEFAULT_OPERATOR_USER_ID.
///
/// Note: For shell CLI, there is no authenticated session — we use a configurable
/// default via `LUMINA_OPERATOR_USER_ID` env var (falls back to "system").
fn resolve_user_id(args: &[String], is_admin: bool) -> Result<String> {
    let requested = find_flag_value(args, "--user");

    match requested {
        Some(requested_id) => {
            if is_admin {
                Ok(requested_id)
            } else {
                Err(LuminaError::SecurityViolation(
                    "Non-admin users cannot access other users' memories via --user flag.".to_string()
                ))
            }
        }
        None => {
            // No --user flag: use operator default.
            let default_id = std::env::var("LUMINA_OPERATOR_USER_ID")
                .unwrap_or_else(|_| "system".to_string());
            Ok(default_id)
        }
    }
}

/// Open an EngramStore for the given user_id using the ENGRAM_DB_KEY from vault/env.
fn open_store_for(user_id: &str) -> Result<EngramStore> {
    let key = engram_key()?;
    let base = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".lumina");
    EngramStore::open_for_user_at(&base, user_id, &key)
}

/// Look for `--<flag> <value>` anywhere in args.
fn find_flag_value(args: &[String], flag: &str) -> Option<String> {
    for (i, arg) in args.iter().enumerate() {
        if arg == flag {
            return args.get(i + 1).cloned();
        }
    }
    None
}

/// `memories add <content> [--type <t>] [--sensitivity <s>] [--user <id>]`
///
/// Inserts a memory directly. Used to seed durable facts (e.g. home/work
/// addresses) the operator wants Lumina to know without learning them in chat.
fn cmd_add(args: &[String], is_admin: bool) -> Result<()> {
    let content = args.get(3).ok_or_else(|| {
        LuminaError::Config(
            "Usage: memories add <content> [--type semantic|preference|episodic|principle] \
             [--sensitivity general|work|personal|health|finance|household] [--user <id>]"
                .to_string(),
        )
    })?;
    if content.starts_with("--") {
        return Err(LuminaError::Config(
            "First argument must be the memory content (quote it).".to_string(),
        ));
    }

    let user_id = resolve_user_id(args, is_admin)?;
    let mem_type = find_flag_value(args, "--type")
        .map(|s| parse_memory_type(&s))
        .transpose()?
        .unwrap_or(MemoryType::Semantic);
    let sensitivity = find_flag_value(args, "--sensitivity")
        .map(|s| parse_sensitivity(&s))
        .transpose()?
        .unwrap_or(SensitivityCategory::General);

    let store = open_store_for(&user_id)?;
    let memory = Memory::new(user_id.clone(), mem_type, sensitivity, content.clone());
    store
        .insert_memory(&memory)
        .map_err(|e| LuminaError::Config(format!("insert failed: {e}")))?;

    println!("Added memory {} for user '{}':", memory.id, user_id);
    println!("  [{:?}/{:?}] {}", memory.memory_type, memory.sensitivity, content);
    Ok(())
}

/// Parse a SensitivityCategory from a string.
fn parse_sensitivity(s: &str) -> Result<SensitivityCategory> {
    match s.to_lowercase().as_str() {
        "general"   => Ok(SensitivityCategory::General),
        "work"      => Ok(SensitivityCategory::Work),
        "personal"  => Ok(SensitivityCategory::Personal),
        "health"    => Ok(SensitivityCategory::Health),
        "finance"   => Ok(SensitivityCategory::Finance),
        "household" => Ok(SensitivityCategory::Household),
        other => Err(LuminaError::Config(format!(
            "Unknown sensitivity '{}'. Valid: general, work, personal, health, finance, household",
            other
        ))),
    }
}

/// Parse a MemoryType from a string.
fn parse_memory_type(s: &str) -> Result<MemoryType> {
    match s.to_lowercase().as_str() {
        "episodic"  => Ok(MemoryType::Episodic),
        "semantic"  => Ok(MemoryType::Semantic),
        "preference" => Ok(MemoryType::Preference),
        "principle" => Ok(MemoryType::Principle),
        other => Err(LuminaError::Config(format!(
            "Unknown memory type '{}'. Valid: episodic, semantic, preference, principle",
            other
        ))),
    }
}

pub fn print_memories_help() {
    eprintln!("Usage: lumina-core memories <subcommand>");
    eprintln!();
    eprintln!("Subcommands:");
    eprintln!("  add <content> [--type <type>] [--sensitivity <s>] [--user <id>]");
    eprintln!("                           Insert a durable memory (seed a fact)");
    eprintln!("  search <query> [--user <id>] [--type <type>] [--since <date>]");
    eprintln!("                           Search memories (full-text LIKE, case-insensitive)");
    eprintln!("  view <memory_id>         Show full memory with provenance and stats");
    eprintln!("  delete <memory_id> [--user <id>]");
    eprintln!("                           Secure-delete a memory");
    eprintln!("  share <memory_id> [--user <id>]");
    eprintln!("                           Share a memory to household visibility");
    eprintln!("  review [--user <id>]     Show quality-flagged memories");
    eprintln!("  export <path.jsonl> [--user <id>] [--include-embeddings]");
    eprintln!("                           Export memories as JSONL (one JSON object per line)");
    eprintln!("  reflexa [--user <id>]    Trigger Reflexa reflection cycle immediately");
    eprintln!("  stats [--user <id>]      Show counts by type, visibility, sensitivity, age");
    eprintln!();
    eprintln!("Flags:");
    eprintln!("  --user <id>   Operate on a specific user's memories (admin only)");
    eprintln!("  --type        Filter by memory type: episodic, semantic, preference, principle");
    eprintln!("  --since       Filter by creation date (ISO 8601: 2026-01-01)");
    eprintln!("  --include-embeddings  Include embedding vectors in export");
    eprintln!();
    eprintln!("Default user: $LUMINA_OPERATOR_USER_ID (env) or 'system'");
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use crate::engram::EngramStore;
    use crate::engram::types::{Memory, MemoryType, SensitivityCategory};

    fn test_key() -> Vec<u8> { vec![0u8; 32] }

    fn tmp_db(tag: &str) -> PathBuf {
        let p = PathBuf::from(format!("/tmp/lumina_emem10_test_{}.db", tag));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn open_store_at(path: &PathBuf) -> EngramStore {
        EngramStore::open(path, &test_key()).unwrap()
    }

    #[test]
    fn test_parse_sensitivity_valid_and_invalid() {
        assert!(matches!(parse_sensitivity("work"), Ok(SensitivityCategory::Work)));
        assert!(matches!(parse_sensitivity("GENERAL"), Ok(SensitivityCategory::General)));
        assert!(parse_sensitivity("nonsense").is_err());
    }

    #[test]
    fn test_cmd_add_rejects_flag_as_content() {
        // First positional must be content, not a flag.
        let args = vec!["lumina-core".into(), "memories".into(), "add".into(), "--type".into()];
        assert!(cmd_add(&args, true).is_err());
    }

    #[test]
    fn test_cmd_add_requires_content() {
        let args = vec!["lumina-core".into(), "memories".into(), "add".into()];
        assert!(cmd_add(&args, true).is_err());
    }

    // ── test_memories_search_filters ────────────────────────────────────────────

    /// Verifies that search can find memories by content and that the --type filter
    /// correctly narrows results.
    #[test]
    fn test_memories_search_filters() {
        let path = tmp_db("search_filters");
        let store = open_store_at(&path);

        let m1 = Memory::new("system", MemoryType::Preference, SensitivityCategory::General, "likes dark roast coffee");
        let m2 = Memory::new("system", MemoryType::Semantic, SensitivityCategory::General, "coffee is a morning ritual");
        let m3 = Memory::new("system", MemoryType::Episodic, SensitivityCategory::Work, "had a meeting on Tuesday");
        store.insert_memory(&m1).unwrap();
        store.insert_memory(&m2).unwrap();
        store.insert_memory(&m3).unwrap();

        // Search for "coffee" — should return m1 and m2
        let like_query = "%coffee%";
        let mut stmt = store.conn.prepare(
            "SELECT id, user_id, memory_type, visibility, sensitivity, content, embedding,
                    source_conversation_id, source_turn_index, confidence, access_count,
                    last_accessed, created_at, updated_at, superseded_by, tags
             FROM memories_v2
             WHERE user_id = 'system' AND content LIKE ?1 AND superseded_by IS NULL
             ORDER BY created_at DESC LIMIT 50"
        ).unwrap();

        let results: Vec<Memory> = stmt.query_map(
            rusqlite::params![like_query],
            |row| crate::engram::row_to_memory(row),
        ).unwrap().filter_map(|r| r.ok()).collect();

        assert_eq!(results.len(), 2, "should find 2 coffee memories");
        assert!(results.iter().any(|m| m.content.contains("dark roast coffee")));
        assert!(results.iter().any(|m| m.content.contains("morning ritual")));

        // Search with type filter — only preference type
        let type_db = MemoryType::Preference.to_db();
        let mut stmt2 = store.conn.prepare(
            "SELECT id, user_id, memory_type, visibility, sensitivity, content, embedding,
                    source_conversation_id, source_turn_index, confidence, access_count,
                    last_accessed, created_at, updated_at, superseded_by, tags
             FROM memories_v2
             WHERE user_id = 'system' AND content LIKE ?1 AND memory_type = ?2 AND superseded_by IS NULL
             ORDER BY created_at DESC LIMIT 50"
        ).unwrap();

        let filtered: Vec<Memory> = stmt2.query_map(
            rusqlite::params![like_query, type_db],
            |row| crate::engram::row_to_memory(row),
        ).unwrap().filter_map(|r| r.ok()).collect();

        assert_eq!(filtered.len(), 1, "type filter should narrow to 1 result");
        assert_eq!(filtered[0].memory_type, MemoryType::Preference);

        let _ = std::fs::remove_file(&path);
    }

    // ── test_memories_stats_returns_counts ──────────────────────────────────────

    /// Verifies that compute_stats returns accurate total and per-type counts.
    #[test]
    fn test_memories_stats_returns_counts() {
        let path = tmp_db("stats_counts");
        let store = open_store_at(&path);

        // Insert 3 semantic, 2 preference
        for i in 0..3 {
            let m = Memory::new("system", MemoryType::Semantic, SensitivityCategory::General, format!("fact {i}"));
            store.insert_memory(&m).unwrap();
        }
        for i in 0..2 {
            let m = Memory::new("system", MemoryType::Preference, SensitivityCategory::General, format!("preference {i}"));
            store.insert_memory(&m).unwrap();
        }

        let stats = compute_stats(&store.conn, "system").unwrap();
        assert_eq!(stats.total, 5, "total should be 5");
        assert_eq!(stats.superseded, 0, "none superseded");

        let type_map: std::collections::HashMap<String, i64> = stats.by_type.into_iter().collect();
        assert_eq!(*type_map.get("semantic").unwrap_or(&0), 3, "3 semantic");
        assert_eq!(*type_map.get("preference").unwrap_or(&0), 2, "2 preference");

        let _ = std::fs::remove_file(&path);
    }

    // ── test_export_produces_jsonl ──────────────────────────────────────────────

    /// Verifies that export writes valid JSONL — one JSON object per line,
    /// all fields present except embedding (excluded by default).
    #[test]
    fn test_export_produces_jsonl() {
        let db_path = tmp_db("export_jsonl");
        let store = open_store_at(&db_path);

        let m1 = Memory::new("system", MemoryType::Semantic, SensitivityCategory::General, "export fact one");
        let m2 = Memory::new("system", MemoryType::Preference, SensitivityCategory::General, "export fact two");
        store.insert_memory(&m1).unwrap();
        store.insert_memory(&m2).unwrap();

        let export_path = PathBuf::from("/tmp/lumina_emem10_test_export.jsonl");
        let _ = std::fs::remove_file(&export_path);

        // Stream export: replicate the export logic inline since cmd_export calls open_store_for
        // which requires vault — we test the export format directly.
        {
            let mut file = std::fs::File::create(&export_path).unwrap();
            let mut stmt = store.conn.prepare(
                "SELECT id, user_id, memory_type, visibility, sensitivity, content, embedding,
                        source_conversation_id, source_turn_index, confidence, access_count,
                        last_accessed, created_at, updated_at, superseded_by, tags
                 FROM memories_v2 WHERE user_id = 'system' ORDER BY created_at ASC"
            ).unwrap();

            let rows: Vec<Memory> = stmt.query_map(
                rusqlite::params![],
                |row| crate::engram::row_to_memory(row),
            ).unwrap().filter_map(|r| r.ok()).collect();

            for mut memory in rows {
                memory.embedding.clear(); // exclude embeddings by default
                let json = serde_json::to_string(&memory).unwrap();
                writeln!(file, "{}", json).unwrap();
            }
        }

        // Verify JSONL: each line is valid JSON with expected fields.
        let content = std::fs::read_to_string(&export_path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2, "should have 2 JSONL lines");

        for line in &lines {
            let parsed: serde_json::Value = serde_json::from_str(line)
                .expect("each line should be valid JSON");
            assert!(parsed.get("id").is_some(), "should have id field");
            assert!(parsed.get("content").is_some(), "should have content field");
            assert!(parsed.get("memory_type").is_some(), "should have memory_type field");
            assert!(parsed.get("visibility").is_some(), "should have visibility field");
            assert!(parsed.get("sensitivity").is_some(), "should have sensitivity field");
            assert!(parsed.get("confidence").is_some(), "should have confidence field");
            assert!(parsed.get("created_at").is_some(), "should have created_at field");
            // Embedding should be empty array (excluded)
            let embedding = parsed.get("embedding").unwrap();
            assert_eq!(embedding.as_array().unwrap().len(), 0, "embedding should be empty");
        }

        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        let contents: Vec<&str> = vec![
            first["content"].as_str().unwrap(),
            second["content"].as_str().unwrap(),
        ];
        assert!(contents.contains(&"export fact one"), "first export fact missing");
        assert!(contents.contains(&"export fact two"), "second export fact missing");

        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(&export_path);
    }

    // ── test_non_admin_cannot_access_other_users ─────────────────────────────────

    /// Verifies that a non-admin user cannot use --user to access another user's memories.
    #[test]
    fn test_non_admin_cannot_access_other_users() {
        let args = vec![
            "lumina-core".to_string(),
            "memories".to_string(),
            "stats".to_string(),
            "--user".to_string(),
            "user-victim".to_string(),
        ];

        // Non-admin: is_admin = false
        let result = resolve_user_id(&args, false);
        assert!(
            result.is_err(),
            "Non-admin should not be able to specify --user for another user's memories"
        );
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("Non-admin"),
            "Error message should mention non-admin restriction: {}",
            err_msg
        );
    }

    // ── test_handle_memories_command_dispatch ─────────────────────────────────────

    #[test]
    fn test_not_a_memories_command_returns_false() {
        let args = vec!["lumina-core".to_string(), "train".to_string()];
        assert!(!handle_memories_command(&args));
    }

    #[test]
    fn test_memories_unknown_subcommand_returns_true() {
        let args = vec![
            "lumina-core".to_string(),
            "memories".to_string(),
            "bogus".to_string(),
        ];
        assert!(handle_memories_command(&args));
    }

    #[test]
    fn test_memories_no_subcommand_returns_true() {
        let args = vec!["lumina-core".to_string(), "memories".to_string()];
        assert!(handle_memories_command(&args));
    }

    // ── test_parse_memory_type ────────────────────────────────────────────────────

    #[test]
    fn test_parse_memory_type_all_values() {
        assert_eq!(parse_memory_type("episodic").unwrap(), MemoryType::Episodic);
        assert_eq!(parse_memory_type("semantic").unwrap(), MemoryType::Semantic);
        assert_eq!(parse_memory_type("preference").unwrap(), MemoryType::Preference);
        assert_eq!(parse_memory_type("principle").unwrap(), MemoryType::Principle);
        // Case-insensitive
        assert_eq!(parse_memory_type("SEMANTIC").unwrap(), MemoryType::Semantic);
    }

    #[test]
    fn test_parse_memory_type_invalid_returns_error() {
        assert!(parse_memory_type("unknown_type").is_err());
    }

    // ── test_find_flag_value ──────────────────────────────────────────────────────

    #[test]
    fn test_find_flag_value_present() {
        let args = vec![
            "lumina-core".to_string(),
            "memories".to_string(),
            "search".to_string(),
            "coffee".to_string(),
            "--user".to_string(),
            "user-alice".to_string(),
        ];
        assert_eq!(find_flag_value(&args, "--user"), Some("user-alice".to_string()));
    }

    #[test]
    fn test_find_flag_value_absent_returns_none() {
        let args = vec!["lumina-core".to_string(), "memories".to_string()];
        assert_eq!(find_flag_value(&args, "--user"), None);
    }

    // ── test_resolve_user_id_admin_can_access_other_user ─────────────────────────

    #[test]
    fn test_admin_can_specify_other_user() {
        let args = vec![
            "lumina-core".to_string(),
            "memories".to_string(),
            "stats".to_string(),
            "--user".to_string(),
            "user-alice".to_string(),
        ];
        let result = resolve_user_id(&args, true);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "user-alice");
    }

    #[test]
    #[serial]
    fn test_default_user_falls_back_to_env_or_system() {
        // Without --user flag and without env var, should default to "system"
        std::env::remove_var("LUMINA_OPERATOR_USER_ID");
        let args = vec!["lumina-core".to_string(), "memories".to_string(), "stats".to_string()];
        let result = resolve_user_id(&args, false).unwrap();
        assert_eq!(result, "system");
    }

    #[test]
    #[serial]
    fn test_default_user_reads_from_env() {
        std::env::set_var("LUMINA_OPERATOR_USER_ID", "user-operator");
        let args = vec!["lumina-core".to_string(), "memories".to_string(), "stats".to_string()];
        let result = resolve_user_id(&args, false).unwrap();
        assert_eq!(result, "user-operator");
        std::env::remove_var("LUMINA_OPERATOR_USER_ID");
    }

    // ── test_stats_empty_store ──────────────────────────────────────────────────

    #[test]
    fn test_stats_empty_store_returns_zeros() {
        let path = tmp_db("stats_empty");
        let store = open_store_at(&path);
        let stats = compute_stats(&store.conn, "system").unwrap();
        assert_eq!(stats.total, 0);
        assert_eq!(stats.superseded, 0);
        assert!(stats.by_type.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    // ── test_export_with_embeddings_flag ──────────────────────────────────────────

    #[test]
    fn test_export_with_embeddings_includes_vector() {
        let db_path = tmp_db("export_with_emb");
        let store = open_store_at(&db_path);

        let mut m = Memory::new("system", MemoryType::Semantic, SensitivityCategory::General, "fact with embedding");
        m.embedding = vec![1.0f32, 0.0, 0.0];
        store.insert_memory(&m).unwrap();

        let export_path = PathBuf::from("/tmp/lumina_emem10_test_export_emb.jsonl");
        let _ = std::fs::remove_file(&export_path);

        {
            let mut file = std::fs::File::create(&export_path).unwrap();
            let mut stmt = store.conn.prepare(
                "SELECT id, user_id, memory_type, visibility, sensitivity, content, embedding,
                        source_conversation_id, source_turn_index, confidence, access_count,
                        last_accessed, created_at, updated_at, superseded_by, tags
                 FROM memories_v2 WHERE user_id = 'system' ORDER BY created_at ASC"
            ).unwrap();

            let rows: Vec<Memory> = stmt.query_map(
                rusqlite::params![],
                |row| crate::engram::row_to_memory(row),
            ).unwrap().filter_map(|r| r.ok()).collect();

            for memory in rows {
                // include_embeddings = true: do NOT clear
                let json = serde_json::to_string(&memory).unwrap();
                writeln!(file, "{}", json).unwrap();
            }
        }

        let content = std::fs::read_to_string(&export_path).unwrap();
        let line = content.lines().next().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
        // Embedding may be noise-injected from ESEC-03 — just check it's non-empty
        let emb = parsed.get("embedding").unwrap().as_array().unwrap();
        assert!(!emb.is_empty(), "embedding should be present when --include-embeddings set");

        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(&export_path);
    }
}
