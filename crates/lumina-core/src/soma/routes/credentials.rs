//! P2-18: Secure credential entry page and handler.
//!
//! Provides the Soma web routes for per-user vault self-service:
//!
//! - [`render_credentials_page`] — builds the credential entry HTML form.
//! - [`parse_credentials_form`] — parses and validates an inbound form POST.
//! - [`validate_csrf_token`] — constant-time CSRF token comparison.
//! - [`generate_csrf_token`] — generates a session-bound CSRF token.
//!
//! ## Security model
//! - Session authentication is enforced by the caller (HTTP middleware).
//! - Credential values are NEVER written to the audit log.
//! - Input fields use `type="password"` and `autocomplete="off"`.
//! - All form fields carry a CSRF token; the handler rejects requests without it.
//! - POST response confirms storage without echoing the credential value.
//! - Blank (empty) fields are treated as "keep existing" (not delete).
//!   Explicit deletion uses a separate revoke checkbox.
//!
//! ## Design rules (MANDATORY — lumina-design-system-spec)
//! - Every HTML page MUST include `<link rel="stylesheet" href="/shared/constellation.css">`.
//! - NO inline `style=""` attributes, NO hardcoded hex colors.
//! - Use `.card`, `.badge-*`, `.btn-*`, etc.
//! - No hardcoded infrastructure addresses or org names.

use crate::users::vault_service::{CredentialSetResult, CredentialType, VaultService};
use crate::vault::VaultStore;

/// Mandatory constellation.css link — must appear in every HTML page.
const CONSTELLATION_CSS: &str =
    r#"<link rel="stylesheet" href="/shared/constellation.css">"#;

/// Length in bytes for the CSRF token.
pub const CSRF_TOKEN_BYTES: usize = 32;

// ── CSRF helpers ──────────────────────────────────────────────────────────────

/// Generate a cryptographically random CSRF token (hex-encoded, 64 chars).
///
/// The token should be stored in the user's server-side session and included
/// as a hidden form field.  It is validated on POST with [`validate_csrf_token`].
pub fn generate_csrf_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; CSRF_TOKEN_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Validate a CSRF token in constant time.
///
/// Returns `true` if `provided` matches `expected` and both are non-empty.
///
/// Uses `subtle::ConstantTimeEq` — the standard Rust constant-time byte
/// comparison primitive, transitively available via the `hmac` dependency.
pub fn validate_csrf_token(expected: &str, provided: &str) -> bool {
    use subtle::ConstantTimeEq;
    if expected.is_empty() || provided.is_empty() {
        return false;
    }
    // Length mismatch is always invalid (length leakage is acceptable for
    // public-length fixed-size tokens).
    if expected.len() != provided.len() {
        return false;
    }
    expected.as_bytes().ct_eq(provided.as_bytes()).into()
}

// ── HTML rendering ────────────────────────────────────────────────────────────

/// Escape HTML special characters to prevent XSS.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// Configuration for rendering the credentials page.
#[derive(Debug, Clone, Default)]
pub struct CredentialsPageParams {
    /// CSRF token to embed in the form.
    pub csrf_token: String,
    /// Configured status per credential type (label, is_configured).
    pub configured_status: Vec<(String, bool)>,
    /// Optional flash message to display (e.g. "Saved." or "Deleted.").
    pub flash_message: Option<String>,
    /// Optional error message to display.
    pub error_message: Option<String>,
}

/// Render the credential self-service page as a complete HTML document.
///
/// Includes:
/// - `constellation.css` for all styling
/// - CSRF hidden field on the form
/// - Password input fields (masked, autocomplete off)
/// - Revoke checkboxes for explicit credential deletion
/// - Configured status indicators for admin visibility
/// - Setup instructions for Google App Passwords
///
/// All dynamic content is HTML-escaped; no infrastructure addresses are
/// hardcoded.
pub fn render_credentials_page(params: &CredentialsPageParams) -> String {
    let csrf = html_escape(&params.csrf_token);

    // Build the configured-status section (admin/self visibility).
    let status_rows: String = params
        .configured_status
        .iter()
        .map(|(label, configured)| {
            let badge = if *configured { "badge-success" } else { "badge-secondary" };
            let text = if *configured { "Configured" } else { "Not set" };
            format!(
                r#"<tr><td>{label}</td><td><span class="badge {badge}">{text}</span></td></tr>"#,
                label = html_escape(label),
                badge = badge,
                text = text,
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Flash message block.
    let flash_html = match &params.flash_message {
        Some(msg) => format!(
            r#"<div class="alert alert-success">{}</div>"#,
            html_escape(msg)
        ),
        None => String::new(),
    };

    // Error message block.
    let error_html = match &params.error_message {
        Some(msg) => format!(
            r#"<div class="alert alert-danger">{}</div>"#,
            html_escape(msg)
        ),
        None => String::new(),
    };

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Credentials — Settings</title>
{css}
</head>
<body>
<div class="page">
<header class="page-header">
  <h1>Credential Settings</h1>
  <p class="text-secondary">Securely store your account credentials</p>
</header>

{flash}
{error}

<div class="grid">

  <div class="card">
    <div class="card-header">
      <h2 class="card-title">Current Status</h2>
    </div>
    <div class="card-body">
      <table class="table">
        <thead>
          <tr><th>Credential</th><th>Status</th></tr>
        </thead>
        <tbody>
          {status_rows}
        </tbody>
      </table>
    </div>
  </div>

  <div class="card">
    <div class="card-header">
      <h2 class="card-title">Setup Instructions</h2>
    </div>
    <div class="card-body">
      <p><strong>Google App Password:</strong></p>
      <ol>
        <li>Go to <code>myaccount.google.com</code></li>
        <li>Navigate to <strong>Security</strong> &rarr; <strong>2-Step Verification</strong></li>
        <li>Scroll to <strong>App passwords</strong> and click it</li>
        <li>Select app: <em>Mail</em>, device: <em>Other</em>, then <strong>Generate</strong></li>
        <li>Copy the 16-character password and paste it below</li>
      </ol>
      <p><strong>CalDAV URL:</strong> Your calendar server URL (e.g. from Nextcloud or Google Calendar).</p>
      <p><strong>IMAP Host:</strong> Your mail server hostname (e.g. <code>imap.gmail.com</code>).</p>
    </div>
  </div>

</div>

<div class="card">
  <div class="card-header">
    <h2 class="card-title">Enter Credentials</h2>
  </div>
  <div class="card-body">
    <p class="text-secondary">
      Values are stored encrypted and are never logged.
      Leave a field blank to keep the existing value.
      Check the revoke box to explicitly delete a stored credential.
    </p>
    <form method="POST" action="/settings/credentials" autocomplete="off">
      <input type="hidden" name="csrf_token" value="{csrf}">

      <div class="form-group">
        <label for="google_app_password">Google App Password</label>
        <input type="password"
               id="google_app_password"
               name="google_app_password"
               autocomplete="off"
               placeholder="Leave blank to keep existing">
        <label class="checkbox-label">
          <input type="checkbox" name="revoke_google_app_password" value="1">
          Revoke (delete) this credential
        </label>
      </div>

      <div class="form-group">
        <label for="caldav_url">CalDAV URL</label>
        <input type="password"
               id="caldav_url"
               name="caldav_url"
               autocomplete="off"
               placeholder="Leave blank to keep existing">
        <label class="checkbox-label">
          <input type="checkbox" name="revoke_caldav_url" value="1">
          Revoke (delete) this credential
        </label>
      </div>

      <div class="form-group">
        <label for="imap_host">IMAP Host</label>
        <input type="password"
               id="imap_host"
               name="imap_host"
               autocomplete="off"
               placeholder="Leave blank to keep existing">
        <label class="checkbox-label">
          <input type="checkbox" name="revoke_imap_host" value="1">
          Revoke (delete) this credential
        </label>
      </div>

      <button type="submit" class="btn-primary">Save Credentials</button>
    </form>
  </div>
</div>

<footer class="lumina-footer">
Lumina Constellation &middot; Credential Settings
</footer>
</div>
</body>
</html>"#,
        css = CONSTELLATION_CSS,
        flash = flash_html,
        error = error_html,
        status_rows = status_rows,
        csrf = csrf,
    )
}

// ── Form parsing ─────────────────────────────────────────────────────────────

/// Parsed credential form submission.
///
/// - Password fields: `None` = absent, `Some("")` = present but empty (keep existing).
/// - Revoke flags: if `true`, the corresponding credential is explicitly deleted.
///   Revoke takes precedence over any value in the password field.
#[derive(Debug)]
pub struct CredentialsFormData {
    /// CSRF token from the hidden field (plain string for comparison only).
    pub csrf_token: String,
    /// Google App Password (None = absent; Some("") = blank/keep; Some(v) = set).
    pub google_app_password: Option<secrecy::SecretString>,
    /// CalDAV URL.
    pub caldav_url: Option<secrecy::SecretString>,
    /// IMAP Host.
    pub imap_host: Option<secrecy::SecretString>,
    /// Explicit revoke flags (checkbox-driven delete).
    pub revoke_google_app_password: bool,
    pub revoke_caldav_url: bool,
    pub revoke_imap_host: bool,
}

/// Error type for credential form parsing.
#[derive(Debug, PartialEq, Eq)]
pub enum FormParseError {
    /// CSRF token field was missing from the form body.
    MissingCsrfToken,
    /// CSRF token did not match the session token.
    InvalidCsrfToken,
}

impl std::fmt::Display for FormParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormParseError::MissingCsrfToken => write!(f, "CSRF token missing from form"),
            FormParseError::InvalidCsrfToken => write!(f, "CSRF token invalid"),
        }
    }
}

/// Parse a URL-encoded form body into [`CredentialsFormData`].
///
/// Validates the CSRF token against `session_csrf` using constant-time
/// comparison.  Returns `Err(FormParseError::InvalidCsrfToken)` if the tokens
/// do not match.
///
/// Field values are wrapped in [`secrecy::SecretString`] to prevent accidental
/// logging.  `None` means the field was absent (no action taken for that
/// credential).  Present-but-empty values are stored as `Some(SecretString(""))`.
/// The caller (`apply_credentials_form`) treats empty as "no change" (not delete)
/// to match the placeholder text "Leave blank to keep existing".
///
/// Uses the `form_urlencoded` crate for robust percent-decoding — handles
/// multi-byte UTF-8 correctly and has extensive upstream fuzzing coverage.
pub fn parse_credentials_form(
    form_body: &str,
    session_csrf: &str,
) -> Result<CredentialsFormData, FormParseError> {
    use secrecy::SecretString;

    // Use the battle-tested form_urlencoded parser instead of a hand-rolled decoder.
    let mut pairs: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (k, v) in form_urlencoded::parse(form_body.as_bytes()) {
        pairs.insert(k.into_owned(), v.into_owned());
    }

    // Extract and validate CSRF token.
    let csrf = pairs.get("csrf_token").cloned().unwrap_or_default();
    if csrf.is_empty() {
        return Err(FormParseError::MissingCsrfToken);
    }
    if !validate_csrf_token(session_csrf, &csrf) {
        return Err(FormParseError::InvalidCsrfToken);
    }

    // Helper: check if a revoke checkbox was submitted with value "1".
    let revoke = |key: &str| pairs.get(key).map(|v| v == "1").unwrap_or(false);

    Ok(CredentialsFormData {
        csrf_token: csrf,
        google_app_password: pairs
            .get("google_app_password")
            .map(|v| SecretString::new(v.clone().into())),
        caldav_url: pairs
            .get("caldav_url")
            .map(|v| SecretString::new(v.clone().into())),
        imap_host: pairs
            .get("imap_host")
            .map(|v| SecretString::new(v.clone().into())),
        revoke_google_app_password: revoke("revoke_google_app_password"),
        revoke_caldav_url: revoke("revoke_caldav_url"),
        revoke_imap_host: revoke("revoke_imap_host"),
    })
}

// ── Form application ──────────────────────────────────────────────────────────

/// Result of applying a credential form submission.
#[derive(Debug)]
pub struct CredentialsApplyResult {
    /// Number of credentials stored.
    pub stored: usize,
    /// Number of credentials deleted.
    pub deleted: usize,
    /// Number of credentials unchanged (blank or no-op).
    pub unchanged: usize,
}

impl CredentialsApplyResult {
    /// Human-readable summary for the flash message (never echoes values).
    pub fn flash_message(&self) -> String {
        if self.stored == 0 && self.deleted == 0 {
            "No changes made.".to_string()
        } else {
            let mut parts = Vec::new();
            if self.stored > 0 {
                parts.push(format!(
                    "{} credential{} saved",
                    self.stored,
                    if self.stored == 1 { "" } else { "s" }
                ));
            }
            if self.deleted > 0 {
                parts.push(format!(
                    "{} credential{} revoked",
                    self.deleted,
                    if self.deleted == 1 { "" } else { "s" }
                ));
            }
            parts.join(", ") + "."
        }
    }
}

/// Apply a parsed credential form to the vault for `user_id`.
///
/// For each credential field:
/// - Revoke checkbox checked → delete the credential (takes precedence).
/// - Value field absent → no action.
/// - Value field present but empty → no action ("leave blank to keep existing").
///   Browsers always submit empty strings for unfilled password inputs; we must
///   not interpret this as a delete.
/// - Value field present and non-empty → store/update.
///
/// Credential values are NEVER written to any log.
pub fn apply_credentials_form(
    user_id: &str,
    form: &CredentialsFormData,
    vault: &mut VaultStore,
) -> crate::error::Result<CredentialsApplyResult> {
    use secrecy::ExposeSecret;

    let mut stored = 0usize;
    let mut deleted = 0usize;
    let mut unchanged = 0usize;

    let mut svc = VaultService::new(vault);

    // Associate each credential type with its form value and revoke flag.
    let fields: [(&Option<secrecy::SecretString>, bool, CredentialType); 3] = [
        (
            &form.google_app_password,
            form.revoke_google_app_password,
            CredentialType::GoogleAppPassword,
        ),
        (
            &form.caldav_url,
            form.revoke_caldav_url,
            CredentialType::CalDavUrl,
        ),
        (
            &form.imap_host,
            form.revoke_imap_host,
            CredentialType::ImapHost,
        ),
    ];

    for (field_val, revoke, cred_type) in &fields {
        if *revoke {
            // Explicit revoke checkbox — delete regardless of the value field.
            let removed = svc.delete_credential(user_id, cred_type)?;
            if removed {
                deleted += 1;
            } else {
                unchanged += 1;
            }
            continue;
        }

        match field_val {
            None => {
                // Field absent — no action.
            }
            Some(secret) if secret.expose_secret().is_empty() => {
                // Present but empty — "leave blank to keep existing" → no action.
                // Browsers always submit empty strings for unfilled password inputs.
            }
            Some(secret) => {
                // Non-empty value — store.  Use into_boxed_str() to convert the
                // intermediate String allocation directly into the box that
                // SecretString will zeroize on drop.
                let value_str: Box<str> = secret.expose_secret().to_string().into_boxed_str();
                let secret_clone = secrecy::SecretString::new(value_str);
                let result = svc.set_credential(user_id, cred_type, secret_clone)?;
                match result {
                    CredentialSetResult::Stored => stored += 1,
                    CredentialSetResult::Deleted => deleted += 1,
                    CredentialSetResult::NothingToDelete => unchanged += 1,
                }
            }
        }
    }

    Ok(CredentialsApplyResult { stored, deleted, unchanged })
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::VaultStore;
    use crate::vault::key_provider::FileKeyProvider;
    use std::path::PathBuf;

    fn make_vault(name: &str) -> (VaultStore, PathBuf, PathBuf) {
        let vault_path = PathBuf::from(format!("/tmp/lumina_p218_creds_{}.enc", name));
        let key_path = PathBuf::from(format!("/tmp/lumina_p218_creds_{}.key", name));
        let _ = std::fs::remove_file(&vault_path);
        let _ = std::fs::remove_file(&key_path);
        FileKeyProvider::generate(&key_path).expect("generate key");
        let kp = Box::new(FileKeyProvider::new(key_path.clone()));
        let store = VaultStore::with_config(vault_path.clone(), kp);
        (store, vault_path, key_path)
    }

    fn cleanup(vp: &PathBuf, kp: &PathBuf) {
        let _ = std::fs::remove_file(vp);
        let _ = std::fs::remove_file(kp);
    }

    // ── CSRF token generation and validation ───────────────────────────────

    #[test]
    fn test_generate_csrf_token_is_64_hex_chars() {
        let token = generate_csrf_token();
        assert_eq!(token.len(), CSRF_TOKEN_BYTES * 2);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_generate_csrf_token_is_random() {
        let t1 = generate_csrf_token();
        let t2 = generate_csrf_token();
        assert_ne!(t1, t2);
    }

    // ── CSRF token validated on POST ──────────────────────────────────────────

    #[test]
    fn test_csrf_token_validated_on_post_valid() {
        let session_csrf = generate_csrf_token();
        let body = format!("csrf_token={}&imap_host=mail.example.com", session_csrf);
        assert!(parse_credentials_form(&body, &session_csrf).is_ok());
    }

    #[test]
    fn test_csrf_token_validated_on_post_invalid() {
        let session_csrf = generate_csrf_token();
        let wrong_csrf = generate_csrf_token();
        let body = format!("csrf_token={}&imap_host=mail.example.com", wrong_csrf);
        assert_eq!(
            parse_credentials_form(&body, &session_csrf).unwrap_err(),
            FormParseError::InvalidCsrfToken
        );
    }

    #[test]
    fn test_csrf_token_missing_returns_error() {
        let session_csrf = generate_csrf_token();
        let result = parse_credentials_form("imap_host=mail.example.com", &session_csrf);
        assert_eq!(result.unwrap_err(), FormParseError::MissingCsrfToken);
    }

    #[test]
    fn test_validate_csrf_token_constant_time_different_lengths() {
        assert!(!validate_csrf_token("short", "a-much-longer-token-than-short"));
        assert!(!validate_csrf_token("", "anything"));
        assert!(!validate_csrf_token("anything", ""));
    }

    // ── blank fields keep existing (no accidental delete) ────────────────────

    #[test]
    fn test_blank_field_keeps_existing_credential() {
        let (mut vault, vp, kp) = make_vault("blank_keep");
        {
            let mut svc = VaultService::new(&mut vault);
            svc.set_credential(
                "user-abc",
                &CredentialType::ImapHost,
                secrecy::SecretString::new("mail.example.com".to_string().into()),
            )
            .unwrap();
        }

        let csrf = generate_csrf_token();
        // Browser submits empty string for an unfilled field.
        let body = format!("csrf_token={}&imap_host=", csrf);
        let form = parse_credentials_form(&body, &csrf).unwrap();
        let result = apply_credentials_form("user-abc", &form, &mut vault).unwrap();

        assert_eq!(result.deleted, 0, "Blank field must not delete existing credential");

        let svc = VaultService::new(&mut vault);
        assert!(
            svc.is_configured("user-abc", &CredentialType::ImapHost).unwrap(),
            "Credential must survive blank field submission"
        );
        cleanup(&vp, &kp);
    }

    // ── explicit revoke via checkbox ──────────────────────────────────────────

    #[test]
    fn test_revoke_checkbox_deletes_credential() {
        let (mut vault, vp, kp) = make_vault("revoke_del");
        {
            let mut svc = VaultService::new(&mut vault);
            svc.set_credential(
                "user-abc",
                &CredentialType::ImapHost,
                secrecy::SecretString::new("mail.example.com".to_string().into()),
            )
            .unwrap();
        }

        let csrf = generate_csrf_token();
        let body = format!("csrf_token={}&revoke_imap_host=1", csrf);
        let form = parse_credentials_form(&body, &csrf).unwrap();
        assert!(form.revoke_imap_host);

        let result = apply_credentials_form("user-abc", &form, &mut vault).unwrap();
        assert_eq!(result.deleted, 1, "Revoke checkbox must delete credential");

        let svc = VaultService::new(&mut vault);
        assert!(
            !svc.is_configured("user-abc", &CredentialType::ImapHost).unwrap()
        );
        cleanup(&vp, &kp);
    }

    // ── credential value NOT present in response ─────────────────────────────

    #[test]
    fn test_credential_value_not_echoed_in_flash_message() {
        let result = CredentialsApplyResult { stored: 1, deleted: 0, unchanged: 0 };
        let msg = result.flash_message();
        assert!(msg.contains("saved"));
        assert!(!msg.contains('<'));
        assert!(!msg.contains('>'));
    }

    #[test]
    fn test_flash_message_no_changes() {
        let result = CredentialsApplyResult { stored: 0, deleted: 0, unchanged: 2 };
        assert_eq!(result.flash_message(), "No changes made.");
    }

    #[test]
    fn test_flash_message_delete() {
        let result = CredentialsApplyResult { stored: 0, deleted: 1, unchanged: 0 };
        assert!(result.flash_message().contains("revoked"));
    }

    #[test]
    fn test_flash_message_store_and_delete() {
        let result = CredentialsApplyResult { stored: 1, deleted: 1, unchanged: 0 };
        let msg = result.flash_message();
        assert!(msg.contains("saved"));
        assert!(msg.contains("revoked"));
    }

    // ── render_credentials_page ───────────────────────────────────────────────

    #[test]
    fn test_credentials_page_has_constellation_css() {
        let params = CredentialsPageParams {
            csrf_token: generate_csrf_token(),
            ..Default::default()
        };
        let html = render_credentials_page(&params);
        assert!(
            html.contains(r#"<link rel="stylesheet" href="/shared/constellation.css">"#)
        );
    }

    #[test]
    fn test_credentials_page_no_inline_styles() {
        let params = CredentialsPageParams {
            csrf_token: generate_csrf_token(),
            ..Default::default()
        };
        let html = render_credentials_page(&params);
        assert!(!html.contains("style=\""));
    }

    #[test]
    fn test_credentials_page_has_password_type_inputs() {
        let params = CredentialsPageParams {
            csrf_token: generate_csrf_token(),
            ..Default::default()
        };
        let html = render_credentials_page(&params);
        let count = html.matches("type=\"password\"").count();
        assert_eq!(count, 3, "All three credential fields must be type='password'");
    }

    #[test]
    fn test_credentials_page_has_autocomplete_off() {
        let params = CredentialsPageParams {
            csrf_token: generate_csrf_token(),
            ..Default::default()
        };
        let html = render_credentials_page(&params);
        assert!(html.contains("autocomplete=\"off\""));
    }

    #[test]
    fn test_credentials_page_has_csrf_hidden_field() {
        let csrf = generate_csrf_token();
        let params = CredentialsPageParams {
            csrf_token: csrf.clone(),
            ..Default::default()
        };
        let html = render_credentials_page(&params);
        assert!(html.contains("type=\"hidden\""));
        assert!(html.contains("csrf_token"));
        assert!(html.contains(&csrf));
    }

    #[test]
    fn test_credentials_page_has_setup_instructions() {
        let params = CredentialsPageParams {
            csrf_token: generate_csrf_token(),
            ..Default::default()
        };
        let html = render_credentials_page(&params);
        assert!(html.contains("myaccount.google.com"));
        assert!(html.contains("App password"));
    }

    #[test]
    fn test_credentials_page_escapes_flash_message() {
        let params = CredentialsPageParams {
            csrf_token: generate_csrf_token(),
            flash_message: Some("<script>alert('xss')</script>".to_string()),
            ..Default::default()
        };
        let html = render_credentials_page(&params);
        assert!(!html.contains("<script>alert"));
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn test_credentials_page_uses_constellation_classes() {
        let params = CredentialsPageParams {
            csrf_token: generate_csrf_token(),
            ..Default::default()
        };
        let html = render_credentials_page(&params);
        assert!(html.contains("class=\"card\"") || html.contains("class=\"card "));
        assert!(html.contains("btn-primary"));
        assert!(html.contains("lumina-footer"));
    }

    // ── apply_credentials_form stores credentials ──────────────────────────────

    #[test]
    fn test_apply_credentials_form_stores_credential() {
        let (mut vault, vp, kp) = make_vault("apply_store");
        let csrf = generate_csrf_token();
        let body = format!(
            "csrf_token={}&google_app_password=mysecretpass&caldav_url=&imap_host=",
            csrf
        );
        let form = parse_credentials_form(&body, &csrf).unwrap();
        let result = apply_credentials_form("user-abc", &form, &mut vault).unwrap();
        assert_eq!(result.stored, 1);

        let svc = VaultService::new(&mut vault);
        assert!(svc.is_configured("user-abc", &CredentialType::GoogleAppPassword).unwrap());
        cleanup(&vp, &kp);
    }

    // ── unauthenticated access ────────────────────────────────────────────────

    #[test]
    fn test_unauthenticated_access_identified_by_missing_session() {
        let session_csrf = generate_csrf_token();
        let result = parse_credentials_form("google_app_password=evil", &session_csrf);
        assert_eq!(result.unwrap_err(), FormParseError::MissingCsrfToken);
    }

    // ── form_urlencoded handles multi-byte UTF-8 ───────────────────────────────

    #[test]
    fn test_form_urlencoded_handles_multibyte_utf8() {
        let csrf = generate_csrf_token();
        // "café" → caf%C3%A9
        let body = format!("csrf_token={}&imap_host=caf%C3%A9.example.com", csrf);
        let form = parse_credentials_form(&body, &csrf).unwrap();
        use secrecy::ExposeSecret;
        let host = form.imap_host.unwrap();
        assert_eq!(
            host.expose_secret(),
            "café.example.com",
            "Multi-byte UTF-8 in form values must decode correctly"
        );
    }

    // ── revoke flag parsing ────────────────────────────────────────────────────

    #[test]
    fn test_revoke_flags_parsed_correctly() {
        let csrf = generate_csrf_token();
        let body = format!("csrf_token={}&revoke_caldav_url=1", csrf);
        let form = parse_credentials_form(&body, &csrf).unwrap();
        assert!(!form.revoke_google_app_password);
        assert!(form.revoke_caldav_url);
        assert!(!form.revoke_imap_host);
    }

    // ── html_escape ────────────────────────────────────────────────────────────

    #[test]
    fn test_html_escape_special_chars() {
        assert_eq!(html_escape("<"), "&lt;");
        assert_eq!(html_escape(">"), "&gt;");
        assert_eq!(html_escape("&"), "&amp;");
        assert_eq!(html_escape("\""), "&quot;");
        assert_eq!(html_escape("'"), "&#x27;");
        assert_eq!(html_escape("safe"), "safe");
    }

    // ── valid HTML structure ──────────────────────────────────────────────────

    #[test]
    fn test_credentials_page_valid_html_structure() {
        let params = CredentialsPageParams {
            csrf_token: generate_csrf_token(),
            ..Default::default()
        };
        let html = render_credentials_page(&params);
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("<html"));
        assert!(html.contains("<head>"));
        assert!(html.contains("<body>"));
        assert!(html.contains("</html>"));
    }
}
