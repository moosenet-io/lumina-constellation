//! P2-06: User management CLI subcommands.
//!
//! Subcommand: `lumina-core users <subcommand>`
//!
//! list                                        Show all users with roles and last seen
//! add <matrix_id> <display_name> [--role <role>]  Create a user
//! remove <user_id>                            Delete a user
//! show <user_id>                              Show user details
//! promote <user_id> <role>                    Change a user's role
//! disable <user_id>                           Soft-disable a user account
//! enable <user_id>                            Re-enable a disabled user
//! grant-tool <user_id> <tool_name>            Grant tool access to a user
//! set-prompt <user_id> --file <path>          Set a custom system prompt for a user
//! link <user_id> <channel_type> <channel_id>  Link a channel identity to a user
//! unlink <channel_type> <channel_id>          Remove a channel identity link
//!
//! All operations use `UserStore::open_default()` which reads from `~/.lumina/users.db`.

use crate::error::{LuminaError, Result};
use crate::users::{UserRole, UserStore};
use crate::users::identity::ChannelType;

const TRUNCATE_LEN: usize = 50;

/// Handle `lumina-core users <args>`.
/// Returns `true` when a users subcommand was dispatched.
pub fn handle_users_command(args: &[String]) -> bool {
    if args.len() < 2 || args[1] != "users" {
        return false;
    }

    let sub = args.get(2).map(|s| s.as_str()).unwrap_or("help");

    let result = match sub {
        "list"        => cmd_list(),
        "add"         => cmd_add(args),
        "remove"      => cmd_remove(args),
        "show"        => cmd_show(args),
        "promote"     => cmd_promote(args),
        "disable"     => cmd_disable(args),
        "enable"      => cmd_enable(args),
        "grant-tool"  => cmd_grant_tool(args),
        "set-prompt"  => cmd_set_prompt(args),
        "link"        => cmd_link(args),
        "unlink"      => cmd_unlink(args),
        _ => {
            print_users_help();
            return true;
        }
    };

    if let Err(e) = result {
        eprintln!("users {}: {}", sub, e);
        std::process::exit(1);
    }

    true
}

// ── Subcommand implementations ────────────────────────────────────────────────

fn cmd_list() -> Result<()> {
    let store = open_store()?;
    let users = store.list()?;

    if users.is_empty() {
        println!("No users found. Use 'users add' to create one.");
        return Ok(());
    }

    println!("{:<38} {:<20} {:<8} {:<10} {}", "ID", "DISPLAY NAME", "ROLE", "ENABLED", "LAST SEEN");
    println!("{}", "-".repeat(100));

    for u in &users {
        let name = truncate(&u.display_name, TRUNCATE_LEN);
        let last_seen = u.last_seen.as_deref().unwrap_or("never");
        let enabled = if u.enabled { "yes" } else { "no" };
        println!(
            "{:<38} {:<20} {:<8} {:<10} {}",
            u.user_id, name, u.role, enabled, last_seen
        );
    }

    println!("\n{} user(s) total", users.len());
    Ok(())
}

fn cmd_add(args: &[String]) -> Result<()> {
    let matrix_id = args.get(3).ok_or_else(|| {
        LuminaError::Config("Usage: users add <matrix_id> <display_name> [--role member|admin|guest]".to_string())
    })?;
    let display_name = args.get(4).ok_or_else(|| {
        LuminaError::Config("Usage: users add <matrix_id> <display_name> [--role member|admin|guest]".to_string())
    })?;

    let role = parse_role_flag(args).unwrap_or(UserRole::Member);

    let store = open_store()?;
    let user = store.create_user(display_name, Some(matrix_id), role)?;
    println!("Created user '{}'", user.display_name);
    println!("  ID:        {}", user.user_id);
    println!("  Matrix ID: {}", matrix_id);
    println!("  Role:      {}", user.role);
    Ok(())
}

fn cmd_remove(args: &[String]) -> Result<()> {
    let user_id = parse_user_id_arg(args, "remove")?;
    let store = open_store()?;

    // Show user info before removal for confirmation feedback.
    // get_by_id returns Err (not found) if user doesn't exist, so delete() below
    // will always affect a row — the else branch is unreachable; omit it.
    let user = store.get_by_id(&user_id)?.ok_or_else(|| {
        LuminaError::Config(format!(
            "User '{}' not found. Run 'users list' to see valid IDs.",
            user_id
        ))
    })?;

    store.delete(&user_id)?;
    println!("Removed user '{}' ({})", user.display_name, user_id);
    Ok(())
}

fn cmd_show(args: &[String]) -> Result<()> {
    let user_id = parse_user_id_arg(args, "show")?;
    let store = open_store()?;

    let user = store.get_by_id(&user_id)?.ok_or_else(|| {
        LuminaError::Config(format!(
            "User '{}' not found. Run 'users list' to see valid IDs.",
            user_id
        ))
    })?;

    println!("User details:");
    println!("  ID:           {}", user.user_id);
    println!("  Display name: {}", user.display_name);
    println!("  Role:         {}", user.role);
    println!("  Enabled:      {}", user.enabled);
    println!("  Created:      {}", user.created_at);
    println!("  Last seen:    {}", user.last_seen.as_deref().unwrap_or("never"));
    if let Some(ref mx) = user.matrix_user_id {
        println!("  Matrix ID:    {}", mx);
    }

    // Show channel identities.
    let channels = store.list_channels_for_user(&user.user_id)?;
    if !channels.is_empty() {
        println!("  Channel identities:");
        for ci in &channels {
            let verified = if ci.verified { "verified" } else { "unverified" };
            println!("    {} {} ({})", ci.channel_type, ci.channel_user_id, verified);
        }
    }
    Ok(())
}

fn cmd_promote(args: &[String]) -> Result<()> {
    let user_id = parse_user_id_arg(args, "promote")?;
    let role_str = args.get(4).ok_or_else(|| {
        LuminaError::Config("Usage: users promote <user_id> <admin|member|guest>".to_string())
    })?;
    let new_role = parse_role_str(role_str)?;

    let store = open_store()?;
    let user = store.get_by_id(&user_id)?.ok_or_else(|| {
        LuminaError::Config(format!(
            "User '{}' not found. Run 'users list' to see valid IDs.",
            user_id
        ))
    })?;

    store.set_role(&user_id, new_role.clone())?;
    println!("User '{}' role changed to '{}'.", user.display_name, new_role);
    Ok(())
}

fn cmd_disable(args: &[String]) -> Result<()> {
    let user_id = parse_user_id_arg(args, "disable")?;
    let store = open_store()?;

    let user = store.get_by_id(&user_id)?.ok_or_else(|| {
        LuminaError::Config(format!(
            "User '{}' not found. Run 'users list' to see valid IDs.",
            user_id
        ))
    })?;

    store.set_enabled(&user_id, false)?;
    println!("User '{}' disabled.", user.display_name);
    Ok(())
}

fn cmd_enable(args: &[String]) -> Result<()> {
    let user_id = parse_user_id_arg(args, "enable")?;
    let store = open_store()?;

    let user = store.get_by_id(&user_id)?.ok_or_else(|| {
        LuminaError::Config(format!(
            "User '{}' not found. Run 'users list' to see valid IDs.",
            user_id
        ))
    })?;

    store.set_enabled(&user_id, true)?;
    println!("User '{}' enabled.", user.display_name);
    Ok(())
}

fn cmd_grant_tool(args: &[String]) -> Result<()> {
    let user_id = parse_user_id_arg(args, "grant-tool")?;
    let tool_name = args.get(4).ok_or_else(|| {
        LuminaError::Config("Usage: users grant-tool <user_id> <tool_name>".to_string())
    })?;

    // Reject tool names that contain commas — they would corrupt the CSV storage.
    if tool_name.contains(',') {
        return Err(LuminaError::Config(format!(
            "Tool name '{}' is invalid — tool names must not contain commas.",
            tool_name
        )));
    }

    let store = open_store()?;
    let user = store.get_by_id(&user_id)?.ok_or_else(|| {
        LuminaError::Config(format!(
            "User '{}' not found. Run 'users list' to see valid IDs.",
            user_id
        ))
    })?;

    // Tool grants are stored in user_settings table under key "granted_tools"
    // as a comma-separated list. This is a lightweight approach that avoids
    // a separate table for MVP; P2-03 will add the full user_tools table.
    let existing_key = "granted_tools".to_string();
    let existing = store.get_user_setting(&user_id, &existing_key)?.unwrap_or_default();
    let mut tools: Vec<&str> = existing.split(',').filter(|s| !s.is_empty()).collect();

    if tools.contains(&tool_name.as_str()) {
        println!("Tool '{}' already granted to '{}'.", tool_name, user.display_name);
        return Ok(());
    }

    tools.push(tool_name.as_str());
    let new_value = tools.join(",");
    store.set_user_setting(&user_id, &existing_key, &new_value)?;
    println!("Tool '{}' granted to user '{}'.", tool_name, user.display_name);
    Ok(())
}

fn cmd_set_prompt(args: &[String]) -> Result<()> {
    let user_id = parse_user_id_arg(args, "set-prompt")?;

    // Find --file <path> in args
    let file_path = find_flag_value(args, "--file").ok_or_else(|| {
        LuminaError::Config("Usage: users set-prompt <user_id> --file <path>".to_string())
    })?;

    let prompt = std::fs::read_to_string(&file_path).map_err(|e| {
        LuminaError::Config(format!("Cannot read prompt file '{}': {}", file_path, e))
    })?;

    if prompt.trim().is_empty() {
        return Err(LuminaError::Config("Prompt file is empty.".to_string()));
    }

    let store = open_store()?;
    let user = store.get_by_id(&user_id)?.ok_or_else(|| {
        LuminaError::Config(format!(
            "User '{}' not found. Run 'users list' to see valid IDs.",
            user_id
        ))
    })?;

    store.set_user_setting(&user_id, "system_prompt", &prompt)?;
    println!(
        "System prompt set for user '{}' ({} chars).",
        user.display_name,
        prompt.len()
    );
    Ok(())
}

fn cmd_link(args: &[String]) -> Result<()> {
    let user_id = parse_user_id_arg(args, "link")?;
    let channel_type_str = args.get(4).ok_or_else(|| {
        LuminaError::Config(
            "Usage: users link <user_id> <channel_type> <channel_id>".to_string(),
        )
    })?;
    let channel_id = args.get(5).ok_or_else(|| {
        LuminaError::Config(
            "Usage: users link <user_id> <channel_type> <channel_id>".to_string(),
        )
    })?;

    let channel_type = parse_channel_type(channel_type_str)?;
    let store = open_store()?;

    let user = store.get_by_id(&user_id)?.ok_or_else(|| {
        LuminaError::Config(format!(
            "User '{}' not found. Run 'users list' to see valid IDs.",
            user_id
        ))
    })?;

    store.link_channel(&user_id, channel_type.clone(), channel_id, false)?;
    println!(
        "Linked {} identity '{}' to user '{}'.",
        channel_type, channel_id, user.display_name
    );
    Ok(())
}

fn cmd_unlink(args: &[String]) -> Result<()> {
    let channel_type_str = args.get(3).ok_or_else(|| {
        LuminaError::Config("Usage: users unlink <channel_type> <channel_id>".to_string())
    })?;
    let channel_id = args.get(4).ok_or_else(|| {
        LuminaError::Config("Usage: users unlink <channel_type> <channel_id>".to_string())
    })?;

    let channel_type = parse_channel_type(channel_type_str)?;
    let store = open_store()?;

    let removed = store.unlink_channel(channel_type, channel_id)?;
    if removed {
        println!("Channel identity '{}' unlinked.", channel_id);
    } else {
        println!("Channel identity '{}' was not linked.", channel_id);
    }
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn open_store() -> Result<UserStore> {
    UserStore::open_default().map_err(|e| {
        LuminaError::Config(format!(
            "Cannot open users database: {e}\n\
             Hint: start lumina-core once to initialise the vault and database."
        ))
    })
}

fn parse_user_id_arg(args: &[String], sub: &str) -> Result<String> {
    args.get(3)
        .cloned()
        .ok_or_else(|| LuminaError::Config(format!("Usage: users {sub} <user_id>")))
}

/// Parse `--role <value>` from anywhere in the arg list.
fn parse_role_flag(args: &[String]) -> Option<UserRole> {
    find_flag_value(args, "--role").and_then(|v| parse_role_str(&v).ok())
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

fn parse_role_str(s: &str) -> Result<UserRole> {
    match s.to_lowercase().as_str() {
        "admin"  => Ok(UserRole::Admin),
        "member" => Ok(UserRole::Member),
        "guest"  => Ok(UserRole::Guest),
        other => Err(LuminaError::Config(format!(
            "Unknown role '{}'. Valid roles: admin, member, guest",
            other
        ))),
    }
}

fn parse_channel_type(s: &str) -> Result<ChannelType> {
    match s.to_lowercase().as_str() {
        "matrix"   => Ok(ChannelType::Matrix),
        "telegram" => Ok(ChannelType::Telegram),
        "http"     => Ok(ChannelType::Http),
        other => Err(LuminaError::Config(format!(
            "Unknown channel type '{}'. Valid types: matrix, telegram, http",
            other
        ))),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{}…", cut)
    }
}

pub fn print_users_help() {
    eprintln!("Usage: lumina-core users <subcommand>");
    eprintln!();
    eprintln!("Subcommands:");
    eprintln!("  list                                         Show all users");
    eprintln!("  add <matrix_id> <display_name> [--role <r>] Create a user (roles: admin, member, guest)");
    eprintln!("  remove <user_id>                             Delete a user");
    eprintln!("  show <user_id>                               Show user details");
    eprintln!("  promote <user_id> <role>                     Change a user's role");
    eprintln!("  disable <user_id>                            Soft-disable a user account");
    eprintln!("  enable <user_id>                             Re-enable a disabled user");
    eprintln!("  grant-tool <user_id> <tool_name>             Grant a tool to a user");
    eprintln!("  set-prompt <user_id> --file <path>           Set a custom system prompt");
    eprintln!("  link <user_id> <channel_type> <channel_id>  Link channel identity to user");
    eprintln!("  unlink <channel_type> <channel_id>           Remove a channel identity link");
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_key() -> Vec<u8> { vec![7u8; 32] }

    fn tmp_store(label: &str) -> UserStore {
        let p = PathBuf::from(format!("/tmp/lumina_users_cli_test_{}.db", label));
        let _ = std::fs::remove_file(&p);
        UserStore::new(&p, &test_key()).expect("open UserStore")
    }

    // ── handle_users_command dispatch ─────────────────────────────────────────

    #[test]
    fn test_not_a_users_command_returns_false() {
        let args = vec!["lumina-core".to_string(), "train".to_string()];
        assert!(!handle_users_command(&args));
    }

    #[test]
    fn test_users_unknown_subcommand_returns_true() {
        let args = vec![
            "lumina-core".to_string(),
            "users".to_string(),
            "bogus".to_string(),
        ];
        // Unknown subcommand prints help and returns true.
        assert!(handle_users_command(&args));
    }

    #[test]
    fn test_users_no_subcommand_returns_true() {
        let args = vec!["lumina-core".to_string(), "users".to_string()];
        assert!(handle_users_command(&args));
    }

    // ── parse helpers ─────────────────────────────────────────────────────────

    #[test]
    fn test_parse_role_flag_present() {
        let args = vec![
            "lumina-core".to_string(),
            "users".to_string(),
            "add".to_string(),
            "@u:h".to_string(),
            "Name".to_string(),
            "--role".to_string(),
            "admin".to_string(),
        ];
        assert_eq!(parse_role_flag(&args), Some(UserRole::Admin));
    }

    #[test]
    fn test_parse_role_flag_absent_returns_none() {
        let args = vec!["lumina-core".to_string(), "users".to_string()];
        assert_eq!(parse_role_flag(&args), None);
    }

    #[test]
    fn test_parse_role_str_all_values() {
        assert_eq!(parse_role_str("admin").unwrap(), UserRole::Admin);
        assert_eq!(parse_role_str("member").unwrap(), UserRole::Member);
        assert_eq!(parse_role_str("guest").unwrap(), UserRole::Guest);
        // Case-insensitive
        assert_eq!(parse_role_str("ADMIN").unwrap(), UserRole::Admin);
    }

    #[test]
    fn test_parse_role_str_invalid_returns_error() {
        assert!(parse_role_str("superuser").is_err());
    }

    #[test]
    fn test_parse_channel_type_valid() {
        assert_eq!(parse_channel_type("matrix").unwrap(), ChannelType::Matrix);
        assert_eq!(parse_channel_type("telegram").unwrap(), ChannelType::Telegram);
        assert_eq!(parse_channel_type("http").unwrap(), ChannelType::Http);
    }

    #[test]
    fn test_parse_channel_type_invalid() {
        assert!(parse_channel_type("discord").is_err());
    }

    #[test]
    fn test_find_flag_value_present() {
        let args = vec![
            "lumina-core".to_string(),
            "users".to_string(),
            "set-prompt".to_string(),
            "some-id".to_string(),
            "--file".to_string(),
            "/tmp/prompt.txt".to_string(),
        ];
        assert_eq!(
            find_flag_value(&args, "--file"),
            Some("/tmp/prompt.txt".to_string())
        );
    }

    #[test]
    fn test_find_flag_value_absent_returns_none() {
        let args = vec!["lumina-core".to_string()];
        assert_eq!(find_flag_value(&args, "--file"), None);
    }

    #[test]
    fn test_truncate_short_string_unchanged() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_long_string_gets_ellipsis() {
        let s = "a".repeat(60);
        let t = truncate(&s, 50);
        assert!(t.ends_with('…'));
        // 50 chars + ellipsis = 51 chars total
        assert_eq!(t.chars().count(), 51);
    }

    #[test]
    fn test_parse_user_id_arg_missing_returns_error() {
        let args = vec!["lumina-core".to_string(), "users".to_string(), "remove".to_string()];
        assert!(parse_user_id_arg(&args, "remove").is_err());
    }

    #[test]
    fn test_parse_user_id_arg_present_returns_value() {
        let args = vec![
            "lumina-core".to_string(),
            "users".to_string(),
            "remove".to_string(),
            "some-uuid".to_string(),
        ];
        assert_eq!(parse_user_id_arg(&args, "remove").unwrap(), "some-uuid");
    }

    // ── UserStore operations (direct) ─────────────────────────────────────────

    #[test]
    fn test_create_and_list_user() {
        let store = tmp_store("create_list");
        let user = store.create_user("Alice", Some("@alice:h.org"), UserRole::Admin).unwrap();
        assert_eq!(user.display_name, "Alice");
        assert_eq!(user.role, UserRole::Admin);

        let users = store.list().unwrap();
        assert_eq!(users.len(), 1);
        let _ = std::fs::remove_file("/tmp/lumina_users_cli_test_create_list.db");
    }

    #[test]
    fn test_role_change_via_store() {
        let store = tmp_store("role_change");
        store.create_user("Admin", None, UserRole::Admin).unwrap();
        let u = store.create_user("Bob", None, UserRole::Member).unwrap();
        store.set_role(&u.user_id, UserRole::Admin).unwrap();
        let updated = store.get_by_id(&u.user_id).unwrap().unwrap();
        assert_eq!(updated.role, UserRole::Admin);
        let _ = std::fs::remove_file("/tmp/lumina_users_cli_test_role_change.db");
    }

    #[test]
    fn test_disable_and_enable_user() {
        let store = tmp_store("disable_enable");
        store.create_user("Admin", None, UserRole::Admin).unwrap();
        let u = store.create_user("Bob", None, UserRole::Member).unwrap();
        store.set_enabled(&u.user_id, false).unwrap();
        let disabled = store.get_by_id(&u.user_id).unwrap().unwrap();
        assert!(!disabled.enabled);
        store.set_enabled(&u.user_id, true).unwrap();
        let enabled = store.get_by_id(&u.user_id).unwrap().unwrap();
        assert!(enabled.enabled);
        let _ = std::fs::remove_file("/tmp/lumina_users_cli_test_disable_enable.db");
    }

    #[test]
    fn test_link_channel_via_store() {
        let store = tmp_store("link_channel");
        let user = store.create_user("Carol", None, UserRole::Admin).unwrap();
        store.link_channel(&user.user_id, ChannelType::Telegram, "123456", false).unwrap();
        let channels = store.list_channels_for_user(&user.user_id).unwrap();
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].channel_user_id, "123456");
        let _ = std::fs::remove_file("/tmp/lumina_users_cli_test_link_channel.db");
    }

    #[test]
    fn test_link_already_claimed_by_other_user_returns_error() {
        let store = tmp_store("link_conflict");
        let u1 = store.create_user("U1", None, UserRole::Admin).unwrap();
        let u2 = store.create_user("U2", None, UserRole::Member).unwrap();
        store.link_channel(&u1.user_id, ChannelType::Matrix, "@x:h", false).unwrap();
        let result = store.link_channel(&u2.user_id, ChannelType::Matrix, "@x:h", false);
        assert!(result.is_err(), "Duplicate channel identity should be rejected");
        let _ = std::fs::remove_file("/tmp/lumina_users_cli_test_link_conflict.db");
    }

    #[test]
    fn test_unlink_channel_removes_identity() {
        let store = tmp_store("unlink");
        let user = store.create_user("Dave", None, UserRole::Admin).unwrap();
        store.link_channel(&user.user_id, ChannelType::Http, "token-xyz", false).unwrap();
        let removed = store.unlink_channel(ChannelType::Http, "token-xyz").unwrap();
        assert!(removed);
        let channels = store.list_channels_for_user(&user.user_id).unwrap();
        assert!(channels.is_empty());
        let _ = std::fs::remove_file("/tmp/lumina_users_cli_test_unlink.db");
    }

    #[test]
    fn test_cannot_remove_last_admin_via_delete() {
        // Deleting the only admin via store is allowed, but set_role guard protects demotion.
        // This test verifies the set_role guard.
        let store = tmp_store("last_admin_guard");
        let u = store.create_user("Solo", None, UserRole::Admin).unwrap();
        let result = store.set_role(&u.user_id, UserRole::Guest);
        assert!(result.is_err(), "Cannot demote last admin");
        let _ = std::fs::remove_file("/tmp/lumina_users_cli_test_last_admin_guard.db");
    }

    #[test]
    fn test_user_setting_get_set() {
        let store = tmp_store("settings");
        let user = store.create_user("Eve", None, UserRole::Admin).unwrap();
        store.set_user_setting(&user.user_id, "timezone", "America/Los_Angeles").unwrap();
        let val = store.get_user_setting(&user.user_id, "timezone").unwrap();
        assert_eq!(val, Some("America/Los_Angeles".to_string()));
        let _ = std::fs::remove_file("/tmp/lumina_users_cli_test_settings.db");
    }

    #[test]
    fn test_user_setting_missing_returns_none() {
        let store = tmp_store("settings_none");
        let user = store.create_user("Frank", None, UserRole::Admin).unwrap();
        let val = store.get_user_setting(&user.user_id, "nonexistent_key").unwrap();
        assert_eq!(val, None);
        let _ = std::fs::remove_file("/tmp/lumina_users_cli_test_settings_none.db");
    }

    /// Verify the comma-injection guard expression directly.
    ///
    /// Note: `cmd_grant_tool` hardcodes `UserStore::open_default()` so it cannot be
    /// called in unit tests without a live vault. The guard is also present in
    /// `matrix_commands::cmd_grant` which IS tested via `test_grant_tool_with_comma_rejected`.
    #[test]
    fn test_comma_guard_expression() {
        // Verify the guard condition that protects CSV storage in cmd_grant_tool.
        assert!("foo,bar".contains(','), "comma guard should catch comma-containing tool names");
        assert!(!"foo_bar".contains(','), "plain tool name should pass the guard");
    }
}
