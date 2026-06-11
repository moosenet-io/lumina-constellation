//! Vault file format constants and utilities

/// Magic bytes for Lumina vault files: "LMVT"
pub const MAGIC: [u8; 4] = [b'L', b'M', b'V', b'T'];

/// Current vault format version
pub const VERSION: u8 = 1;

/// AES-256-GCM key length
pub const KEY_LEN: usize = 32;

/// AES-256-GCM nonce length
pub const NONCE_LEN: usize = 12;

/// Argon2 salt length for interactive key provider
pub const ARGON2_SALT_LEN: usize = 16;

/// Minimum passphrase length for security
pub const MIN_PASSPHRASE_LEN: usize = 8;

/// Header length: magic + version + salt + nonce
pub const HEADER_LEN: usize = MAGIC.len() + 1 + ARGON2_SALT_LEN + NONCE_LEN;

/// File format layout:
/// [magic: 4 bytes] [version: 1 byte] [salt: 16 bytes] [nonce: 12 bytes] [ciphertext: variable]
///
/// The salt is used for Argon2 key derivation in interactive mode.
/// For file and env providers, the salt is present but not used (filled with zeros).
pub struct VaultHeader {
    pub magic: [u8; 4],
    pub version: u8,
    pub salt: [u8; ARGON2_SALT_LEN],
    pub nonce: [u8; NONCE_LEN],
}

impl VaultHeader {
    /// Create a new header with random salt and nonce
    pub fn new() -> Self {
        let mut salt = [0u8; ARGON2_SALT_LEN];
        let mut nonce = [0u8; NONCE_LEN];

        use rand::RngCore;
        let mut rng = rand::thread_rng();
        rng.fill_bytes(&mut salt);
        rng.fill_bytes(&mut nonce);

        Self {
            magic: MAGIC,
            version: VERSION,
            salt,
            nonce,
        }
    }

    /// Create header with zero salt (for file/env providers)
    pub fn new_no_salt(nonce: [u8; NONCE_LEN]) -> Self {
        Self {
            magic: MAGIC,
            version: VERSION,
            salt: [0u8; ARGON2_SALT_LEN],
            nonce,
        }
    }

    /// Serialize header to bytes
    pub fn to_bytes(&self) -> [u8; HEADER_LEN] {
        let mut header = [0u8; HEADER_LEN];
        header[0..4].copy_from_slice(&self.magic);
        header[4] = self.version;
        header[5..21].copy_from_slice(&self.salt);
        header[21..33].copy_from_slice(&self.nonce);
        header
    }

    /// Deserialize header from bytes
    pub fn from_bytes(data: &[u8]) -> Result<Self, VaultError> {
        if data.len() < HEADER_LEN {
            return Err(VaultError::InvalidFormat("Header too short".to_string()));
        }

        let mut magic = [0u8; 4];
        magic.copy_from_slice(&data[0..4]);

        if magic != MAGIC {
            return Err(VaultError::InvalidFormat("Invalid magic bytes".to_string()));
        }

        let version = data[4];
        if version != VERSION {
            return Err(VaultError::UnsupportedVersion(version));
        }

        let mut salt = [0u8; ARGON2_SALT_LEN];
        salt.copy_from_slice(&data[5..21]);

        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(&data[21..33]);

        Ok(Self {
            magic,
            version,
            salt,
            nonce,
        })
    }
}

impl Default for VaultHeader {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("Invalid vault format: {0}")]
    InvalidFormat(String),

    #[error("Unsupported vault version: {0}")]
    UnsupportedVersion(u8),

    #[error("Decryption failed")]
    DecryptionFailed,

    #[error("Encryption failed")]
    EncryptionFailed,

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Key derivation failed: {0}")]
    KeyDerivation(String),

    #[error("Invalid passphrase: {0}")]
    InvalidPassphrase(String),
}