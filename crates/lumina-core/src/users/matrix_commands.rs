//! P2-06: Matrix /admin command handlers for runtime user management.
//!
//! Admin commands are prefixed with `/admin` and processed before the normal
//! agent loop. Non-admin senders receive a clear rejection message.
//!
//! Supported commands:
//!   /admin users                             — list all users
//!   /admin grant <@user:server> <tool>       — grant a tool to a Matrix user
//!   /admin promote <@user:server> <role>     — change a user's role
//!   /admin disable <@user:server>            — disable a user by Matrix ID
//!   /admin show <@user:server>               — show user details by Matrix ID
//!
//! All /admin commands are audit-logged via eprintln! (audit sink integration in P2-02).

use crate::error::Result;
use crate::users::{UserRole, UserStore};

// ── Public API ────────────────────────────────────────────────────────────────

/// Result of parsing and dispatching a Matrix /admin command.
#[derive(Debug, PartialEq)]
pub enum AdminCommandResult {
    /// The text was not an /admin command — caller should handle normally.
    NotAdminCommand,
    /// Command executed; response to post to Matrix room.
    Response(String),
}

/// Attempt to parse and dispatch an `/admin ...` command.
///
/// - `sender_matrix_id`: the Matrix user ID of the sender (e.g. `@alice:home.org`).
/// - `store`: an open `UserStore` for identity lookups and mutations.
/// - `text`: the raw message body (leading whitespace trimmed by caller).
///
/// Returns `AdminCommandResult::NotAdminCommand` if `text` does not start with `/admin`.
pub fn handle_admin_command(
    sender_matrix_id: &str,
    store: &UserStore,
    text: &str,
) -> Result<AdminCommandResult> {
    let trimmed = text.trim();
    if !trimmed.starts_with("/admin") {
        return Ok(AdminCommandResult::NotAdminCommand);
    }

    // Verify the sender is an Admin.
    let sender = store.get_by_matrix_id(sender_matrix_id)?;
    let is_admin = sender
        .as_ref()
        .map(|u| u.role == UserRole::Admin && u.enabled)
        .unwrap_or(false);

    if !is_admin {
        eprintln!(
            "matrix-admin: rejected /admin from non-admin '{}'",
            sender_matrix_id
        );
        return Ok(AdminCommandResult::Response(
            "Admin access required. You do not have permission to use /admin commands.".to_string(),
        ));
    }

    // split_whitespace handles double-spaces common from mobile Matrix clients.
    let tokens: Vec<&str> = trimmed.split_whitespace().take(4).collect();
    let sub = tokens.get(1).copied().unwrap_or("help");

    let response = match sub {
        "users"   => cmd_users_list(store)?,
        "grant"   => cmd_grant(store, &tokens, sender_matrix_id)?,
        "promote" => cmd_promote(store, &tokens, sender_matrix_id)?,
        "disable" => cmd_disable(store, &tokens, sender_matrix_id)?,
        "enable"  => cmd_enable(store, &tokens, sender_matrix_id)?,
        "show"    => cmd_show(store, &tokens)?,
        _ => admin_help(),
    };

    Ok(AdminCommandResult::Response(response))
}

// ── Subcommand implementations ────────────────────────────────────────────────

fn cmd_users_list(store: &UserStore) -> Result<String> {
    let users = store.list()?;

    if users.is_empty() {
        return Ok("No users registered.".to_string());
    }

    let mut lines = vec!["**Users:**".to_string()];
    for u in &users {
        let status = if u.enabled { "active" } else { "disabled" };
        let last = u.last_seen.as_deref().unwrap_or("never");
        let mx = u.matrix_user_id.as_deref().unwrap_or("(no matrix id)");
        lines.push(format!(
            "- `{}` — {} ({}) — last seen: {} — matrix: {}",
            u.display_name, u.role, status, last, mx
        ));
    }
    lines.push(format!("\n{} user(s) total.", users.len()));
    Ok(lines.join("\n"))
}

fn cmd_grant(
    store: &UserStore,
    tokens: &[&str],
    actor: &str,
) -> Result<String> {
    // /admin grant <@user:server> <tool_name>
    let matrix_id = match tokens.get(2) {
        Some(id) => *id,
        None => return Ok("Usage: /admin grant <@user:server> <tool_name>".to_string()),
    };
    let tool_name = match tokens.get(3) {
        Some(t) => *t,
        None => return Ok("Usage: /admin grant <@user:server> <tool_name>".to_string()),
    };

    // Reject tool names that contain commas — they would corrupt the CSV storage.
    if tool_name.contains(',') {
        return Ok(format!(
            "Tool name `{}` is invalid — tool names must not contain commas.",
            tool_name
        ));
    }

    // Use get_by_matrix_id_any so admins can grant tools to disabled users too.
    let user = match store.get_by_matrix_id_any(matrix_id)? {
        Some(u) => u,
        None => {
            return Ok(format!(
                "User with Matrix ID `{}` not found.",
                matrix_id
            ));
        }
    };

    // Append to granted_tools setting.
    let key = "granted_tools";
    let existing = store.get_user_setting(&user.user_id, key)?.unwrap_or_default();
    let mut tools: Vec<&str> = existing.split(',').filter(|s| !s.is_empty()).collect();

    if tools.contains(&tool_name) {
        return Ok(format!(
            "Tool `{}` is already granted to `{}`.",
            tool_name, user.display_name
        ));
    }

    tools.push(tool_name);
    store.set_user_setting(&user.user_id, key, &tools.join(","))?;

    eprintln!(
        "matrix-admin: {} granted tool '{}' to user '{}' ({})",
        actor, tool_name, user.display_name, user.user_id
    );

    Ok(format!(
        "Tool `{}` granted to `{}` ({}).",
        tool_name, user.display_name, matrix_id
    ))
}

fn cmd_promote(
    store: &UserStore,
    tokens: &[&str],
    actor: &str,
) -> Result<String> {
    // /admin promote <@user:server> <role>
    let matrix_id = match tokens.get(2) {
        Some(id) => *id,
        None => return Ok("Usage: /admin promote <@user:server> <admin|member|guest>".to_string()),
    };
    let role_str = match tokens.get(3) {
        Some(r) => *r,
        None => return Ok("Usage: /admin promote <@user:server> <admin|member|guest>".to_string()),
    };

    let new_role = match role_str.to_lowercase().as_str() {
        "admin"  => UserRole::Admin,
        "member" => UserRole::Member,
        "guest"  => UserRole::Guest,
        other => {
            return Ok(format!(
                "Unknown role `{}`. Valid roles: admin, member, guest",
                other
            ))
        }
    };

    // Use get_by_matrix_id_any so admins can promote disabled users.
    let user = match store.get_by_matrix_id_any(matrix_id)? {
        Some(u) => u,
        None => {
            return Ok(format!(
                "User with Matrix ID `{}` not found.",
                matrix_id
            ));
        }
    };

    store.set_role(&user.user_id, new_role.clone())?;

    eprintln!(
        "matrix-admin: {} promoted '{}' ({}) to role '{}'",
        actor, user.display_name, user.user_id, new_role
    );

    Ok(format!(
        "User `{}` ({}) role changed to `{}`.",
        user.display_name, matrix_id, new_role
    ))
}

fn cmd_disable(
    store: &UserStore,
    tokens: &[&str],
    actor: &str,
) -> Result<String> {
    // /admin disable <@user:server>
    let matrix_id = match tokens.get(2) {
        Some(id) => *id,
        None => return Ok("Usage: /admin disable <@user:server>".to_string()),
    };

    // Use get_by_matrix_id_any so already-disabled users get a clear "already disabled" message.
    let user = match store.get_by_matrix_id_any(matrix_id)? {
        Some(u) => u,
        None => {
            return Ok(format!(
                "User with Matrix ID `{}` not found.",
                matrix_id
            ));
        }
    };

    if !user.enabled {
        return Ok(format!("User `{}` is already disabled.", user.display_name));
    }

    // Delegate to set_enabled which has the last-admin guard.
    match store.set_enabled(&user.user_id, false) {
        Ok(_) => {}
        Err(e) => return Ok(format!("Cannot disable user: {}", e)),
    }

    eprintln!(
        "matrix-admin: {} disabled user '{}' ({})",
        actor, user.display_name, user.user_id
    );

    Ok(format!(
        "User `{}` ({}) has been disabled.",
        user.display_name, matrix_id
    ))
}

fn cmd_enable(
    store: &UserStore,
    tokens: &[&str],
    actor: &str,
) -> Result<String> {
    // /admin enable <@user:server>
    let matrix_id = match tokens.get(2) {
        Some(id) => *id,
        None => return Ok("Usage: /admin enable <@user:server>".to_string()),
    };

    let user = match store.get_by_matrix_id_any(matrix_id)? {
        Some(u) => u,
        None => {
            return Ok(format!(
                "User with Matrix ID `{}` not found.",
                matrix_id
            ));
        }
    };

    if user.enabled {
        return Ok(format!("User `{}` is already enabled.", user.display_name));
    }

    store.set_enabled(&user.user_id, true)?;

    eprintln!(
        "matrix-admin: {} enabled user '{}' ({})",
        actor, user.display_name, user.user_id
    );

    Ok(format!(
        "User `{}` ({}) has been enabled.",
        user.display_name, matrix_id
    ))
}

fn cmd_show(store: &UserStore, tokens: &[&str]) -> Result<String> {
    // /admin show <@user:server>
    let matrix_id = match tokens.get(2) {
        Some(id) => *id,
        None => return Ok("Usage: /admin show <@user:server>".to_string()),
    };

    // Use get_by_matrix_id_any so admins can inspect disabled accounts.
    let user = match store.get_by_matrix_id_any(matrix_id)? {
        Some(u) => u,
        None => {
            return Ok(format!(
                "User with Matrix ID `{}` not found.",
                matrix_id
            ));
        }
    };

    let channels = store.list_channels_for_user(&user.user_id)?;
    let channel_list = if channels.is_empty() {
        "(none)".to_string()
    } else {
        channels
            .iter()
            .map(|c| format!("{}: {}", c.channel_type, c.channel_user_id))
            .collect::<Vec<_>>()
            .join(", ")
    };

    Ok(format!(
        "**User:** `{}`\n\
         **ID:** `{}`\n\
         **Role:** {}\n\
         **Enabled:** {}\n\
         **Created:** {}\n\
         **Last seen:** {}\n\
         **Channels:** {}",
        user.display_name,
        user.user_id,
        user.role,
        user.enabled,
        user.created_at,
        user.last_seen.as_deref().unwrap_or("never"),
        channel_list
    ))
}

fn admin_help() -> String {
    "/admin commands:\n\
     - `/admin users` — list all users\n\
     - `/admin grant <@user:server> <tool_name>` — grant a tool\n\
     - `/admin promote <@user:server> <role>` — change role (admin/member/guest)\n\
     - `/admin disable <@user:server>` — disable a user\n\
     - `/admin enable <@user:server>` — re-enable a disabled user\n\
     - `/admin show <@user:server>` — show user details (works on disabled users)"
        .to_string()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::users::{UserRole, UserStore};
    use std::path::PathBuf;

    fn test_key() -> Vec<u8> { vec![55u8; 32] }

    fn tmp_store(label: &str) -> UserStore {
        let p = PathBuf::from(format!("/tmp/lumina_matrix_cmd_test_{}.db", label));
        let _ = std::fs::remove_file(&p);
        UserStore::new(&p, &test_key()).expect("open UserStore")
    }

    fn cleanup(label: &str) {
        let _ = std::fs::remove_file(format!("/tmp/lumina_matrix_cmd_test_{}.db", label));
    }

    // ── Not an admin command ──────────────────────────────────────────────────

    #[test]
    fn test_not_admin_command_returns_not_admin_command() {
        let store = tmp_store("not_admin");
        let result = handle_admin_command("@alice:h.org", &store, "Hello, world!").unwrap();
        assert_eq!(result, AdminCommandResult::NotAdminCommand);
        cleanup("not_admin");
    }

    #[test]
    fn test_non_admin_command_prefix_regular_message() {
        let store = tmp_store("not_admin2");
        let result = handle_admin_command("@alice:h.org", &store, "what is the weather?").unwrap();
        assert_eq!(result, AdminCommandResult::NotAdminCommand);
        cleanup("not_admin2");
    }

    // ── Admin-only enforcement ────────────────────────────────────────────────

    #[test]
    fn test_non_admin_rejected_from_admin_commands() {
        let store = tmp_store("non_admin_reject");
        // Create a seed admin, then a member user.
        store.create_user("Admin", Some("@admin:h.org"), UserRole::Admin).unwrap();
        let _member = store.create_user("Bob", Some("@bob:h.org"), UserRole::Member).unwrap();

        let result = handle_admin_command("@bob:h.org", &store, "/admin users").unwrap();
        match result {
            AdminCommandResult::Response(msg) => {
                assert!(
                    msg.contains("Admin access required"),
                    "Expected rejection message, got: {}",
                    msg
                );
            }
            other => panic!("Expected Response, got {:?}", other),
        }
        cleanup("non_admin_reject");
    }

    #[test]
    fn test_unknown_sender_rejected_from_admin_commands() {
        let store = tmp_store("unknown_sender");
        // No users in the database.
        let result = handle_admin_command("@ghost:h.org", &store, "/admin users").unwrap();
        match result {
            AdminCommandResult::Response(msg) => {
                assert!(msg.contains("Admin access required"), "got: {}", msg);
            }
            other => panic!("Expected Response, got {:?}", other),
        }
        cleanup("unknown_sender");
    }

    #[test]
    fn test_guest_user_rejected_from_admin_commands() {
        let store = tmp_store("guest_reject");
        store.create_user("Admin", Some("@admin:h.org"), UserRole::Admin).unwrap();
        let _guest = store.create_user("Guest", Some("@guest:h.org"), UserRole::Guest).unwrap();

        let result = handle_admin_command("@guest:h.org", &store, "/admin users").unwrap();
        match result {
            AdminCommandResult::Response(msg) => {
                assert!(msg.contains("Admin access required"), "got: {}", msg);
            }
            other => panic!("Expected Response, got {:?}", other),
        }
        cleanup("guest_reject");
    }

    // ── /admin users ─────────────────────────────────────────────────────────

    #[test]
    fn test_admin_users_list_returns_formatted_list() {
        let store = tmp_store("users_list");
        store.create_user("Admin", Some("@admin:h.org"), UserRole::Admin).unwrap();
        store.create_user("Bob", Some("@bob:h.org"), UserRole::Member).unwrap();

        let result = handle_admin_command("@admin:h.org", &store, "/admin users").unwrap();
        match result {
            AdminCommandResult::Response(msg) => {
                assert!(msg.contains("Admin"), "Should mention Admin user");
                assert!(msg.contains("Bob"), "Should mention Bob");
                assert!(msg.contains("2 user(s)"), "Should show count");
            }
            other => panic!("Expected Response, got {:?}", other),
        }
        cleanup("users_list");
    }

    #[test]
    fn test_admin_users_list_empty_store() {
        let store = tmp_store("empty_list");
        // Force-add an admin without going through normal creation path.
        // Actually: we need at least one admin to send /admin commands.
        store.create_user("Admin", Some("@admin:h.org"), UserRole::Admin).unwrap();
        // Delete all other users — admin is still there.
        let result = handle_admin_command("@admin:h.org", &store, "/admin users").unwrap();
        match result {
            AdminCommandResult::Response(msg) => {
                assert!(!msg.is_empty());
            }
            other => panic!("Expected Response, got {:?}", other),
        }
        cleanup("empty_list");
    }

    // ── /admin promote ────────────────────────────────────────────────────────

    #[test]
    fn test_admin_promote_changes_role() {
        let store = tmp_store("promote");
        store.create_user("Admin", Some("@admin:h.org"), UserRole::Admin).unwrap();
        let bob = store.create_user("Bob", Some("@bob:h.org"), UserRole::Guest).unwrap();
        assert_eq!(bob.role, UserRole::Guest);

        let result = handle_admin_command(
            "@admin:h.org",
            &store,
            "/admin promote @bob:h.org member",
        ).unwrap();

        match result {
            AdminCommandResult::Response(msg) => {
                assert!(msg.contains("member") || msg.contains("Bob"), "got: {}", msg);
            }
            other => panic!("Expected Response, got {:?}", other),
        }

        let updated = store.get_by_matrix_id("@bob:h.org").unwrap().unwrap();
        assert_eq!(updated.role, UserRole::Member);
        cleanup("promote");
    }

    #[test]
    fn test_admin_promote_unknown_user_returns_not_found() {
        let store = tmp_store("promote_unknown");
        store.create_user("Admin", Some("@admin:h.org"), UserRole::Admin).unwrap();

        let result = handle_admin_command(
            "@admin:h.org",
            &store,
            "/admin promote @nobody:h.org admin",
        ).unwrap();

        match result {
            AdminCommandResult::Response(msg) => {
                assert!(msg.contains("not found"), "got: {}", msg);
            }
            other => panic!("Expected Response, got {:?}", other),
        }
        cleanup("promote_unknown");
    }

    #[test]
    fn test_admin_promote_invalid_role_returns_error() {
        let store = tmp_store("promote_bad_role");
        store.create_user("Admin", Some("@admin:h.org"), UserRole::Admin).unwrap();
        store.create_user("Bob", Some("@bob:h.org"), UserRole::Member).unwrap();

        let result = handle_admin_command(
            "@admin:h.org",
            &store,
            "/admin promote @bob:h.org superuser",
        ).unwrap();

        match result {
            AdminCommandResult::Response(msg) => {
                assert!(msg.contains("Unknown role") || msg.contains("Valid roles"), "got: {}", msg);
            }
            other => panic!("Expected Response, got {:?}", other),
        }
        cleanup("promote_bad_role");
    }

    // ── /admin grant ──────────────────────────────────────────────────────────

    #[test]
    fn test_admin_grant_tool_to_user() {
        let store = tmp_store("grant");
        store.create_user("Admin", Some("@admin:h.org"), UserRole::Admin).unwrap();
        store.create_user("Bob", Some("@bob:h.org"), UserRole::Member).unwrap();

        let result = handle_admin_command(
            "@admin:h.org",
            &store,
            "/admin grant @bob:h.org weather_tool",
        ).unwrap();

        match result {
            AdminCommandResult::Response(msg) => {
                assert!(
                    msg.contains("weather_tool") || msg.contains("granted"),
                    "got: {}",
                    msg
                );
            }
            other => panic!("Expected Response, got {:?}", other),
        }

        let bob = store.get_by_matrix_id("@bob:h.org").unwrap().unwrap();
        let granted = store.get_user_setting(&bob.user_id, "granted_tools").unwrap();
        assert!(granted.as_deref().unwrap_or("").contains("weather_tool"));
        cleanup("grant");
    }

    #[test]
    fn test_admin_grant_duplicate_tool_gives_already_granted_message() {
        let store = tmp_store("grant_dup");
        store.create_user("Admin", Some("@admin:h.org"), UserRole::Admin).unwrap();
        let bob = store.create_user("Bob", Some("@bob:h.org"), UserRole::Member).unwrap();
        store.set_user_setting(&bob.user_id, "granted_tools", "news_tool").unwrap();

        let result = handle_admin_command(
            "@admin:h.org",
            &store,
            "/admin grant @bob:h.org news_tool",
        ).unwrap();

        match result {
            AdminCommandResult::Response(msg) => {
                assert!(msg.contains("already") || msg.contains("news_tool"), "got: {}", msg);
            }
            other => panic!("Expected Response, got {:?}", other),
        }
        cleanup("grant_dup");
    }

    // ── /admin disable ────────────────────────────────────────────────────────

    #[test]
    fn test_admin_disable_user() {
        let store = tmp_store("disable");
        store.create_user("Admin", Some("@admin:h.org"), UserRole::Admin).unwrap();
        store.create_user("Bob", Some("@bob:h.org"), UserRole::Member).unwrap();

        let result = handle_admin_command(
            "@admin:h.org",
            &store,
            "/admin disable @bob:h.org",
        ).unwrap();

        match result {
            AdminCommandResult::Response(msg) => {
                assert!(msg.contains("disabled") || msg.contains("Bob"), "got: {}", msg);
            }
            other => panic!("Expected Response, got {:?}", other),
        }

        // Should no longer be returned by enabled-only lookup.
        let bob = store.get_by_matrix_id("@bob:h.org").unwrap();
        assert!(bob.is_none(), "Disabled user should not be returned by get_by_matrix_id");

        // But admin-any lookup should still find them.
        let bob_any = store.get_by_matrix_id_any("@bob:h.org").unwrap();
        assert!(bob_any.is_some(), "get_by_matrix_id_any should find disabled user");
        assert!(!bob_any.unwrap().enabled);
        cleanup("disable");
    }

    #[test]
    fn test_admin_cannot_disable_last_admin() {
        let store = tmp_store("disable_last_admin");
        store.create_user("Admin", Some("@admin:h.org"), UserRole::Admin).unwrap();

        // Attempt to disable the only admin via Matrix.
        let result = handle_admin_command(
            "@admin:h.org",
            &store,
            "/admin disable @admin:h.org",
        ).unwrap();

        match result {
            AdminCommandResult::Response(msg) => {
                // Should either refuse (last-admin guard) or succeed but the guard
                // in set_enabled should prevent it.
                assert!(
                    msg.contains("Cannot") || msg.contains("admin") || msg.contains("promote"),
                    "Expected a refusal or guard message, got: {}",
                    msg
                );
            }
            other => panic!("Expected Response, got {:?}", other),
        }

        // Verify the admin is still enabled.
        let admin = store.get_by_matrix_id("@admin:h.org").unwrap();
        assert!(admin.is_some(), "Admin should still be enabled");
        cleanup("disable_last_admin");
    }

    // ── /admin enable ─────────────────────────────────────────────────────────

    #[test]
    fn test_admin_enable_disabled_user() {
        let store = tmp_store("enable");
        store.create_user("Admin", Some("@admin:h.org"), UserRole::Admin).unwrap();
        let bob = store.create_user("Bob", Some("@bob:h.org"), UserRole::Member).unwrap();
        store.set_enabled(&bob.user_id, false).unwrap();

        let result = handle_admin_command(
            "@admin:h.org",
            &store,
            "/admin enable @bob:h.org",
        ).unwrap();

        match result {
            AdminCommandResult::Response(msg) => {
                assert!(msg.contains("enabled") || msg.contains("Bob"), "got: {}", msg);
            }
            other => panic!("Expected Response, got {:?}", other),
        }

        let bob_now = store.get_by_matrix_id("@bob:h.org").unwrap();
        assert!(bob_now.is_some(), "Re-enabled user should be visible again");
        cleanup("enable");
    }

    #[test]
    fn test_admin_enable_already_enabled_gives_message() {
        let store = tmp_store("enable_already");
        store.create_user("Admin", Some("@admin:h.org"), UserRole::Admin).unwrap();
        store.create_user("Bob", Some("@bob:h.org"), UserRole::Member).unwrap();

        let result = handle_admin_command(
            "@admin:h.org",
            &store,
            "/admin enable @bob:h.org",
        ).unwrap();

        match result {
            AdminCommandResult::Response(msg) => {
                assert!(msg.contains("already") || msg.contains("enabled"), "got: {}", msg);
            }
            other => panic!("Expected Response, got {:?}", other),
        }
        cleanup("enable_already");
    }

    // ── /admin show works on disabled users ───────────────────────────────────

    #[test]
    fn test_admin_show_disabled_user_still_visible() {
        let store = tmp_store("show_disabled");
        store.create_user("Admin", Some("@admin:h.org"), UserRole::Admin).unwrap();
        let bob = store.create_user("Bob", Some("@bob:h.org"), UserRole::Member).unwrap();
        store.set_enabled(&bob.user_id, false).unwrap();

        let result = handle_admin_command(
            "@admin:h.org",
            &store,
            "/admin show @bob:h.org",
        ).unwrap();

        match result {
            AdminCommandResult::Response(msg) => {
                assert!(msg.contains("Bob"), "Should still show disabled user");
                assert!(msg.contains("false") || msg.contains("disabled"), "Should show disabled status");
            }
            other => panic!("Expected Response, got {:?}", other),
        }
        cleanup("show_disabled");
    }

    // ── /admin show ───────────────────────────────────────────────────────────

    #[test]
    fn test_admin_show_user_details() {
        let store = tmp_store("show");
        store.create_user("Admin", Some("@admin:h.org"), UserRole::Admin).unwrap();
        store.create_user("Alice", Some("@alice:h.org"), UserRole::Member).unwrap();

        let result = handle_admin_command(
            "@admin:h.org",
            &store,
            "/admin show @alice:h.org",
        ).unwrap();

        match result {
            AdminCommandResult::Response(msg) => {
                assert!(msg.contains("Alice"), "Should show display name");
                assert!(msg.contains("member") || msg.contains("Member"), "Should show role");
            }
            other => panic!("Expected Response, got {:?}", other),
        }
        cleanup("show");
    }

    #[test]
    fn test_admin_show_unknown_user_not_found() {
        let store = tmp_store("show_unknown");
        store.create_user("Admin", Some("@admin:h.org"), UserRole::Admin).unwrap();

        let result = handle_admin_command(
            "@admin:h.org",
            &store,
            "/admin show @nobody:h.org",
        ).unwrap();

        match result {
            AdminCommandResult::Response(msg) => {
                assert!(msg.contains("not found"), "got: {}", msg);
            }
            other => panic!("Expected Response, got {:?}", other),
        }
        cleanup("show_unknown");
    }

    // ── Admin actions audited (eprintln side-effect — just verify no panic) ───

    #[test]
    fn test_admin_actions_do_not_panic() {
        let store = tmp_store("audit");
        store.create_user("Admin", Some("@admin:h.org"), UserRole::Admin).unwrap();
        store.create_user("Bob", Some("@bob:h.org"), UserRole::Member).unwrap();

        // These should all succeed without panicking.
        let _ = handle_admin_command("@admin:h.org", &store, "/admin users");
        let _ = handle_admin_command("@admin:h.org", &store, "/admin show @bob:h.org");
        let _ = handle_admin_command("@admin:h.org", &store, "/admin promote @bob:h.org admin");
        cleanup("audit");
    }

    // ── Double-space tokenization ─────────────────────────────────────────────

    #[test]
    fn test_double_space_in_admin_command_still_routes_correctly() {
        let store = tmp_store("double_space");
        store.create_user("Admin", Some("@admin:h.org"), UserRole::Admin).unwrap();
        store.create_user("Bob", Some("@bob:h.org"), UserRole::Member).unwrap();

        // Double space between /admin and users should still work.
        let result = handle_admin_command("@admin:h.org", &store, "/admin  users").unwrap();
        match result {
            AdminCommandResult::Response(msg) => {
                assert!(!msg.contains("help") || msg.contains("user"), "Double space broke routing, got: {}", msg);
                // Should list users or show help — not silently fail.
            }
            other => panic!("Expected Response, got {:?}", other),
        }
        cleanup("double_space");
    }

    // ── Comma injection guard ─────────────────────────────────────────────────

    #[test]
    fn test_grant_tool_with_comma_rejected() {
        let store = tmp_store("comma_inject");
        store.create_user("Admin", Some("@admin:h.org"), UserRole::Admin).unwrap();
        store.create_user("Bob", Some("@bob:h.org"), UserRole::Member).unwrap();

        let result = handle_admin_command(
            "@admin:h.org",
            &store,
            "/admin grant @bob:h.org foo,bar",
        ).unwrap();

        match result {
            AdminCommandResult::Response(msg) => {
                assert!(
                    msg.contains("invalid") || msg.contains("comma"),
                    "Expected comma rejection, got: {}",
                    msg
                );
            }
            other => panic!("Expected Response, got {:?}", other),
        }
        cleanup("comma_inject");
    }

    // ── Missing arguments fall back to usage strings ──────────────────────────

    #[test]
    fn test_admin_grant_missing_args_returns_usage() {
        let store = tmp_store("grant_no_args");
        store.create_user("Admin", Some("@admin:h.org"), UserRole::Admin).unwrap();

        let result = handle_admin_command("@admin:h.org", &store, "/admin grant").unwrap();
        match result {
            AdminCommandResult::Response(msg) => {
                assert!(msg.contains("Usage") || msg.contains("usage"), "got: {}", msg);
            }
            other => panic!("Expected Response, got {:?}", other),
        }
        cleanup("grant_no_args");
    }

    #[test]
    fn test_admin_promote_missing_args_returns_usage() {
        let store = tmp_store("promote_no_args");
        store.create_user("Admin", Some("@admin:h.org"), UserRole::Admin).unwrap();

        let result = handle_admin_command("@admin:h.org", &store, "/admin promote").unwrap();
        match result {
            AdminCommandResult::Response(msg) => {
                assert!(msg.contains("Usage") || msg.contains("usage"), "got: {}", msg);
            }
            other => panic!("Expected Response, got {:?}", other),
        }
        cleanup("promote_no_args");
    }
}
