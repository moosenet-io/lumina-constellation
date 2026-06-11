//! Key provider implementations for vault encryption

use crate::vault::format::{VaultError, ARGON2_SALT_LEN, KEY_LEN, MIN_PASSPHRASE_LEN};
use argon2::{Argon2, PasswordHasher};
use argon2::password_hash::SaltString;
use secrecy::{ExposeSecret, SecretString};
use std::path::PathBuf;

/// Trait for vault key providers. Each implementation returns a 32-byte key.
pub trait KeyProvider: Send + Sync {
    /// Derive the 32-byte encryption key
    fn derive_key(&self) -> Result<[u8; KEY_LEN], VaultError>;

    /// Derive key with explicit salt (for interactive provider)
    fn derive_key_with_salt(&self, _salt: &[u8; ARGON2_SALT_LEN]) -> Result<[u8; KEY_LEN], VaultError> {
        // Default implementation ignores salt and calls derive_key
        self.derive_key()
    }

    /// Name for logging (never contains secret values)
    fn name(&self) -> &str;
}

/// File-based key provider - reads raw 32-byte key from file
pub struct FileKeyProvider {
    pub path: PathBuf,
}

impl FileKeyProvider {
    /// Create new FileKeyProvider with specified path
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Default key file path: ~/.lumina/vault.key
    pub fn default_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(".lumina")
            .join("vault.key")
    }

    /// Create from LUMINA_VAULT_KEY_PATH env var or default path
    pub fn from_env_or_default() -> Self {
        let path = std::env::var("LUMINA_VAULT_KEY_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| Self::default_path());
        Self::new(path)
    }

    /// Generate a new random 32-byte key file with secure permissions (0600)
    pub fn generate(path: &PathBuf) -> Result<(), VaultError> {
        use rand::RngCore;

        let mut key = [0u8; KEY_LEN];
        rand::thread_rng().fill_bytes(&mut key);

        // Create parent directory if it doesn't exist
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Write key file
        std::fs::write(path, &key)?;

        // Set secure permissions (Unix only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }

        // Zero out the key in memory
        zeroize::Zeroize::zeroize(&mut key);

        Ok(())
    }

    /// Check if key file has secure permissions (Unix only)
    #[cfg(unix)]
    pub fn check_permissions(&self) -> bool {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::metadata(&self.path) {
            let mode = metadata.permissions().mode();
            // Check if group/other have any permissions (should be 0600)
            (mode & 0o077) == 0
        } else {
            false
        }
    }

    /// Always returns true on non-Unix systems
    #[cfg(not(unix))]
    pub fn check_permissions(&self) -> bool {
        true
    }
}

impl KeyProvider for FileKeyProvider {
    fn derive_key(&self) -> Result<[u8; KEY_LEN], VaultError> {
        // Warn about insecure permissions but don't fail
        if !self.check_permissions() {
            eprintln!("Warning: Vault key file {} has insecure permissions (should be 0600)",
                     self.path.display());
        }

        let data = std::fs::read(&self.path)?;

        if data.len() != KEY_LEN {
            return Err(VaultError::InvalidFormat(
                format!("Key file must be exactly {} bytes, got {}", KEY_LEN, data.len())
            ));
        }

        let mut key = [0u8; KEY_LEN];
        key.copy_from_slice(&data);
        Ok(key)
    }

    fn name(&self) -> &str {
        "file"
    }
}

/// Environment variable key provider - reads hex-encoded key from LUMINA_VAULT_KEY
pub struct EnvKeyProvider {
    var_name: String,
}

impl EnvKeyProvider {
    /// Create new EnvKeyProvider with default variable name
    pub fn new() -> Self {
        Self {
            var_name: "LUMINA_VAULT_KEY".to_string(),
        }
    }

    /// Create with custom environment variable name
    pub fn with_var_name(var_name: String) -> Self {
        Self { var_name }
    }
}

impl Default for EnvKeyProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyProvider for EnvKeyProvider {
    fn derive_key(&self) -> Result<[u8; KEY_LEN], VaultError> {
        let hex_key = std::env::var(&self.var_name)
            .map_err(|_| VaultError::KeyDerivation(
                format!("Environment variable {} not set", self.var_name)
            ))?;

        // Decode hex string
        let decoded = hex::decode(&hex_key)
            .map_err(|e| VaultError::KeyDerivation(
                format!("Invalid hex key in {}: {}", self.var_name, e)
            ))?;

        if decoded.len() != KEY_LEN {
            return Err(VaultError::KeyDerivation(
                format!("Key must be exactly {} bytes when hex-decoded, got {}", KEY_LEN, decoded.len())
            ));
        }

        let mut key = [0u8; KEY_LEN];
        key.copy_from_slice(&decoded);
        Ok(key)
    }

    fn name(&self) -> &str {
        "env"
    }
}

/// Interactive key provider - derives key from passphrase using Argon2
pub struct InteractiveKeyProvider {
    passphrase: SecretString,
}

impl InteractiveKeyProvider {
    /// Create new InteractiveKeyProvider with passphrase
    pub fn new(passphrase: SecretString) -> Result<Self, VaultError> {
        // Validate passphrase length
        if passphrase.expose_secret().len() < MIN_PASSPHRASE_LEN {
            return Err(VaultError::InvalidPassphrase(
                format!("Passphrase must be at least {} characters", MIN_PASSPHRASE_LEN)
            ));
        }

        Ok(Self { passphrase })
    }

    /// Create by prompting user for passphrase
    pub fn from_prompt(prompt: &str) -> Result<Self, VaultError> {
        // In a real implementation, this would use a proper password prompt library
        // For now, we'll read from stdin (NOTE: This is insecure for production!)
        eprint!("{}", prompt);

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)
            .map_err(|e| VaultError::KeyDerivation(format!("Failed to read passphrase: {}", e)))?;

        let passphrase = input.trim().to_string();
        let secret = SecretString::new(passphrase.into());

        Self::new(secret)
    }

}

impl KeyProvider for InteractiveKeyProvider {
    /// Note: This requires a salt from the vault header to derive the key
    /// This method will fail - use derive_key_with_salt instead
    fn derive_key(&self) -> Result<[u8; KEY_LEN], VaultError> {
        Err(VaultError::KeyDerivation(
            "Interactive key provider requires salt - use derive_key_with_salt()".to_string()
        ))
    }

    fn derive_key_with_salt(&self, salt: &[u8; ARGON2_SALT_LEN]) -> Result<[u8; KEY_LEN], VaultError> {
        // Use Argon2id with conservative parameters
        let argon2 = Argon2::default();

        // Convert salt to SaltString format expected by argon2
        let salt_string = SaltString::encode_b64(salt)
            .map_err(|e| VaultError::KeyDerivation(format!("Invalid salt: {}", e)))?;

        // Hash the passphrase with Argon2
        let password_hash = argon2
            .hash_password(self.passphrase.expose_secret().as_bytes(), &salt_string)
            .map_err(|e| VaultError::KeyDerivation(format!("Argon2 hashing failed: {}", e)))?;

        // Extract the raw hash bytes (first 32 bytes)
        let hash_bytes = password_hash.hash
            .ok_or_else(|| VaultError::KeyDerivation("No hash in password result".to_string()))?;

        if hash_bytes.as_bytes().len() < KEY_LEN {
            return Err(VaultError::KeyDerivation("Argon2 output too short".to_string()));
        }

        let mut key = [0u8; KEY_LEN];
        key.copy_from_slice(&hash_bytes.as_bytes()[..KEY_LEN]);
        Ok(key)
    }

    fn name(&self) -> &str {
        "interactive"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::fs;

    #[test]
    fn test_file_key_provider_generate_and_read() {
        let temp_dir = std::env::temp_dir();
        let key_path = temp_dir.join("test_vault.key");

        // Clean up any existing file
        let _ = fs::remove_file(&key_path);

        // Generate key
        FileKeyProvider::generate(&key_path).unwrap();

        // Read key back
        let provider = FileKeyProvider::new(key_path.clone());
        let key = provider.derive_key().unwrap();

        assert_eq!(key.len(), KEY_LEN);

        // Clean up
        let _ = fs::remove_file(&key_path);
    }

    #[test]
    #[serial]
    fn test_env_key_provider() {
        let hex_key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        std::env::set_var("TEST_VAULT_KEY", hex_key);

        let provider = EnvKeyProvider::with_var_name("TEST_VAULT_KEY".to_string());
        let key = provider.derive_key().unwrap();

        assert_eq!(key.len(), KEY_LEN);

        std::env::remove_var("TEST_VAULT_KEY");
    }

    #[test]
    fn test_interactive_key_provider() {
        let passphrase = SecretString::new("test_passphrase_123".to_string().into());
        let provider = InteractiveKeyProvider::new(passphrase).unwrap();

        assert_eq!(provider.name(), "interactive");

        // Should fail without salt
        assert!(provider.derive_key().is_err());

        // Should work with salt
        let salt = [1u8; ARGON2_SALT_LEN];
        let key = provider.derive_key_with_salt(&salt).unwrap();
        assert_eq!(key.len(), KEY_LEN);
    }

    #[test]
    fn test_passphrase_validation() {
        // Too short passphrase should fail
        let short_passphrase = SecretString::new("short".to_string().into());
        assert!(InteractiveKeyProvider::new(short_passphrase).is_err());

        // Valid passphrase should succeed
        let valid_passphrase = SecretString::new("valid_passphrase".to_string().into());
        assert!(InteractiveKeyProvider::new(valid_passphrase).is_ok());
    }
}