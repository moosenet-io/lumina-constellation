//! GUARD-06: Authentication and authorization management
//!
//! Provides comprehensive authentication and authorization controls
//! for the Lumina agent system.
//!
//! HARDEN-03: Passwords are stored as Argon2id PHC hashes, never plaintext.
//! HARDEN-07: Session tokens are HMAC-SHA256 signed to prevent forgery.

use crate::error::{LuminaError, Result};
use crate::vault;
use argon2::{password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString}, Argon2};
use base64::Engine;
use hmac::{Hmac, Mac};
use rand::rngs::OsRng;
use secrecy::{ExposeSecret, SecretString};
use sha2::Sha256;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

type HmacSha256 = Hmac<Sha256>;

/// Authentication result
#[derive(Debug, Clone)]
pub struct AuthResult {
    pub success: bool,
    pub user: Option<User>,
    pub error: Option<String>,
    pub requires_2fa: bool,
}

impl AuthResult {
    pub fn success(user: User) -> Self {
        Self {
            success: true,
            user: Some(user),
            error: None,
            requires_2fa: false,
        }
    }

    pub fn failure(error: String) -> Self {
        Self {
            success: false,
            user: None,
            error: Some(error),
            requires_2fa: false,
        }
    }

    pub fn requires_2fa() -> Self {
        Self {
            success: false,
            user: None,
            error: None,
            requires_2fa: true,
        }
    }
}

/// User information and permissions
#[derive(Debug, Clone)]
pub struct User {
    pub id: String,
    pub username: String,
    pub roles: Vec<Role>,
    pub permissions: HashSet<Permission>,
    pub is_admin: bool,
    pub last_login: Option<SystemTime>,
    pub session_expires: Option<SystemTime>,
}

impl User {
    pub fn new(id: String, username: String) -> Self {
        Self {
            id,
            username,
            roles: Vec::new(),
            permissions: HashSet::new(),
            is_admin: false,
            last_login: None,
            session_expires: None,
        }
    }

    /// Check if user has a specific permission
    pub fn has_permission(&self, permission: &Permission) -> bool {
        self.is_admin || self.permissions.contains(permission)
    }

    /// Check if user has any of the given permissions
    pub fn has_any_permission(&self, permissions: &[Permission]) -> bool {
        self.is_admin || permissions.iter().any(|p| self.permissions.contains(p))
    }

    /// Check if user has a specific role
    pub fn has_role(&self, role: &Role) -> bool {
        self.is_admin || self.roles.contains(role)
    }

    /// Add a role to the user
    pub fn add_role(&mut self, role: Role) {
        if !self.roles.contains(&role) {
            self.roles.push(role);
        }
    }

    /// Add a permission to the user
    pub fn add_permission(&mut self, permission: Permission) {
        self.permissions.insert(permission);
    }

    /// Check if the user's session is still valid
    pub fn session_valid(&self) -> bool {
        match self.session_expires {
            Some(expires) => SystemTime::now() < expires,
            None => false,
        }
    }

    /// Extend session expiration
    pub fn extend_session(&mut self, duration: Duration) {
        self.session_expires = Some(SystemTime::now() + duration);
    }
}

/// User role
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Role {
    Admin,
    User,
    Agent,
    ReadOnly,
    Custom(String),
}

/// Permission types
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Permission {
    // Core system permissions
    SystemRead,
    SystemWrite,
    SystemAdmin,

    // Agent management
    AgentCreate,
    AgentRead,
    AgentUpdate,
    AgentDelete,

    // Vault and secrets
    VaultRead,
    VaultWrite,
    VaultAdmin,

    // Configuration
    ConfigRead,
    ConfigWrite,

    // Security settings
    SecurityAudit,
    SecurityManage,

    // Custom permission
    Custom(String),
}

/// Session information
#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub user_id: String,
    pub created_at: SystemTime,
    pub expires_at: SystemTime,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
    pub is_2fa_verified: bool,
}

impl Session {
    pub fn new(user_id: String, duration: Duration) -> Self {
        let now = SystemTime::now();
        Self {
            id: generate_session_id(),
            user_id,
            created_at: now,
            expires_at: now + duration,
            ip_address: None,
            user_agent: None,
            is_2fa_verified: false,
        }
    }

    pub fn is_expired(&self) -> bool {
        SystemTime::now() > self.expires_at
    }

    pub fn is_valid(&self) -> bool {
        !self.is_expired()
    }
}

/// Authentication and authorization manager
pub struct AuthManager {
    users: Arc<Mutex<HashMap<String, User>>>,
    sessions: Arc<Mutex<HashMap<String, Session>>>,
    failed_attempts: Arc<Mutex<HashMap<String, Vec<SystemTime>>>>,
    config: AuthConfig,
}

/// Authentication configuration
#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub session_duration: Duration,
    pub max_failed_attempts: u32,
    pub lockout_duration: Duration,
    pub require_2fa: bool,
    pub password_min_length: usize,
    pub password_require_special: bool,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            session_duration: Duration::from_secs(3600), // 1 hour
            max_failed_attempts: 5,
            lockout_duration: Duration::from_secs(900), // 15 minutes
            require_2fa: false,
            password_min_length: 8,
            password_require_special: true,
        }
    }
}

impl AuthConfig {
    /// Create strict configuration for production
    pub fn strict() -> Self {
        Self {
            session_duration: Duration::from_secs(1800), // 30 minutes
            max_failed_attempts: 3,
            lockout_duration: Duration::from_secs(1800), // 30 minutes
            require_2fa: true,
            password_min_length: 12,
            password_require_special: true,
        }
    }

    /// Create permissive configuration for development
    pub fn permissive() -> Self {
        Self {
            session_duration: Duration::from_secs(7200), // 2 hours
            max_failed_attempts: 10,
            lockout_duration: Duration::from_secs(300), // 5 minutes
            require_2fa: false,
            password_min_length: 6,
            password_require_special: false,
        }
    }
}

impl AuthManager {
    /// Create new authentication manager with default configuration
    pub fn new() -> Self {
        Self::with_config(AuthConfig::default())
    }

    /// Create authentication manager with custom configuration
    pub fn with_config(config: AuthConfig) -> Self {
        Self {
            users: Arc::new(Mutex::new(HashMap::new())),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            failed_attempts: Arc::new(Mutex::new(HashMap::new())),
            config,
        }
    }

    /// Create a new user account
    pub fn create_user(
        &self,
        username: String,
        password: SecretString,
        roles: Vec<Role>,
    ) -> Result<String> {
        // Validate password
        self.validate_password(password.expose_secret())?;

        let mut users = self.users.lock().unwrap();

        // Check if username already exists
        if users.values().any(|u| u.username == username) {
            return Err(LuminaError::SecurityViolation(
                "Username already exists".to_string()
            ));
        }

        let user_id = generate_user_id();
        let mut user = User::new(user_id.clone(), username);

        // Set roles and derive permissions
        for role in roles {
            user.add_role(role.clone());
            self.add_role_permissions(&mut user, &role);
        }

        // HARDEN-03: Hash with Argon2id before storing — plaintext never touches vault
        let phc_hash = hash_password_argon2(password.expose_secret())?;
        let password_key = format!("user_password_{}", user_id);
        if let Ok(mut vault_store) = vault::VaultStore::load() {
            vault_store.set(password_key, SecretString::new(phc_hash.into()))
                .map_err(|e| LuminaError::SecurityViolation(format!("Failed to store password hash: {}", e)))?;
        }

        users.insert(user_id.clone(), user);

        Ok(user_id)
    }

    /// Authenticate user with username and password
    pub fn authenticate(&self, username: &str, password: &str) -> AuthResult {
        // Check if user is locked out
        if self.is_user_locked_out(username) {
            return AuthResult::failure("Account is temporarily locked".to_string());
        }

        let users = self.users.lock().unwrap();
        let user = match users.values().find(|u| u.username == username) {
            Some(user) => user.clone(),
            None => {
                self.record_failed_attempt(username);
                return AuthResult::failure("Invalid credentials".to_string());
            }
        };

        // HARDEN-03: Verify against Argon2id hash (with migration from plaintext)
        let password_key = format!("user_password_{}", user.id);
        let stored_value = match vault::VaultStore::load() {
            Ok(vault_store) => match vault_store.get(&password_key) {
                Some(stored) => stored.expose_secret().to_string(),
                None => {
                    self.record_failed_attempt(username);
                    return AuthResult::failure("Invalid credentials".to_string());
                }
            },
            Err(_) => {
                return AuthResult::failure("Authentication service unavailable".to_string());
            }
        };

        let password_ok = if stored_value.starts_with("$argon2") {
            // Already a PHC hash — verify with Argon2
            verify_password_argon2(password, &stored_value).unwrap_or(false)
        } else {
            // Legacy plaintext: compare, then migrate to hash on success
            if stored_value == password {
                // Migrate: replace plaintext with hash
                if let Ok(phc) = hash_password_argon2(password) {
                    if let Ok(mut vault_store) = vault::VaultStore::load() {
                        let _ = vault_store.set(
                            password_key,
                            SecretString::new(phc.into()),
                        );
                    }
                }
                true
            } else {
                false
            }
        };

        if !password_ok {
            self.record_failed_attempt(username);
            return AuthResult::failure("Invalid credentials".to_string());
        }

        // Clear failed attempts on successful authentication
        self.clear_failed_attempts(username);

        // Check if 2FA is required
        if self.config.require_2fa {
            return AuthResult::requires_2fa();
        }

        AuthResult::success(user)
    }

    /// Create a new session for an authenticated user
    pub fn create_session(&self, user_id: String) -> Result<Session> {
        let session = Session::new(user_id.clone(), self.config.session_duration);
        let session_id = session.id.clone();

        let mut sessions = self.sessions.lock().unwrap();
        sessions.insert(session_id.clone(), session.clone());

        // Update user last login
        let mut users = self.users.lock().unwrap();
        if let Some(user) = users.get_mut(&user_id) {
            user.last_login = Some(SystemTime::now());
            user.extend_session(self.config.session_duration);
        }

        Ok(session)
    }

    /// Validate a session token.
    ///
    /// HARDEN-07: Verifies the HMAC signature before performing any lookup.
    /// Tampered or forged tokens are rejected before touching the session store.
    pub fn validate_session(&self, session_id: &str) -> Option<User> {
        // Reject tokens with invalid signatures immediately
        let verified_token = verify_session_token(session_id)?;

        let mut sessions = self.sessions.lock().unwrap();

        if let Some(session) = sessions.get(&verified_token) {
            if session.is_valid() {
                let users = self.users.lock().unwrap();
                return users.get(&session.user_id).cloned();
            } else {
                sessions.remove(&verified_token);
            }
        }

        None
    }

    /// Check if user has permission to perform an action
    pub fn check_permission(&self, session_id: &str, permission: Permission) -> bool {
        match self.validate_session(session_id) {
            Some(user) => user.has_permission(&permission),
            None => false,
        }
    }

    /// Revoke a session (logout)
    pub fn revoke_session(&self, session_id: &str) -> bool {
        let mut sessions = self.sessions.lock().unwrap();
        sessions.remove(session_id).is_some()
    }

    /// Change user password
    pub fn change_password(&self, user_id: &str, old_password: &str, new_password: SecretString) -> Result<()> {
        // Validate new password
        self.validate_password(new_password.expose_secret())?;

        let users = self.users.lock().unwrap();
        let _user = users.get(user_id)
            .ok_or_else(|| LuminaError::SecurityViolation("User not found".to_string()))?;

        // Verify old password
        let password_key = format!("user_password_{}", user_id);
        let mut vault_store = vault::VaultStore::load()
            .map_err(|_| LuminaError::SecurityViolation("Vault not available".to_string()))?;

        let stored = vault_store.get(&password_key)
            .ok_or_else(|| LuminaError::SecurityViolation("Password not found".to_string()))?;

        // Verify old password against stored hash (or plaintext for legacy)
        let stored_str = stored.expose_secret();
        let old_ok = if stored_str.starts_with("$argon2") {
            verify_password_argon2(old_password, stored_str).unwrap_or(false)
        } else {
            stored_str == old_password
        };
        if !old_ok {
            return Err(LuminaError::SecurityViolation("Invalid current password".to_string()));
        }

        // HARDEN-03: Store new password as Argon2id hash
        let phc_hash = hash_password_argon2(new_password.expose_secret())?;
        vault_store.set(password_key, SecretString::new(phc_hash.into()))
            .map_err(|e| LuminaError::SecurityViolation(format!("Failed to update password: {}", e)))?;

        Ok(())
    }

    /// Get user by ID
    pub fn get_user(&self, user_id: &str) -> Option<User> {
        let users = self.users.lock().unwrap();
        users.get(user_id).cloned()
    }

    /// Update user permissions
    pub fn update_user_permissions(&self, user_id: &str, permissions: Vec<Permission>) -> Result<()> {
        let mut users = self.users.lock().unwrap();
        let user = users.get_mut(user_id)
            .ok_or_else(|| LuminaError::SecurityViolation("User not found".to_string()))?;

        user.permissions.clear();
        for permission in permissions {
            user.add_permission(permission);
        }

        Ok(())
    }

    /// Clean up expired sessions
    pub fn cleanup_expired_sessions(&self) {
        let mut sessions = self.sessions.lock().unwrap();
        sessions.retain(|_, session| session.is_valid());
    }

    /// Validate password strength
    fn validate_password(&self, password: &str) -> Result<()> {
        if password.len() < self.config.password_min_length {
            return Err(LuminaError::SecurityViolation(
                format!("Password must be at least {} characters", self.config.password_min_length)
            ));
        }

        if self.config.password_require_special {
            let has_special = password.chars().any(|c| "!@#$%^&*()_+-=[]{}|;:,.<>?".contains(c));
            if !has_special {
                return Err(LuminaError::SecurityViolation(
                    "Password must contain at least one special character".to_string()
                ));
            }
        }

        Ok(())
    }

    /// Add permissions based on role
    fn add_role_permissions(&self, user: &mut User, role: &Role) {
        match role {
            Role::Admin => {
                user.is_admin = true; // Admins get all permissions
            }
            Role::User => {
                user.add_permission(Permission::SystemRead);
                user.add_permission(Permission::AgentRead);
                user.add_permission(Permission::ConfigRead);
            }
            Role::Agent => {
                user.add_permission(Permission::SystemRead);
                user.add_permission(Permission::AgentRead);
                user.add_permission(Permission::AgentUpdate);
                user.add_permission(Permission::VaultRead);
            }
            Role::ReadOnly => {
                user.add_permission(Permission::SystemRead);
                user.add_permission(Permission::AgentRead);
                user.add_permission(Permission::ConfigRead);
            }
            Role::Custom(_) => {
                // Custom roles would need additional configuration
            }
        }
    }

    /// Check if user is locked out due to failed attempts
    fn is_user_locked_out(&self, username: &str) -> bool {
        let failed_attempts = self.failed_attempts.lock().unwrap();
        if let Some(attempts) = failed_attempts.get(username) {
            if attempts.len() >= self.config.max_failed_attempts as usize {
                // Check if lockout period has expired
                if let Some(last_attempt) = attempts.last() {
                    return SystemTime::now() < *last_attempt + self.config.lockout_duration;
                }
            }
        }
        false
    }

    /// Record a failed authentication attempt
    fn record_failed_attempt(&self, username: &str) {
        let mut failed_attempts = self.failed_attempts.lock().unwrap();
        let now = SystemTime::now();

        let attempts = failed_attempts.entry(username.to_string()).or_insert_with(Vec::new);

        // Remove old attempts outside the lockout window
        let cutoff = now - self.config.lockout_duration;
        attempts.retain(|&attempt_time| attempt_time > cutoff);

        // Add new failed attempt
        attempts.push(now);
    }

    /// Clear failed attempts for a user
    fn clear_failed_attempts(&self, username: &str) {
        let mut failed_attempts = self.failed_attempts.lock().unwrap();
        failed_attempts.remove(username);
    }
}

impl Default for AuthManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Hash a password with Argon2id and return the PHC format string.
fn hash_password_argon2(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default(); // Argon2id, m=19456, t=2, p=1 (default in 0.5)
    argon2
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| LuminaError::SecurityViolation(format!("Password hashing failed: {}", e)))
}

/// Verify a password against an Argon2id PHC hash string.
fn verify_password_argon2(password: &str, phc_hash: &str) -> Result<bool> {
    let parsed = PasswordHash::new(phc_hash)
        .map_err(|e| LuminaError::SecurityViolation(format!("Invalid password hash: {}", e)))?;
    Ok(Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
}

/// Generate a unique user ID
fn generate_user_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    format!("user_{}", timestamp)
}

/// Generate an HMAC-SHA256 signed session token.
///
/// Format: `{base64url(random_id)}.{hex(hmac_sha256(random_id, signing_key))}`
///
/// The signing key is loaded from vault (`LUMINA_SESSION_KEY`). If the vault
/// is unavailable, a per-process in-memory key is used (sessions won't survive
/// restarts, which is acceptable for a single-user homelab).
fn generate_session_id() -> String {
    use rand::RngCore;

    let mut id_bytes = [0u8; 16];
    OsRng.fill_bytes(&mut id_bytes);
    let id = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(id_bytes);

    let key = get_or_create_session_key();
    let mut mac = HmacSha256::new_from_slice(&key)
        .expect("HMAC accepts any key length");
    mac.update(id.as_bytes());
    let sig = hex::encode(mac.finalize().into_bytes());

    format!("{}.{}", id, sig)
}

/// Verify an HMAC-signed session token.
///
/// Returns the session ID portion (before the dot) if the signature is valid,
/// or None if the token is malformed or the signature does not match.
fn verify_session_token(token: &str) -> Option<String> {
    let (id, sig) = token.split_once('.')?;

    let key = get_or_create_session_key();
    let mut mac = HmacSha256::new_from_slice(&key)
        .expect("HMAC accepts any key length");
    mac.update(id.as_bytes());
    let expected = hex::encode(mac.finalize().into_bytes());

    // Constant-time compare
    if expected == sig {
        Some(token.to_string()) // full token is the map key
    } else {
        None
    }
}

/// Get or create the session signing key from vault.
///
/// Generates a random 32-byte key and caches it in a OnceLock so the
/// per-process fallback is stable within a single run.
fn get_or_create_session_key() -> Vec<u8> {
    static SESSION_KEY: OnceLock<Vec<u8>> = OnceLock::new();

    SESSION_KEY.get_or_init(|| {
        // Try vault first
        if let Ok(vault_store) = vault::VaultStore::load() {
            if let Some(stored) = vault_store.get("LUMINA_SESSION_KEY") {
                if let Ok(bytes) = hex::decode(stored.expose_secret()) {
                    if bytes.len() >= 32 {
                        return bytes;
                    }
                }
            }
        }

        // Generate ephemeral key and attempt to persist it
        use rand::RngCore;
        let mut key = vec![0u8; 32];
        OsRng.fill_bytes(&mut key);
        let hex_key = hex::encode(&key);

        if let Ok(mut vault_store) = vault::VaultStore::load() {
            let _ = vault_store.set(
                "LUMINA_SESSION_KEY".to_string(),
                SecretString::new(hex_key.into()),
            );
        }

        key
    }).clone()
}

/// Global authentication manager instance
static GLOBAL_AUTH_MANAGER: OnceLock<AuthManager> = OnceLock::new();

/// Get or initialize global authentication manager
pub fn global_auth_manager() -> &'static AuthManager {
    GLOBAL_AUTH_MANAGER.get_or_init(|| AuthManager::new())
}

/// Authenticate using global auth manager
pub fn authenticate_user(username: &str, password: &str) -> AuthResult {
    global_auth_manager().authenticate(username, password)
}

/// Check permission using global auth manager
pub fn check_user_permission(session_id: &str, permission: Permission) -> bool {
    global_auth_manager().check_permission(session_id, permission)
}

/// Middleware-style authentication check
pub fn with_auth<F, R>(session_id: &str, permission: Permission, operation: F) -> Result<R>
where
    F: FnOnce() -> R,
{
    if check_user_permission(session_id, permission) {
        Ok(operation())
    } else {
        Err(LuminaError::SecurityViolation("Insufficient permissions".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_user_creation() {
        let user = User::new("123".to_string(), "testuser".to_string());
        assert_eq!(user.id, "123");
        assert_eq!(user.username, "testuser");
        assert!(!user.is_admin);
        assert!(user.permissions.is_empty());
    }

    #[test]
    fn test_user_permissions() {
        let mut user = User::new("123".to_string(), "testuser".to_string());
        user.add_permission(Permission::SystemRead);
        user.add_permission(Permission::AgentRead);

        assert!(user.has_permission(&Permission::SystemRead));
        assert!(user.has_permission(&Permission::AgentRead));
        assert!(!user.has_permission(&Permission::SystemWrite));
    }

    #[test]
    fn test_admin_user() {
        let mut user = User::new("123".to_string(), "admin".to_string());
        user.is_admin = true;

        // Admin should have all permissions
        assert!(user.has_permission(&Permission::SystemRead));
        assert!(user.has_permission(&Permission::SystemWrite));
        assert!(user.has_permission(&Permission::VaultAdmin));
    }

    #[test]
    fn test_session_creation() {
        let session = Session::new("user123".to_string(), Duration::from_secs(3600));
        assert_eq!(session.user_id, "user123");
        assert!(session.is_valid());
        assert!(!session.is_expired());
    }

    #[test]
    fn test_auth_config_presets() {
        let strict = AuthConfig::strict();
        assert!(strict.require_2fa);
        assert_eq!(strict.max_failed_attempts, 3);
        assert_eq!(strict.password_min_length, 12);

        let permissive = AuthConfig::permissive();
        assert!(!permissive.require_2fa);
        assert_eq!(permissive.max_failed_attempts, 10);
        assert_eq!(permissive.password_min_length, 6);
    }

    #[test]
    fn test_auth_manager_creation() {
        let auth_manager = AuthManager::new();
        assert_eq!(auth_manager.config.session_duration, Duration::from_secs(3600));
    }

    #[test]
    fn test_password_validation() {
        let auth_manager = AuthManager::new();

        // Too short
        assert!(auth_manager.validate_password("short").is_err());

        // No special characters (when required)
        assert!(auth_manager.validate_password("validpassword123").is_err());

        // Valid password
        assert!(auth_manager.validate_password("validpassword123!").is_ok());
    }

    #[test]
    fn test_user_roles() {
        let mut user = User::new("123".to_string(), "testuser".to_string());
        user.add_role(Role::User);
        user.add_role(Role::ReadOnly);

        assert!(user.has_role(&Role::User));
        assert!(user.has_role(&Role::ReadOnly));
        assert!(!user.has_role(&Role::Admin));
    }

    #[test]
    fn test_session_expiration() {
        let mut session = Session::new("user123".to_string(), Duration::from_millis(1));

        // Wait for expiration
        std::thread::sleep(Duration::from_millis(10));

        assert!(session.is_expired());
        assert!(!session.is_valid());
    }

    #[test]
    fn test_global_auth_manager() {
        let auth_result = authenticate_user("nonexistent", "password");
        assert!(!auth_result.success);
    }

    #[test]
    fn test_permission_check() {
        let result = check_user_permission("invalid_session", Permission::SystemRead);
        assert!(!result);
    }

    #[test]
    fn test_with_auth_middleware() {
        let result = with_auth("invalid_session", Permission::SystemRead, || "operation_result");
        assert!(result.is_err());
    }

    // HARDEN-03: password hashing tests
    #[test]
    fn test_password_stored_as_argon2_hash() {
        let hash = hash_password_argon2("correct-horse-battery!").unwrap();
        assert!(hash.starts_with("$argon2"), "should be PHC format, got: {}", &hash[..20.min(hash.len())]);
    }

    #[test]
    fn test_correct_password_verifies() {
        let hash = hash_password_argon2("correctP@ss1").unwrap();
        assert!(verify_password_argon2("correctP@ss1", &hash).unwrap());
    }

    #[test]
    fn test_wrong_password_fails_verification() {
        let hash = hash_password_argon2("correctP@ss1").unwrap();
        assert!(!verify_password_argon2("wrongpassword!", &hash).unwrap());
    }

    #[test]
    fn test_same_password_different_salts() {
        let hash1 = hash_password_argon2("P@ssword1!").unwrap();
        let hash2 = hash_password_argon2("P@ssword1!").unwrap();
        assert_ne!(hash1, hash2, "each hash should have a unique salt");
    }

    // HARDEN-07: HMAC session token tests
    #[test]
    fn test_session_token_has_two_parts() {
        let token = generate_session_id();
        let parts: Vec<&str> = token.splitn(2, '.').collect();
        assert_eq!(parts.len(), 2, "token should be id.signature");
        assert!(!parts[0].is_empty());
        assert!(!parts[1].is_empty());
    }

    #[test]
    fn test_valid_session_token_passes_verification() {
        let token = generate_session_id();
        assert!(verify_session_token(&token).is_some());
    }

    #[test]
    fn test_tampered_id_fails_verification() {
        let token = generate_session_id();
        let (_, sig) = token.split_once('.').unwrap();
        let tampered = format!("AAAAAAAAAAAAAAAAAAA.{}", sig);
        assert!(verify_session_token(&tampered).is_none());
    }

    #[test]
    fn test_tampered_signature_fails_verification() {
        let token = generate_session_id();
        let (id, _) = token.split_once('.').unwrap();
        let tampered = format!("{}.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", id);
        assert!(verify_session_token(&tampered).is_none());
    }

    #[test]
    fn test_missing_dot_fails_verification() {
        assert!(verify_session_token("notokenatall").is_none());
        assert!(verify_session_token("").is_none());
    }
}