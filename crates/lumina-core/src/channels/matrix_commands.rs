//! DPROMPT-10: Informational Matrix commands for prompt visibility.
//!
//! These let a user see *how Lumina sees them* — the current trait values, the
//! reconstructed personality vector, and the knowledge digest. They are pure,
//! read-only formatters with no network access and no clock dependency.
//!
//! ## Commands
//! - `/traits`       → current trait values as text bars.
//! - `/personality`  → the full personality-vector layer (~200 words).
//! - `/digest`       → the full knowledge-digest layer (~300 words).
//! - `/admin traits <user>` → admin views another user's traits (see scoping).
//!
//! ## Scoping (caller responsibility)
//! [`dispatch_prompt_command`] takes a `target_user` already resolved by the
//! caller. The message path is responsible for authorization:
//! - A self command (`/traits`) resolves `target_user` to the sender's own id.
//! - An admin command (`/admin traits <user>`) is only routed here with the
//!   requested `target_user` *after* the caller has verified the sender is an
//!   admin (`is_admin == true`). When a non-admin attempts an admin command the
//!   caller must refuse before dispatching; [`admin_traits_command`] returns a
//!   refusal string if asked to render for a non-admin so the policy is also
//!   enforced defensively here.
//!
//! This module only reads layer files under `{layers_root}/{user_id}/`; it
//! never decides who may view whom.

use std::path::Path;

use crate::prompt::traits::{TraitVector, TRAIT_MAX, TRAIT_MIN};

/// Width (in cells) of a text trait bar.
const BAR_WIDTH: usize = 10;

/// Filenames of the file-backed layers (mirrors the prompt module constants).
const DIGEST_FILE: &str = "knowledge-digest.txt";
const PERSONALITY_FILE: &str = "personality-vector.txt";
const TRAIT_FILE: &str = "trait-vector.json";

/// Keep user ids filesystem-safe — mirrors `prompt::sanitize_user`.
fn sanitize_user(user_id: &str) -> String {
    let cleaned: String = user_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "default".to_string()
    } else {
        cleaned
    }
}

/// Build a text trait bar like `████████░░` for `value` in `[TRAIT_MIN, TRAIT_MAX]`.
fn text_bar(value: f32) -> String {
    let v = value.clamp(TRAIT_MIN, TRAIT_MAX);
    let span = (TRAIT_MAX - TRAIT_MIN).max(f32::EPSILON);
    let frac = (v - TRAIT_MIN) / span;
    let mut filled = (frac * BAR_WIDTH as f32).round() as usize;
    if filled > BAR_WIDTH {
        filled = BAR_WIDTH;
    }
    let empty = BAR_WIDTH - filled;
    let mut bar = String::with_capacity(BAR_WIDTH * 3);
    for _ in 0..filled {
        bar.push('█');
    }
    for _ in 0..empty {
        bar.push('░');
    }
    bar
}

/// Read a layer file, returning `None` when absent/empty.
fn read_layer(path: &Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        _ => None,
    }
}

// ── Pure formatters ──────────────────────────────────────────────────────────

/// Format a trait vector as aligned text bars, one per trait.
///
/// ```text
/// flair       ████████░░ 0.70
/// spontaneity █████░░░░░ 0.55
/// humor       ███████░░░ 0.65
/// focus       █████████░ 0.75
/// ```
pub fn format_traits(tv: &TraitVector) -> String {
    let rows = [
        ("flair", tv.flair),
        ("spontaneity", tv.spontaneity),
        ("humor", tv.humor),
        ("focus", tv.focus),
    ];
    let mut out = String::from("Current traits:\n");
    for (name, value) in rows {
        out.push_str(&format!("{name:<11} {bar} {value:.2}\n", bar = text_bar(value)));
    }
    out.trim_end().to_string()
}

/// Format the personality vector for display (full text).
pub fn format_personality(text: &str) -> String {
    let t = text.trim();
    if t.is_empty() {
        "Pending first consolidation".to_string()
    } else {
        format!("How I see your personality:\n\n{t}")
    }
}

/// Format the knowledge digest for display (full text).
pub fn format_digest(text: &str) -> String {
    let t = text.trim();
    if t.is_empty() {
        "Pending first consolidation".to_string()
    } else {
        format!("What I know about you:\n\n{t}")
    }
}

// ── Dispatch ─────────────────────────────────────────────────────────────────

/// Dispatch a self-scoped prompt command (`/traits`, `/personality`, `/digest`).
///
/// Reads the layer files for `user_id` under `layers_root`. Returns `None` when
/// `cmd` is not one of the recognised commands (so the caller can fall through
/// to other handlers). Whitespace and a leading slash are tolerated; the match
/// is case-insensitive on the first token, and any trailing arguments are
/// ignored (these commands take none).
pub fn dispatch_prompt_command(cmd: &str, user_id: &str, layers_root: &Path) -> Option<String> {
    let first = cmd.trim().split_whitespace().next()?;
    let key = first.trim_start_matches('/').to_ascii_lowercase();
    let user_dir = layers_root.join(sanitize_user(user_id));
    match key.as_str() {
        "traits" => {
            let tv = TraitVector::load(&user_dir.join(TRAIT_FILE));
            Some(format_traits(&tv))
        }
        "personality" => {
            let body = read_layer(&user_dir.join(PERSONALITY_FILE)).unwrap_or_default();
            Some(format_personality(&body))
        }
        "digest" => {
            let body = read_layer(&user_dir.join(DIGEST_FILE)).unwrap_or_default();
            Some(format_digest(&body))
        }
        _ => None,
    }
}

/// Render another user's traits on behalf of an admin (`/admin traits <user>`).
///
/// Scoping is a caller responsibility, but this enforces it defensively: when
/// `is_admin` is `false` it returns a refusal string instead of leaking another
/// user's data. The caller is expected to have parsed `target_user` out of the
/// `/admin traits <user>` command.
pub fn admin_traits_command(is_admin: bool, target_user: &str, layers_root: &Path) -> String {
    if !is_admin {
        return "Not permitted: viewing another user's traits requires admin.".to_string();
    }
    let user_dir = layers_root.join(sanitize_user(target_user));
    let tv = TraitVector::load(&user_dir.join(TRAIT_FILE));
    format!("Traits for {target_user}:\n{}", format_traits(&tv))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_user_file(root: &Path, user: &str, name: &str, body: &str) {
        let dir = root.join(super::sanitize_user(user));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn format_traits_has_bars_and_values() {
        let s = format_traits(&TraitVector::default());
        assert!(s.contains("flair"));
        assert!(s.contains("humor"));
        assert!(s.contains('█'));
        assert!(s.contains('░'));
        assert!(s.contains("0.65")); // default humor
    }

    #[test]
    fn text_bar_bounds() {
        assert_eq!(text_bar(TRAIT_MIN).chars().filter(|c| *c == '█').count(), 0);
        assert_eq!(text_bar(TRAIT_MAX).chars().filter(|c| *c == '█').count(), BAR_WIDTH);
        // Clamps out-of-range without panic.
        assert_eq!(text_bar(5.0).chars().filter(|c| *c == '█').count(), BAR_WIDTH);
    }

    #[test]
    fn format_personality_and_digest_pending_when_empty() {
        assert_eq!(format_personality("   "), "Pending first consolidation");
        assert_eq!(format_digest(""), "Pending first consolidation");
    }

    #[test]
    fn format_personality_and_digest_show_text() {
        assert!(format_personality("systems thinker").contains("systems thinker"));
        assert!(format_digest("marketing manager").contains("marketing manager"));
    }

    #[test]
    fn dispatch_traits_reads_user_vector() {
        let dir = tempdir().unwrap();
        // Write a non-default vector for operator.
        let tv = TraitVector { flair: 0.20, spontaneity: 0.20, humor: 0.20, focus: 0.20 };
        let udir = dir.path().join("operator");
        std::fs::create_dir_all(&udir).unwrap();
        tv.save(&udir.join(TRAIT_FILE)).unwrap();

        let out = dispatch_prompt_command("/traits", "operator", dir.path()).unwrap();
        assert!(out.contains("0.20"));
        assert!(out.contains('█') || out.contains('░'));
    }

    #[test]
    fn dispatch_traits_defaults_when_absent() {
        let dir = tempdir().unwrap();
        let out = dispatch_prompt_command("/traits", "newbie", dir.path()).unwrap();
        assert!(out.contains("0.65")); // default humor
    }

    #[test]
    fn dispatch_personality_and_digest() {
        let dir = tempdir().unwrap();
        write_user_file(dir.path(), "operator", PERSONALITY_FILE, "values directness");
        write_user_file(dir.path(), "operator", DIGEST_FILE, "lives in a city by the bay");

        let p = dispatch_prompt_command("/personality", "operator", dir.path()).unwrap();
        assert!(p.contains("values directness"));
        let d = dispatch_prompt_command("/digest", "operator", dir.path()).unwrap();
        assert!(d.contains("lives in a city by the bay"));
    }

    #[test]
    fn dispatch_is_case_insensitive_and_tolerates_args() {
        let dir = tempdir().unwrap();
        assert!(dispatch_prompt_command("/TRAITS", "u", dir.path()).is_some());
        assert!(dispatch_prompt_command("traits", "u", dir.path()).is_some());
        assert!(dispatch_prompt_command("/traits extra args", "u", dir.path()).is_some());
    }

    #[test]
    fn dispatch_non_command_returns_none() {
        let dir = tempdir().unwrap();
        assert!(dispatch_prompt_command("hello there", "u", dir.path()).is_none());
        assert!(dispatch_prompt_command("", "u", dir.path()).is_none());
        assert!(dispatch_prompt_command("/help", "u", dir.path()).is_none());
    }

    #[test]
    fn dispatch_pending_personality_when_absent() {
        let dir = tempdir().unwrap();
        let out = dispatch_prompt_command("/personality", "newbie", dir.path()).unwrap();
        assert!(out.contains("Pending first consolidation"));
    }

    #[test]
    fn admin_can_view_other_user_non_admin_refused() {
        let dir = tempdir().unwrap();
        let tv = TraitVector { flair: 0.30, spontaneity: 0.30, humor: 0.30, focus: 0.30 };
        let udir = dir.path().join("alice");
        std::fs::create_dir_all(&udir).unwrap();
        tv.save(&udir.join(TRAIT_FILE)).unwrap();

        let admin_view = admin_traits_command(true, "alice", dir.path());
        assert!(admin_view.contains("Traits for alice"));
        assert!(admin_view.contains("0.30"));

        let denied = admin_traits_command(false, "alice", dir.path());
        assert!(denied.contains("Not permitted"));
        assert!(!denied.contains("0.30"));
    }

    #[test]
    fn no_hardcoded_infra_in_output() {
        let dir = tempdir().unwrap();
        let out = dispatch_prompt_command("/traits", "u", dir.path()).unwrap();
        let needle = format!("{}.{}", "192", "168");
        assert!(!out.contains(&needle));
    }
}
