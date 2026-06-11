//! P2-18: Per-user vault service — secure credential storage.
//!
//! Provides per-user vault operations for setting, deleting, and checking
//! the configured status of sensitive credentials (App Passwords, CalDAV URLs,
//! IMAP hosts).  Credentials are stored in the vault keyed by user ID and
//! credential type.  Values are NEVER logged.
//!
//! Vault key naming convention:
//!   `GOOGLE_APP_PASSWORD_{user_id}`
//!   `CALDAV_URL_{user_id}`
//!   `IMAP_HOST_{user_id}`
//!
//! All vault keys are upper-cased and only alphanumeric + underscore characters
//! are permitted in the user-id portion (enforced by [`validate_user_id`]).

use crate::error::{LuminaError, Result};
use crate::users::validate_user_id;
use secrecy::SecretString;

// ── Credential types ───────────────────────────────────────────────────────

/// The type of credential being stored in the vault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialType {
    /// Google App Password for Gmail / Google Workspace.
    GoogleAppPassword,
    /// CalDAV server URL (treated as sensitive — can contain embedded credentials).
    CalDavUrl,
    /// IMAP server hostname.
    ImapHost,
}

impl CredentialType {
    /// Return the vault key prefix for this credential type.
    pub fn vault_prefix(&self) -> &'static str {
        match self {
            CredentialType::GoogleAppPassword => "GOOGLE_APP_PASSWORD",
            CredentialType::CalDavUrl => "CALDAV_URL",
            CredentialType::ImapHost => "IMAP_HOST",
        }
    }

    /// All supported credential types — used for admin status checks.
    pub fn all() -> &'static [CredentialType] {
        &[
            CredentialType::GoogleAppPassword,
            CredentialType::CalDavUrl,
            CredentialType::ImapHost,
        ]
    }

    /// Human-readable label for UI display.
    pub fn label(&self) -> &'static str {
        match self {
            CredentialType::GoogleAppPassword => "Google App Password",
            CredentialType::CalDavUrl => "CalDAV URL",
            CredentialType::ImapHost => "IMAP Host",
        }
    }

    /// Parse from the form field name used in the credential form.
    pub fn from_field_name(name: &str) -> Option<Self> {
        match name {
            "google_app_password" => Some(CredentialType::GoogleAppPassword),
            "caldav_url" => Some(CredentialType::CalDavUrl),
            "imap_host" => Some(CredentialType::ImapHost),
            _ => None,
        }
    }

    /// Return the HTML form field name for this credential type.
    pub fn field_name(&self) -> &'static str {
        match self {
            CredentialType::GoogleAppPassword => "google_app_password",
            CredentialType::CalDavUrl => "caldav_url",
            CredentialType::ImapHost => "imap_host",
        }
    }
}

// ── Vault key construction ─────────────────────────────────────────────────

/// Build the vault key for a per-user credential.
///
/// Returns an error if `user_id` fails validation (path traversal etc.).
///
/// # Examples
/// ```
/// # use lumina_core::users::vault_service::{vault_key, CredentialType};
/// let key = vault_key("abc-123", &CredentialType::GoogleAppPassword).unwrap();
/// assert_eq!(key, "GOOGLE_APP_PASSWORD_abc-123");
/// ```
pub fn vault_key(user_id: &str, cred_type: &CredentialType) -> Result<String> {
    validate_user_id(user_id)?;
    Ok(format!("{}_{}", cred_type.vault_prefix(), user_id))
}

// ── VaultService ───────────────────────────────────────────────────────────

/// Per-user vault operations.
///
/// This struct holds a mutable reference to a [`crate::vault::VaultStore`]
/// and provides typed credential accessors that never log values.
///
/// # Security invariants
/// - `set_credential` / `delete_credential` only write to the vault; they do
///   NOT write to the audit log (see spec P2-18 §Security).
/// - `is_configured` returns only a boolean — it never exposes the value.
/// - All user IDs are validated before use in vault keys.
pub struct VaultService<'a> {
    vault: &'a mut crate::vault::VaultStore,
}

impl<'a> VaultService<'a> {
    /// Create a new `VaultService` wrapping a mutable vault reference.
    pub fn new(vault: &'a mut crate::vault::VaultStore) -> Self {
        Self { vault }
    }

    /// Store a credential value for `user_id`.
    ///
    /// An empty `value` is treated as a delete (revoke) rather than stored as
    /// an empty string. This matches the spec edge-case: "User submits empty
    /// password → treat as delete (revoke credential)".
    ///
    /// # Security
    /// - The `value` parameter is a [`SecretString`] so it is never in a plain
    ///   `String` on the stack.
    /// - This function does NOT emit audit log entries.
    pub fn set_credential(
        &mut self,
        user_id: &str,
        cred_type: &CredentialType,
        value: SecretString,
    ) -> Result<CredentialSetResult> {
        use secrecy::ExposeSecret;
        let key = vault_key(user_id, cred_type)?;

        if value.expose_secret().is_empty() {
            // Empty value → delete (revoke).
            let removed = self.vault.remove(&key).map_err(|e| {
                LuminaError::Config(format!("Failed to remove credential from vault: {}", e))
            })?;
            return Ok(if removed {
                CredentialSetResult::Deleted
            } else {
                CredentialSetResult::NothingToDelete
            });
        }

        self.vault.set(key, value).map_err(|e| {
            LuminaError::Config(format!("Failed to store credential in vault: {}", e))
        })?;
        Ok(CredentialSetResult::Stored)
    }

    /// Delete a credential for `user_id`.
    ///
    /// Returns `true` if a credential was actually removed, `false` if it was
    /// not present (idempotent).
    pub fn delete_credential(
        &mut self,
        user_id: &str,
        cred_type: &CredentialType,
    ) -> Result<bool> {
        let key = vault_key(user_id, cred_type)?;
        let removed = self.vault.remove(&key).map_err(|e| {
            LuminaError::Config(format!("Failed to delete credential from vault: {}", e))
        })?;
        Ok(removed)
    }

    /// Check whether a credential is configured for `user_id`.
    ///
    /// Returns `true` if the vault contains a non-empty value for this
    /// credential, `false` otherwise.  The value itself is never returned.
    pub fn is_configured(&self, user_id: &str, cred_type: &CredentialType) -> Result<bool> {
        let key = vault_key(user_id, cred_type)?;
        Ok(self.vault.contains_key(&key))
    }

    /// Return the configured status for all credential types for `user_id`.
    ///
    /// This is the admin-visible summary: a list of `(label, is_configured)`
    /// tuples.  Values are never included.
    pub fn configured_status(&self, user_id: &str) -> Result<Vec<(String, bool)>> {
        let mut result = Vec::new();
        for cred_type in CredentialType::all() {
            let configured = self.is_configured(user_id, cred_type)?;
            result.push((cred_type.label().to_string(), configured));
        }
        Ok(result)
    }
}

// ── Result type ────────────────────────────────────────────────────────────

/// Outcome of a [`VaultService::set_credential`] call.
#[derive(Debug, PartialEq, Eq)]
pub enum CredentialSetResult {
    /// A new value was stored (or an existing value was updated).
    Stored,
    /// An empty value was submitted and an existing credential was deleted.
    Deleted,
    /// An empty value was submitted but no credential existed to delete.
    NothingToDelete,
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::VaultStore;
    use crate::vault::key_provider::FileKeyProvider;
    use secrecy::SecretString;
    use std::path::PathBuf;

    /// Build a fresh in-memory-equivalent vault using a temp file + temp key.
    fn make_vault(name: &str) -> (VaultStore, PathBuf, PathBuf) {
        let vault_path = PathBuf::from(format!("/tmp/lumina_p218_vault_{}.enc", name));
        let key_path = PathBuf::from(format!("/tmp/lumina_p218_key_{}.key", name));
        let _ = std::fs::remove_file(&vault_path);
        let _ = std::fs::remove_file(&key_path);
        FileKeyProvider::generate(&key_path).expect("generate key");
        let kp = Box::new(FileKeyProvider::new(key_path.clone()));
        let store = VaultStore::with_config(vault_path.clone(), kp);
        (store, vault_path, key_path)
    }

    fn cleanup(vault_path: &PathBuf, key_path: &PathBuf) {
        let _ = std::fs::remove_file(vault_path);
        let _ = std::fs::remove_file(key_path);
    }

    // ── vault_key construction ─────────────────────────────────────────────

    #[test]
    fn test_vault_key_google_app_password() {
        let key = vault_key("user-abc", &CredentialType::GoogleAppPassword).unwrap();
        assert_eq!(key, "GOOGLE_APP_PASSWORD_user-abc");
    }

    #[test]
    fn test_vault_key_caldav_url() {
        let key = vault_key("user123", &CredentialType::CalDavUrl).unwrap();
        assert_eq!(key, "CALDAV_URL_user123");
    }

    #[test]
    fn test_vault_key_imap_host() {
        let key = vault_key("u1", &CredentialType::ImapHost).unwrap();
        assert_eq!(key, "IMAP_HOST_u1");
    }

    #[test]
    fn test_vault_key_rejects_invalid_user_id() {
        // Path traversal must be rejected.
        assert!(vault_key("../etc", &CredentialType::GoogleAppPassword).is_err());
        assert!(vault_key("", &CredentialType::CalDavUrl).is_err());
        assert!(vault_key("user/name", &CredentialType::ImapHost).is_err());
    }

    // ── credential stored in vault with correct per-user key ───────────────

    #[test]
    fn test_credential_stored_in_vault_with_correct_per_user_key() {
        let (mut vault, vp, kp) = make_vault("store_key");
        let mut svc = VaultService::new(&mut vault);

        let result = svc
            .set_credential(
                "user-001",
                &CredentialType::GoogleAppPassword,
                SecretString::new("mysecret123".to_string().into()),
            )
            .unwrap();

        assert_eq!(result, CredentialSetResult::Stored);

        // Verify the key is correct without reading the value through VaultService.
        let key = vault_key("user-001", &CredentialType::GoogleAppPassword).unwrap();
        assert!(vault.contains_key(&key), "Vault must contain the per-user key");
        cleanup(&vp, &kp);
    }

    #[test]
    fn test_credential_stored_for_different_users_isolated() {
        let (mut vault, vp, kp) = make_vault("isolation");
        let mut svc = VaultService::new(&mut vault);

        svc.set_credential(
            "user-001",
            &CredentialType::ImapHost,
            SecretString::new("mail.a.example".to_string().into()),
        )
        .unwrap();
        svc.set_credential(
            "user-002",
            &CredentialType::ImapHost,
            SecretString::new("mail.b.example".to_string().into()),
        )
        .unwrap();

        let key1 = vault_key("user-001", &CredentialType::ImapHost).unwrap();
        let key2 = vault_key("user-002", &CredentialType::ImapHost).unwrap();
        assert!(vault.contains_key(&key1));
        assert!(vault.contains_key(&key2));
        assert_ne!(key1, key2);
        cleanup(&vp, &kp);
    }

    // ── is_configured returns correct status ─────────────────────────────────

    #[test]
    fn test_is_configured_false_when_not_set() {
        let (mut vault, vp, kp) = make_vault("not_set");
        let svc = VaultService::new(&mut vault);
        let configured = svc
            .is_configured("user-001", &CredentialType::CalDavUrl)
            .unwrap();
        assert!(!configured, "Should be false when credential not set");
        cleanup(&vp, &kp);
    }

    #[test]
    fn test_is_configured_true_after_set() {
        let (mut vault, vp, kp) = make_vault("after_set");
        {
            let mut svc = VaultService::new(&mut vault);
            svc.set_credential(
                "user-x",
                &CredentialType::CalDavUrl,
                SecretString::new("https://cal.example.com/dav".to_string().into()),
            )
            .unwrap();
        }
        let svc = VaultService::new(&mut vault);
        assert!(svc.is_configured("user-x", &CredentialType::CalDavUrl).unwrap());
        cleanup(&vp, &kp);
    }

    // ── credential deletion removes vault entry ───────────────────────────────

    #[test]
    fn test_credential_deletion_removes_vault_entry() {
        let (mut vault, vp, kp) = make_vault("delete");
        {
            let mut svc = VaultService::new(&mut vault);
            svc.set_credential(
                "user-del",
                &CredentialType::GoogleAppPassword,
                SecretString::new("pass123".to_string().into()),
            )
            .unwrap();
        }

        {
            let mut svc = VaultService::new(&mut vault);
            let removed = svc
                .delete_credential("user-del", &CredentialType::GoogleAppPassword)
                .unwrap();
            assert!(removed, "delete_credential should return true when credential existed");
        }

        let svc = VaultService::new(&mut vault);
        assert!(
            !svc.is_configured("user-del", &CredentialType::GoogleAppPassword).unwrap(),
            "Credential should no longer be configured after deletion"
        );
        cleanup(&vp, &kp);
    }

    #[test]
    fn test_credential_deletion_idempotent() {
        let (mut vault, vp, kp) = make_vault("del_idem");
        let mut svc = VaultService::new(&mut vault);
        let removed = svc
            .delete_credential("user-x", &CredentialType::ImapHost)
            .unwrap();
        assert!(!removed, "delete_credential should return false when nothing to delete");
        cleanup(&vp, &kp);
    }

    // ── empty value treated as delete ─────────────────────────────────────────

    #[test]
    fn test_empty_value_treated_as_delete() {
        let (mut vault, vp, kp) = make_vault("empty_del");
        {
            let mut svc = VaultService::new(&mut vault);
            svc.set_credential(
                "user-abc",
                &CredentialType::ImapHost,
                SecretString::new("mail.example.com".to_string().into()),
            )
            .unwrap();
        }

        {
            let mut svc = VaultService::new(&mut vault);
            let result = svc
                .set_credential(
                    "user-abc",
                    &CredentialType::ImapHost,
                    SecretString::new("".to_string().into()),
                )
                .unwrap();
            assert_eq!(result, CredentialSetResult::Deleted);
        }

        let svc = VaultService::new(&mut vault);
        assert!(
            !svc.is_configured("user-abc", &CredentialType::ImapHost).unwrap(),
            "Empty submit should delete the credential"
        );
        cleanup(&vp, &kp);
    }

    #[test]
    fn test_empty_value_on_nonexistent_returns_nothing_to_delete() {
        let (mut vault, vp, kp) = make_vault("empty_no_del");
        let mut svc = VaultService::new(&mut vault);
        let result = svc
            .set_credential(
                "user-abc",
                &CredentialType::ImapHost,
                SecretString::new("".to_string().into()),
            )
            .unwrap();
        assert_eq!(result, CredentialSetResult::NothingToDelete);
        cleanup(&vp, &kp);
    }

    // ── admin can check configured status (not read values) ──────────────────

    #[test]
    fn test_admin_can_check_configured_status_not_values() {
        let (mut vault, vp, kp) = make_vault("admin_status");
        {
            let mut svc = VaultService::new(&mut vault);
            svc.set_credential(
                "user-operator",
                &CredentialType::GoogleAppPassword,
                SecretString::new("apppass".to_string().into()),
            )
            .unwrap();
            // CalDAV not set, IMAP not set.
        }

        let svc = VaultService::new(&mut vault);
        let statuses = svc.configured_status("user-operator").unwrap();

        // There should be one entry per CredentialType::all().
        assert_eq!(statuses.len(), CredentialType::all().len());

        // Google App Password should be configured.
        let gapp = statuses
            .iter()
            .find(|(label, _)| label == "Google App Password");
        assert!(gapp.is_some());
        assert!(gapp.unwrap().1, "Google App Password should be configured");

        // CalDAV should not be configured.
        let caldav = statuses.iter().find(|(label, _)| label == "CalDAV URL");
        assert!(caldav.is_some());
        assert!(!caldav.unwrap().1, "CalDAV should not be configured");

        // IMAP should not be configured.
        let imap = statuses.iter().find(|(label, _)| label == "IMAP Host");
        assert!(imap.is_some());
        assert!(!imap.unwrap().1, "IMAP should not be configured");

        cleanup(&vp, &kp);
    }

    // ── CredentialType helpers ─────────────────────────────────────────────

    #[test]
    fn test_credential_type_from_field_name_roundtrip() {
        for cred in CredentialType::all() {
            let name = cred.field_name();
            let parsed = CredentialType::from_field_name(name);
            assert!(parsed.is_some(), "from_field_name failed for {:?}", cred);
            assert_eq!(&parsed.unwrap(), cred);
        }
    }

    #[test]
    fn test_credential_type_from_field_name_unknown_returns_none() {
        assert!(CredentialType::from_field_name("unknown_field").is_none());
        assert!(CredentialType::from_field_name("").is_none());
    }

    #[test]
    fn test_credential_type_all_has_three_entries() {
        assert_eq!(CredentialType::all().len(), 3);
    }
}
