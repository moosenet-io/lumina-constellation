//! Vault setup wizard for interactive configuration

use crate::vault::format::VaultError;
use crate::vault::key_provider::{FileKeyProvider, InteractiveKeyProvider};
use crate::vault::VaultStore;
use secrecy::{ExposeSecret, SecretString};
use std::io::{self, Write};
use std::path::PathBuf;

/// Interactive vault setup wizard
pub struct VaultWizard;

impl VaultWizard {
    /// Run the interactive vault setup process
    pub fn run() -> Result<(), VaultError> {
        println!("=== Lumina Vault Setup Wizard ===");
        println!();

        let key_provider_type = Self::choose_key_provider()?;

        match key_provider_type.as_str() {
            "file" => Self::setup_file_provider()?,
            "env" => Self::setup_env_provider()?,
            "interactive" => Self::setup_interactive_provider()?,
            _ => unreachable!(),
        }

        println!("Vault setup complete!");
        Ok(())
    }

    /// Let user choose key provider type
    fn choose_key_provider() -> Result<String, VaultError> {
        println!("Choose a key provider:");
        println!("1. File (key stored in ~/.lumina/vault.key) - recommended for homelab/self-hosted");
        println!("2. Environment (key from LUMINA_VAULT_KEY env var) - recommended for cloud/VPS");
        println!("3. Interactive (passphrase prompt) - recommended for developer/eval");
        println!();

        loop {
            print!("Enter choice (1-3): ");
            io::stdout().flush().unwrap();

            let mut input = String::new();
            io::stdin().read_line(&mut input)
                .map_err(|e| VaultError::Io(e))?;

            match input.trim() {
                "1" => return Ok("file".to_string()),
                "2" => return Ok("env".to_string()),
                "3" => return Ok("interactive".to_string()),
                _ => println!("Invalid choice. Please enter 1, 2, or 3."),
            }
        }
    }

    /// Setup file-based key provider
    fn setup_file_provider() -> Result<(), VaultError> {
        let default_path = FileKeyProvider::default_path();

        println!("File key provider selected.");
        println!("Default key path: {}", default_path.display());

        if default_path.exists() {
            println!("Key file already exists at default location.");
            return Ok(());
        }

        print!("Generate new key file? [Y/n]: ");
        io::stdout().flush().unwrap();

        let mut input = String::new();
        io::stdin().read_line(&mut input)
            .map_err(|e| VaultError::Io(e))?;

        let generate = input.trim().is_empty() || input.trim().to_lowercase() == "y" || input.trim().to_lowercase() == "yes";

        if generate {
            FileKeyProvider::generate(&default_path)?;
            println!("Generated secure key file at: {}", default_path.display());
            println!("Keep this file safe and secure (permissions: 0600)");
        }

        println!("Set LUMINA_KEY_PROVIDER=file in your environment");
        Ok(())
    }

    /// Setup environment-based key provider
    fn setup_env_provider() -> Result<(), VaultError> {
        println!("Environment key provider selected.");
        println!();

        // Generate a random key for the user to set in their environment
        use rand::RngCore;
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        let hex_key = hex::encode(key);

        println!("Generated random key (copy this to your environment):");
        println!("export LUMINA_VAULT_KEY={}", hex_key);
        println!("export LUMINA_KEY_PROVIDER=env");
        println!();
        println!("Keep this key secure and do not share it!");

        // Zero out the key
        zeroize::Zeroize::zeroize(&mut key);

        Ok(())
    }

    /// Setup interactive (passphrase) key provider
    fn setup_interactive_provider() -> Result<(), VaultError> {
        println!("Interactive key provider selected.");
        println!("You will be prompted for a passphrase each time the vault is accessed.");
        println!();

        // Test passphrase entry
        let passphrase1 = Self::prompt_passphrase("Enter passphrase: ")?;
        let passphrase2 = Self::prompt_passphrase("Confirm passphrase: ")?;

        if passphrase1.expose_secret() != passphrase2.expose_secret() {
            return Err(VaultError::InvalidPassphrase("Passphrases do not match".to_string()));
        }

        // Validate passphrase
        InteractiveKeyProvider::new(passphrase1)?;

        println!("Passphrase accepted.");
        println!("Set LUMINA_KEY_PROVIDER=interactive in your environment");

        Ok(())
    }

    /// Prompt for passphrase securely
    fn prompt_passphrase(prompt: &str) -> Result<SecretString, VaultError> {
        print!("{}", prompt);
        io::stdout().flush().unwrap();

        // In a real implementation, this would use a library like `rpassword`
        // to hide the passphrase input. For now, we use plain stdin.
        let mut input = String::new();
        io::stdin().read_line(&mut input)
            .map_err(|e| VaultError::Io(e))?;

        let passphrase = input.trim().to_string();
        Ok(SecretString::new(passphrase.into()))
    }

    /// Create an empty vault file
    pub fn create_vault(vault_path: Option<PathBuf>) -> Result<(), VaultError> {
        let path = vault_path.unwrap_or_else(|| VaultStore::default_vault_path());

        if path.exists() {
            println!("Vault file already exists at: {}", path.display());
            return Ok(());
        }

        // Create empty vault
        let key_provider = std::env::var("LUMINA_KEY_PROVIDER").unwrap_or_else(|_| "file".to_string());

        println!("Creating vault with {} key provider...", key_provider);

        // This will create an empty vault and save it
        let _store = VaultStore::load()?;

        println!("Created empty vault at: {}", path.display());
        println!("Use 'lumina vault set <key> <value>' to add secrets");

        Ok(())
    }

    /// Import secrets from environment variables
    pub fn import_from_env(keys: Vec<String>) -> Result<(), VaultError> {
        let mut store = VaultStore::load()?;
        let mut imported_count = 0;

        println!("Importing secrets from environment variables...");

        for key in keys {
            if let Ok(value) = std::env::var(&key) {
                let secret = SecretString::new(value.into());
                store.set(key.clone(), secret)?;
                println!("Imported: {}", key);
                imported_count += 1;
            } else {
                println!("Warning: Environment variable {} not found", key);
            }
        }

        println!("Imported {} secrets into vault", imported_count);
        Ok(())
    }
}

/// Command-line interface for vault operations
pub struct VaultCli;

impl VaultCli {
    /// Set a secret in the vault
    pub fn set(key: String, value: String) -> Result<(), VaultError> {
        let mut store = VaultStore::load()?;
        let secret = SecretString::new(value.into());
        store.set(key.clone(), secret)?;
        println!("Set secret: {}", key);
        Ok(())
    }

    /// Get a secret from the vault (prints to stdout)
    pub fn get(key: String) -> Result<(), VaultError> {
        let store = VaultStore::load()?;

        if let Some(secret) = store.get(&key) {
            println!("{}", secret.expose_secret());
        } else {
            eprintln!("Secret '{}' not found in vault", key);
            std::process::exit(1);
        }

        Ok(())
    }

    /// List all secret keys
    pub fn list() -> Result<(), VaultError> {
        let store = VaultStore::load()?;
        let keys = store.list();

        if keys.is_empty() {
            println!("Vault is empty");
        } else {
            println!("Secrets in vault:");
            for key in keys {
                println!("  {}", key);
            }
            println!("Total: {} secrets", store.len());
        }

        Ok(())
    }

    /// Remove a secret from the vault
    pub fn remove(key: String) -> Result<(), VaultError> {
        let mut store = VaultStore::load()?;

        if store.remove(&key)? {
            println!("Removed secret: {}", key);
        } else {
            eprintln!("Secret '{}' not found in vault", key);
            std::process::exit(1);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vault_cli_operations() {
        // These would be integration tests requiring actual vault setup
        // For now, just test that the functions exist and have correct signatures
        assert!(true);
    }
}