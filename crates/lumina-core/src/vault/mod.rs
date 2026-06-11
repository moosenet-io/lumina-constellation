//! Lumina encrypted vault system
//!
//! Provides secure storage for secrets using AES-256-GCM encryption with
//! pluggable key providers (file, environment, interactive).

pub mod format;
pub mod key_provider;
pub mod wizard;

use crate::vault::format::{VaultHeader, VaultError, HEADER_LEN, NONCE_LEN};
use crate::vault::key_provider::{EnvKeyProvider, FileKeyProvider, InteractiveKeyProvider, KeyProvider};
use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::fmt;

/// Global vault store instance
static VAULT_STORE: OnceLock<VaultStore> = OnceLock::new();

/// Initialize the vault system - call once at startup
pub fn init() -> Result<(), VaultError> {
    let store = VaultStore::load()?;
    let _ = VAULT_STORE.set(store);
    Ok(())
}

/// Get the global vault store - panics if not initialized
pub fn manager() -> &'static VaultStore {
    VAULT_STORE.get_or_init(|| {
        VaultStore::load().unwrap_or_else(|_| VaultStore::empty())
    })
}

/// Non-panicking accessor - returns None if vault not initialized
pub fn manager_opt() -> Option<&'static VaultStore> {
    VAULT_STORE.get()
}

/// In-memory secret store with AES-256-GCM persistence
pub struct VaultStore {
    /// In-memory secrets
    secrets: HashMap<String, SecretString>,
    /// Path to the encrypted vault file
    vault_path: PathBuf,
    /// Key provider for encryption
    key_provider: Box<dyn KeyProvider>,
}

/// Serializable representation of vault contents
#[derive(Serialize, Deserialize)]
struct VaultData {
    secrets: HashMap<String, String>,
}

impl VaultStore {
    /// Create empty vault store
    pub fn empty() -> Self {
        Self {
            secrets: HashMap::new(),
            vault_path: Self::default_vault_path(),
            key_provider: Box::new(FileKeyProvider::from_env_or_default()),
        }
    }

    /// Load vault from disk or create empty if not exists
    pub fn load() -> Result<Self, VaultError> {
        let vault_path = Self::default_vault_path();
        let key_provider = Self::create_key_provider()?;

        let mut store = Self {
            secrets: HashMap::new(),
            vault_path,
            key_provider,
        };

        // Try to load existing vault
        if store.vault_path.exists() {
            store.decrypt_from_disk()?;
        }

        Ok(store)
    }

    /// Create vault with specific path and key provider
    pub fn with_config(vault_path: PathBuf, key_provider: Box<dyn KeyProvider>) -> Self {
        Self {
            secrets: HashMap::new(),
            vault_path,
            key_provider,
        }
    }

    /// Default vault file path: ~/.lumina/vault.enc
    pub fn default_vault_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(".lumina")
            .join("vault.enc")
    }

    /// Create appropriate key provider based on LUMINA_KEY_PROVIDER env var
    fn create_key_provider() -> Result<Box<dyn KeyProvider>, VaultError> {
        let provider_type = std::env::var("LUMINA_KEY_PROVIDER").unwrap_or_else(|_| "file".to_string());

        match provider_type.as_str() {
            "file" => Ok(Box::new(FileKeyProvider::from_env_or_default())),
            "env" => Ok(Box::new(EnvKeyProvider::new())),
            "interactive" => {
                let provider = InteractiveKeyProvider::from_prompt("Vault passphrase: ")?;
                Ok(Box::new(provider))
            }
            _ => Err(VaultError::KeyDerivation(
                format!("Unknown key provider type: {}", provider_type)
            )),
        }
    }

    /// Get a secret by key
    pub fn get(&self, key: &str) -> Option<&SecretString> {
        self.secrets.get(key)
    }

    /// Set a secret
    pub fn set(&mut self, key: String, value: SecretString) -> Result<(), VaultError> {
        self.secrets.insert(key, value);
        self.save()
    }

    /// Remove a secret
    pub fn remove(&mut self, key: &str) -> Result<bool, VaultError> {
        let removed = self.secrets.remove(key).is_some();
        self.save()?;
        Ok(removed)
    }

    /// List all secret keys (not values)
    pub fn list(&self) -> Vec<String> {
        self.secrets.keys().cloned().collect()
    }

    /// Check if vault contains a key
    pub fn contains_key(&self, key: &str) -> bool {
        self.secrets.contains_key(key)
    }

    /// Number of secrets in vault
    pub fn len(&self) -> usize {
        self.secrets.len()
    }

    /// Check if vault is empty
    pub fn is_empty(&self) -> bool {
        self.secrets.is_empty()
    }

    /// Save vault to disk with encryption
    fn save(&self) -> Result<(), VaultError> {
        // Create parent directory if it doesn't exist
        if let Some(parent) = self.vault_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Serialize secrets to JSON
        let vault_data = VaultData {
            secrets: self.secrets
                .iter()
                .map(|(k, v)| (k.clone(), v.expose_secret().to_string()))
                .collect(),
        };

        let plaintext = serde_json::to_vec(&vault_data)
            .map_err(|_e| VaultError::EncryptionFailed)?;

        // Encrypt and write
        let encrypted = self.encrypt(&plaintext)?;

        // Atomic write: write to temp file then rename
        let temp_path = self.vault_path.with_extension("enc.tmp");
        std::fs::write(&temp_path, encrypted)?;
        std::fs::rename(&temp_path, &self.vault_path)?;

        Ok(())
    }

    /// Load and decrypt vault from disk
    fn decrypt_from_disk(&mut self) -> Result<(), VaultError> {
        let encrypted_data = std::fs::read(&self.vault_path)?;
        let plaintext = self.decrypt(&encrypted_data)?;

        let vault_data: VaultData = serde_json::from_slice(&plaintext)
            .map_err(|_| VaultError::InvalidFormat("Invalid JSON in vault file".to_string()))?;

        // Convert strings back to SecretString
        self.secrets = vault_data.secrets
            .into_iter()
            .map(|(k, v)| (k, SecretString::new(v.into())))
            .collect();

        Ok(())
    }

    /// Encrypt plaintext using AES-256-GCM
    fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>, VaultError> {
        // Create header first (we need the salt for interactive providers)
        let header = if self.key_provider.name() == "interactive" {
            VaultHeader::new() // Random salt for Argon2
        } else {
            // We'll set the nonce later for non-interactive providers
            VaultHeader::new_no_salt([0u8; NONCE_LEN])
        };

        // Derive encryption key
        let key = match self.key_provider.name() {
            "interactive" => {
                self.key_provider.derive_key_with_salt(&header.salt)?
            }
            _ => self.key_provider.derive_key()?,
        };

        // Update nonce if not already set
        let final_header = if self.key_provider.name() == "interactive" {
            header // Already has random salt and nonce
        } else {
            // Generate random nonce for file/env providers
            let mut nonce_bytes = [0u8; NONCE_LEN];
            use rand::RngCore;
            rand::thread_rng().fill_bytes(&mut nonce_bytes);
            VaultHeader::new_no_salt(nonce_bytes)
        };

        // Initialize AES-GCM
        let cipher = Aes256Gcm::new_from_slice(&key)
            .map_err(|_| VaultError::EncryptionFailed)?;

        // Encrypt
        let nonce = Nonce::from_slice(&final_header.nonce);
        let ciphertext = cipher.encrypt(nonce, plaintext)
            .map_err(|_| VaultError::EncryptionFailed)?;

        // Build final format: header + ciphertext
        let mut result = Vec::with_capacity(HEADER_LEN + ciphertext.len());
        result.extend_from_slice(&final_header.to_bytes());
        result.extend_from_slice(&ciphertext);

        // Zero out key
        let mut key_copy = key;
        zeroize::Zeroize::zeroize(&mut key_copy);

        Ok(result)
    }

    /// Decrypt ciphertext using AES-256-GCM
    fn decrypt(&self, encrypted_data: &[u8]) -> Result<Vec<u8>, VaultError> {
        if encrypted_data.len() < HEADER_LEN {
            return Err(VaultError::InvalidFormat("Vault file too short".to_string()));
        }

        // Parse header
        let header = VaultHeader::from_bytes(&encrypted_data[..HEADER_LEN])?;
        let ciphertext = &encrypted_data[HEADER_LEN..];

        // Derive decryption key
        let key = match self.key_provider.name() {
            "interactive" => {
                self.key_provider.derive_key_with_salt(&header.salt)?
            }
            _ => self.key_provider.derive_key()?,
        };

        // Initialize AES-GCM
        let cipher = Aes256Gcm::new_from_slice(&key)
            .map_err(|_| VaultError::DecryptionFailed)?;

        // Decrypt
        let nonce = Nonce::from_slice(&header.nonce);
        let plaintext = cipher.decrypt(nonce, ciphertext)
            .map_err(|_| VaultError::DecryptionFailed)?;

        // Zero out key
        let mut key_copy = key;
        zeroize::Zeroize::zeroize(&mut key_copy);

        Ok(plaintext)
    }
}

impl fmt::Debug for VaultStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VaultStore")
            .field("secrets_count", &self.secrets.len())
            .field("vault_path", &self.vault_path)
            .field("key_provider_name", &self.key_provider.name())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_vault_store_empty() {
        let store = VaultStore::empty();
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
        assert!(!store.contains_key("test"));
    }

    #[test]
    fn test_vault_store_set_get() {
        let temp_dir = std::env::temp_dir();
        let vault_path = temp_dir.join("test_vault.enc");
        let key_path = temp_dir.join("test_key.key");

        // Clean up any existing files
        let _ = fs::remove_file(&vault_path);
        let _ = fs::remove_file(&key_path);

        // Generate key file
        FileKeyProvider::generate(&key_path).unwrap();
        let key_provider = Box::new(FileKeyProvider::new(key_path.clone()));

        let mut store = VaultStore::with_config(vault_path.clone(), key_provider);

        // Test set/get
        let secret = SecretString::new("test_value".to_string().into());
        store.set("test_key".to_string(), secret).unwrap();

        assert!(store.contains_key("test_key"));
        assert_eq!(store.len(), 1);

        let retrieved = store.get("test_key").unwrap();
        assert_eq!(retrieved.expose_secret(), "test_value");

        // Clean up
        let _ = fs::remove_file(&vault_path);
        let _ = fs::remove_file(&key_path);
    }

    #[test]
    fn test_vault_persistence() {
        let temp_dir = std::env::temp_dir();
        let vault_path = temp_dir.join("test_persist.enc");
        let key_path = temp_dir.join("test_persist.key");

        // Clean up any existing files
        let _ = fs::remove_file(&vault_path);
        let _ = fs::remove_file(&key_path);

        // Generate key file
        FileKeyProvider::generate(&key_path).unwrap();

        // Create store and save secret
        {
            let key_provider = Box::new(FileKeyProvider::new(key_path.clone()));
            let mut store = VaultStore::with_config(vault_path.clone(), key_provider);

            let secret = SecretString::new("persistent_value".to_string().into());
            store.set("persist_key".to_string(), secret).unwrap();
        }

        // Load store again and verify secret persisted
        {
            let key_provider = Box::new(FileKeyProvider::new(key_path.clone()));
            let store = VaultStore::with_config(vault_path.clone(), key_provider);

            let mut store_mut = store;
            store_mut.decrypt_from_disk().unwrap();

            assert!(store_mut.contains_key("persist_key"));
            let retrieved = store_mut.get("persist_key").unwrap();
            assert_eq!(retrieved.expose_secret(), "persistent_value");
        }

        // Clean up
        let _ = fs::remove_file(&vault_path);
        let _ = fs::remove_file(&key_path);
    }
}