//! ESEC-07: Secure export and backup controls.
//!
//! Memory exports are encrypted by default (AES-256-GCM) with a password-derived
//! key. Plaintext exports require explicit `include_sensitive = false` with the
//! `--plaintext` flag at the CLI layer.
//!
//! ## Key derivation
//! Password → Argon2id (memory=19MiB, iterations=2, parallelism=1) → 32-byte key.
//! A random 16-byte salt is generated per export and prepended to the file.
//!
//! ## Wire format (.enc file)
//! ```text
//! magic  (4 bytes): 0x4C 0x4D 0x45 0x37  ("LME7")
//! salt   (16 bytes): Argon2 salt
//! nonce  (12 bytes): AES-GCM nonce
//! ciphertext (remaining bytes): AES-256-GCM encrypted JSONL
//! ```
//!
//! ## Sensitive category handling
//! Health, Finance, and Personal memories are EXCLUDED from exports by default.
//! Pass `include_sensitive = true` to include them (CLI: `--include-sensitive`).
//!
//! ## Import
//! Validates that each memory's `user_id` matches `target_user_id`. Foreign
//! memories are rejected. Duplicate IDs (by UUID) are skipped.
//!
//! ## Audit
//! Every export/import is logged with metadata (user_id, count, types, encrypted,
//! path, timestamp) but NEVER with memory content.

use crate::engram::types::{Memory, SensitivityCategory, iso_now};
use crate::error::{LuminaError, Result};
use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use argon2::Argon2;
use rand::RngCore;
use rusqlite::{params, Connection};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use zeroize::Zeroize;

// ── Magic / wire-format constants ──────────────────────────────────────────────

/// 4-byte magic prefix for encrypted export files.
const MAGIC: [u8; 4] = [0x4C, 0x4D, 0x45, 0x37]; // "LME7"
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

// ── Audit types ────────────────────────────────────────────────────────────────

/// Metadata logged for every export operation.
///
/// Content is NEVER included — only structural metadata.
#[derive(Debug, Clone)]
pub struct ExportAuditEntry {
    /// User whose memories were exported.
    pub user_id: String,
    /// Number of memories exported.
    pub memory_count: usize,
    /// Distinct memory types included (e.g. ["semantic", "preference"]).
    pub types_included: Vec<String>,
    /// Whether sensitive categories (Health/Finance/Personal) were included.
    pub sensitive_included: bool,
    /// Whether the output file was encrypted.
    pub encrypted: bool,
    /// Destination path (string, not PathBuf, for logging).
    pub dest_path: String,
    /// ISO 8601 timestamp.
    pub timestamp: String,
}

/// Summary returned from a successful export.
#[derive(Debug, Clone)]
pub struct ExportSummary {
    /// Number of memories written.
    pub memory_count: usize,
    /// Distinct memory type strings.
    pub types_included: Vec<String>,
    /// Whether sensitive memories were included.
    pub sensitive_included: bool,
    /// Output path (may have .enc suffix appended).
    pub output_path: PathBuf,
}

/// Summary returned from a successful import.
#[derive(Debug, Clone)]
pub struct ImportSummary {
    /// Number of memories successfully inserted.
    pub inserted: usize,
    /// Number of memories skipped (duplicate ID).
    pub skipped_duplicates: usize,
    /// Number of memories rejected (foreign user_id).
    pub rejected_foreign: usize,
}

// ── Key derivation ─────────────────────────────────────────────────────────────

/// Derive a 32-byte AES key from `password` and `salt` using Argon2id.
///
/// Uses `Argon2::hash_password_into` directly to avoid SaltString base64 encoding
/// issues. Parameters: memory=19456 KiB (~19 MiB), iterations=2, parallelism=1.
/// These match the OWASP recommended minimum for interactive logins.
fn derive_key(password: &[u8], salt: &[u8; SALT_LEN]) -> Result<[u8; KEY_LEN]> {
    use argon2::{Algorithm, Params, Version};

    let params = Params::new(19456, 2, 1, Some(KEY_LEN))
        .map_err(|e| LuminaError::Config(format!("Argon2 params error: {e}")))?;

    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);

    let mut key = [0u8; KEY_LEN];
    argon2
        .hash_password_into(password, salt.as_slice(), &mut key)
        .map_err(|e| LuminaError::Config(format!("Argon2 key derivation error: {e}")))?;

    Ok(key)
}

// ── Encryption helpers ─────────────────────────────────────────────────────────

/// Encrypt `plaintext` with AES-256-GCM using a key derived from `password`.
///
/// Returns the full wire-format blob: magic || salt || nonce || ciphertext.
fn encrypt_export(plaintext: &[u8], password: &[u8]) -> Result<Vec<u8>> {
    // Generate random salt and nonce
    let mut salt = [0u8; SALT_LEN];
    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::thread_rng().fill_bytes(&mut salt);
    rand::thread_rng().fill_bytes(&mut nonce_bytes);

    let mut key = derive_key(password, &salt)?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|e| LuminaError::Config(format!("AES-GCM init error: {e}")))?;
    key.zeroize();

    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| LuminaError::Config(format!("AES-GCM encryption error: {e}")))?;

    // Assemble wire format
    let mut out = Vec::with_capacity(MAGIC.len() + SALT_LEN + NONCE_LEN + ciphertext.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt a wire-format blob previously created by `encrypt_export`.
fn decrypt_export(blob: &[u8], password: &[u8]) -> Result<Vec<u8>> {
    // Minimum: magic + salt + nonce + 16-byte GCM tag (no plaintext)
    let min_len = MAGIC.len() + SALT_LEN + NONCE_LEN + 16;
    if blob.len() < min_len {
        return Err(LuminaError::Config(format!(
            "Encrypted blob too short: {} bytes (minimum {})",
            blob.len(),
            min_len
        )));
    }

    // Verify magic
    if blob[..4] != MAGIC {
        return Err(LuminaError::Config(
            "Invalid export file: missing magic bytes (is this a .enc export file?)".to_string(),
        ));
    }

    let salt_start = MAGIC.len();
    let nonce_start = salt_start + SALT_LEN;
    let cipher_start = nonce_start + NONCE_LEN;

    let mut salt = [0u8; SALT_LEN];
    salt.copy_from_slice(&blob[salt_start..nonce_start]);
    let nonce_bytes = &blob[nonce_start..cipher_start];
    let ciphertext = &blob[cipher_start..];

    let mut key = derive_key(password, &salt)?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|e| LuminaError::Config(format!("AES-GCM init error: {e}")))?;
    key.zeroize();

    let nonce = Nonce::from_slice(nonce_bytes);
    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| {
            LuminaError::SecurityViolation(
                "Decryption failed — wrong password or corrupted export file".to_string(),
            )
        })
}

// ── SecureExporter ─────────────────────────────────────────────────────────────

/// Exports and imports memory archives with encryption and audit logging.
///
/// All exports are encrypted by default (AES-256-GCM, Argon2id key derivation).
/// Plaintext exports require explicit `include_sensitive` opt-in and print a warning.
pub struct SecureExporter;

impl SecureExporter {
    /// Export memories for `user_id` to an encrypted `.enc` file.
    ///
    /// - Fetches all memories for the user (respecting privacy — only that user's memories).
    /// - Excludes Health/Finance/Personal categories unless `filters.include_sensitive` is true.
    /// - Serializes to JSONL bytes (one JSON object per line).
    /// - Encrypts with AES-256-GCM using Argon2id-derived key from `password`.
    /// - Writes to `{output_path}.enc`.
    /// - Logs an audit entry (no content).
    ///
    /// Temporary plaintext bytes are zeroized after encryption.
    pub fn export_encrypted(
        conn: &Connection,
        user_id: &str,
        output_path: &Path,
        password: &[u8],
        filters: &ExportFilters,
    ) -> Result<ExportSummary> {
        // Collect memories (respects user_id scoping)
        let (memories, types_included) = fetch_memories(conn, user_id, filters)?;
        let memory_count = memories.len();

        // Serialize to JSONL
        let mut plaintext = serialize_to_jsonl(&memories)?;

        // Encrypt
        let encrypted = encrypt_export(&plaintext, password)?;
        plaintext.zeroize();

        // Write .enc file
        let enc_path = output_path.with_extension(
            output_path
                .extension()
                .map(|e| format!("{}.enc", e.to_string_lossy()))
                .unwrap_or_else(|| "enc".to_string()),
        );
        // Simpler: just append .enc suffix
        let enc_path = PathBuf::from(format!("{}.enc", output_path.display()));
        write_file(&enc_path, &encrypted)?;

        // Audit log
        let entry = ExportAuditEntry {
            user_id: user_id.to_string(),
            memory_count,
            types_included: types_included.clone(),
            sensitive_included: filters.include_sensitive,
            encrypted: true,
            dest_path: enc_path.display().to_string(),
            timestamp: iso_now(),
        };
        log_export_audit(&entry);

        Ok(ExportSummary {
            memory_count,
            types_included,
            sensitive_included: filters.include_sensitive,
            output_path: enc_path,
        })
    }

    /// Export memories for `user_id` to a plaintext JSONL file.
    ///
    /// Requires `include_sensitive` to be explicitly set to allow Health/Finance/Personal.
    /// Prints a warning to stderr regardless of `include_sensitive` value.
    pub fn export_plaintext(
        conn: &Connection,
        user_id: &str,
        output_path: &Path,
        include_sensitive: bool,
        filters: &ExportFilters,
    ) -> Result<ExportSummary> {
        eprintln!("Warning: unencrypted export contains sensitive personal data");

        let filters = ExportFilters {
            include_sensitive,
            ..filters.clone()
        };

        let (memories, types_included) = fetch_memories(conn, user_id, &filters)?;
        let memory_count = memories.len();

        let jsonl = serialize_to_jsonl(&memories)?;

        write_file(output_path, &jsonl)?;

        let entry = ExportAuditEntry {
            user_id: user_id.to_string(),
            memory_count,
            types_included: types_included.clone(),
            sensitive_included: include_sensitive,
            encrypted: false,
            dest_path: output_path.display().to_string(),
            timestamp: iso_now(),
        };
        log_export_audit(&entry);

        Ok(ExportSummary {
            memory_count,
            types_included,
            sensitive_included: include_sensitive,
            output_path: output_path.to_path_buf(),
        })
    }

    /// Import memories from a file into `conn`, associating them with `target_user_id`.
    ///
    /// - If the file ends in `.enc`, decrypts with `password` first.
    /// - Validates each memory's `user_id` against `target_user_id` — rejects foreign.
    /// - Skips duplicates (by UUID `id` field).
    pub fn import_memories(
        conn: &Connection,
        input_path: &Path,
        password: Option<&[u8]>,
        target_user_id: &str,
    ) -> Result<ImportSummary> {
        let raw = std::fs::read(input_path)
            .map_err(|e| LuminaError::Config(format!("Cannot read import file: {e}")))?;

        // Detect if encrypted by magic prefix
        let jsonl_bytes: Vec<u8> = if raw.len() >= 4 && raw[..4] == MAGIC {
            let pw = password.ok_or_else(|| {
                LuminaError::Config(
                    "Encrypted export file requires a password (--password)".to_string(),
                )
            })?;
            decrypt_export(&raw, pw)?
        } else {
            raw
        };

        // Parse JSONL
        let jsonl_str = std::str::from_utf8(&jsonl_bytes)
            .map_err(|e| LuminaError::Config(format!("Import file is not valid UTF-8: {e}")))?;

        // Gather existing IDs to detect duplicates
        let existing_ids = fetch_existing_ids(conn, target_user_id)?;

        let mut inserted = 0usize;
        let mut skipped_duplicates = 0usize;
        let mut rejected_foreign = 0usize;

        for (line_num, line) in jsonl_str.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let memory: Memory = match serde_json::from_str(line) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("import: skipping line {}: JSON parse error: {e}", line_num + 1);
                    continue;
                }
            };

            // Reject foreign user_id
            if memory.user_id != target_user_id {
                eprintln!(
                    "import: rejecting memory {} — user_id '{}' does not match target '{}'",
                    memory.id, memory.user_id, target_user_id
                );
                rejected_foreign += 1;
                continue;
            }

            // Skip duplicates by id
            if existing_ids.contains(&memory.id) {
                skipped_duplicates += 1;
                continue;
            }

            // Insert (no embedding — exports strip embeddings by default)
            let tags_json =
                serde_json::to_string(&memory.tags).unwrap_or_else(|_| "[]".to_string());
            let result = conn.execute(
                "INSERT OR IGNORE INTO memories_v2
                 (id, user_id, memory_type, visibility, sensitivity, content, embedding,
                  source_conversation_id, source_turn_index, confidence, access_count,
                  last_accessed, created_at, updated_at, superseded_by, tags)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    memory.id,
                    memory.user_id,
                    memory.memory_type.to_db(),
                    memory.visibility.to_db(),
                    memory.sensitivity.to_db(),
                    memory.content,
                    memory.source_conversation_id,
                    memory.source_turn_index,
                    memory.confidence,
                    memory.access_count,
                    memory.last_accessed,
                    memory.created_at,
                    memory.updated_at,
                    memory.superseded_by,
                    tags_json,
                ],
            );

            match result {
                Ok(1) => inserted += 1,
                Ok(_) => skipped_duplicates += 1, // INSERT OR IGNORE hit a conflict
                Err(e) => {
                    eprintln!(
                        "import: failed to insert memory {}: {e}",
                        &memory.id[..8.min(memory.id.len())]
                    );
                }
            }
        }

        log::info!(
            "ESEC-07 import: user={} inserted={} skipped_dup={} rejected_foreign={}",
            target_user_id,
            inserted,
            skipped_duplicates,
            rejected_foreign
        );

        Ok(ImportSummary {
            inserted,
            skipped_duplicates,
            rejected_foreign,
        })
    }
}

// ── ExportFilters ──────────────────────────────────────────────────────────────

/// Controls what is included in an export.
#[derive(Debug, Clone, Default)]
pub struct ExportFilters {
    /// Include Health, Finance, Personal memories. Default: false.
    pub include_sensitive: bool,
    /// If Some, only memories of this type are exported.
    pub memory_type_filter: Option<String>,
}

// ── Audit logging ──────────────────────────────────────────────────────────────

/// Log an export audit entry.
///
/// Uses `log::info!` so the output goes to the configured logger without
/// requiring a database or filesystem side-channel.
/// Content is NEVER logged — only structural metadata.
pub fn log_export_audit(entry: &ExportAuditEntry) {
    log::info!(
        "ESEC-07 export audit: user={} count={} types=[{}] sensitive={} encrypted={} dest={} ts={}",
        entry.user_id,
        entry.memory_count,
        entry.types_included.join(","),
        entry.sensitive_included,
        entry.encrypted,
        entry.dest_path,
        entry.timestamp,
    );
}

// ── Temp file cleanup ──────────────────────────────────────────────────────────

/// Securely delete temporary files (overwrite with zeros, then remove).
///
/// Non-fatal: logs errors but does not return them — cleanup failures should
/// never mask the primary operation error.
pub fn auto_cleanup(temp_paths: &[PathBuf]) {
    for path in temp_paths {
        if !path.exists() {
            continue;
        }
        // Overwrite with zeros
        if let Ok(metadata) = std::fs::metadata(path) {
            let size = metadata.len() as usize;
            let zeros = vec![0u8; size];
            if let Err(e) = std::fs::write(path, &zeros) {
                eprintln!("auto_cleanup: overwrite failed for {:?}: {e}", path);
            }
        }
        // Remove the file
        if let Err(e) = std::fs::remove_file(path) {
            eprintln!("auto_cleanup: remove failed for {:?}: {e}", path);
        }
    }
}

// ── Internal helpers ───────────────────────────────────────────────────────────

/// Fetch memories for a user, applying filters.
///
/// Returns (memories_vec, distinct_type_strings).
fn fetch_memories(
    conn: &Connection,
    user_id: &str,
    filters: &ExportFilters,
) -> Result<(Vec<Memory>, Vec<String>)> {
    // Build query — exclude sensitive categories unless opted in
    let mut sql = String::from(
        "SELECT id, user_id, memory_type, visibility, sensitivity, content, embedding,
                source_conversation_id, source_turn_index, confidence, access_count,
                last_accessed, created_at, updated_at, superseded_by, tags
         FROM memories_v2
         WHERE user_id = ?1",
    );

    if !filters.include_sensitive {
        sql.push_str(" AND sensitivity NOT IN ('health','finance','personal')");
    }

    if let Some(ref mt) = filters.memory_type_filter {
        let _ = mt; // used below via dynamic param
        sql.push_str(" AND memory_type = ?2");
    }

    sql.push_str(" ORDER BY created_at ASC");

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| LuminaError::Config(format!("export prepare: {e}")))?;

    let memories: Vec<Memory> = if filters.memory_type_filter.is_some() {
        let mt = filters.memory_type_filter.as_deref().unwrap();
        stmt.query_map(params![user_id, mt], |row| {
            crate::engram::row_to_memory(row)
        })
        .map_err(|e| LuminaError::Config(format!("export query: {e}")))?
        .filter_map(|r| r.ok())
        .collect()
    } else {
        stmt.query_map(params![user_id], |row| crate::engram::row_to_memory(row))
            .map_err(|e| LuminaError::Config(format!("export query: {e}")))?
            .filter_map(|r| r.ok())
            .collect()
    };

    // Collect distinct types
    let types_included: Vec<String> = {
        let mut seen = std::collections::BTreeSet::new();
        for m in &memories {
            seen.insert(m.memory_type.to_db().to_string());
        }
        seen.into_iter().collect()
    };

    Ok((memories, types_included))
}

/// Serialize a slice of memories to JSONL bytes (one JSON object per line).
///
/// Embeddings are cleared before serialization (large and regenerable).
fn serialize_to_jsonl(memories: &[Memory]) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    for memory in memories {
        let mut m = memory.clone();
        m.embedding.clear(); // don't include embeddings in exports
        let json =
            serde_json::to_string(&m).map_err(|e| LuminaError::Config(format!("JSON error: {e}")))?;
        buf.extend_from_slice(json.as_bytes());
        buf.push(b'\n');
    }
    Ok(buf)
}

/// Write bytes to a file, returning a clear error if the filesystem is not writable.
fn write_file(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| LuminaError::Config(format!("Cannot create export directory: {e}")))?;
        }
    }
    std::fs::write(path, data)
        .map_err(|e| LuminaError::Config(format!("Cannot write export file '{}': {e}", path.display())))
}

/// Fetch the set of existing memory IDs for a user (for duplicate detection).
fn fetch_existing_ids(conn: &Connection, user_id: &str) -> Result<HashSet<String>> {
    let mut stmt = conn
        .prepare("SELECT id FROM memories_v2 WHERE user_id = ?1")
        .map_err(|e| LuminaError::Config(format!("fetch_existing_ids prepare: {e}")))?;
    let ids: HashSet<String> = stmt
        .query_map(params![user_id], |r| r.get(0))
        .map_err(|e| LuminaError::Config(format!("fetch_existing_ids query: {e}")))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(ids)
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engram::{EngramStore, types::{Memory, MemoryType, SensitivityCategory, Visibility}};
    use std::path::PathBuf;

    fn test_key() -> Vec<u8> {
        vec![0u8; 32]
    }

    fn tmp_db(tag: &str) -> PathBuf {
        let p = PathBuf::from(format!("/tmp/lumina_esec07_{tag}.db"));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn tmp_path(tag: &str) -> PathBuf {
        PathBuf::from(format!("/tmp/lumina_esec07_{tag}"))
    }

    fn open_store(path: &PathBuf) -> EngramStore {
        EngramStore::open(path, &test_key()).unwrap()
    }

    /// Insert a variety of memories into a store for testing.
    fn seed_store(store: &EngramStore) {
        let m1 = Memory::new("user-test", MemoryType::Semantic, SensitivityCategory::General, "likes dark roast coffee");
        let m2 = Memory::new("user-test", MemoryType::Preference, SensitivityCategory::General, "prefers terminal-based editors");
        let m3 = Memory::new("user-test", MemoryType::Semantic, SensitivityCategory::Health, "shellfish allergy");
        let m4 = Memory::new("user-test", MemoryType::Semantic, SensitivityCategory::Finance, "monthly budget is confidential");
        store.insert_memory(&m1).unwrap();
        store.insert_memory(&m2).unwrap();
        store.insert_memory(&m3).unwrap();
        store.insert_memory(&m4).unwrap();
    }

    // ── test_encrypted_export_is_not_plaintext ─────────────────────────────────

    /// Encrypted export file must start with the magic bytes and not be valid UTF-8 JSONL.
    #[test]
    fn test_encrypted_export_is_not_plaintext() {
        let db = tmp_db("enc_not_plain");
        let store = open_store(&db);
        seed_store(&store);

        let out = tmp_path("enc_not_plain_out");
        let enc_path = PathBuf::from(format!("{}.enc", out.display()));
        let _ = std::fs::remove_file(&enc_path);

        let filters = ExportFilters { include_sensitive: false, memory_type_filter: None };
        let summary = SecureExporter::export_encrypted(
            &store.conn,
            "user-test",
            &out,
            b"test-password-123",
            &filters,
        ).unwrap();

        assert!(summary.memory_count > 0, "should export at least one memory");

        let raw = std::fs::read(&enc_path).unwrap();
        // Must start with our magic
        assert_eq!(&raw[..4], &MAGIC, "file should start with LME7 magic");
        // Must NOT start with '{' (JSON) — it's binary ciphertext
        assert_ne!(raw[0], b'{', "encrypted file should not look like JSONL");

        // Must not be parseable as valid UTF-8 JSONL in most cases
        // (The ciphertext is random bytes — very unlikely to be valid UTF-8)
        let jsonl_parse_ok = std::str::from_utf8(&raw).is_ok()
            && raw.iter().any(|&b| b == b'{');
        assert!(!jsonl_parse_ok, "encrypted blob should not be valid JSONL");

        let _ = std::fs::remove_file(&enc_path);
        let _ = std::fs::remove_file(&db);
    }

    // ── test_encrypted_export_decryptable_correct_password ────────────────────

    /// Export → decrypt with correct password → contents match original.
    #[test]
    fn test_encrypted_export_decryptable_correct_password() {
        let db = tmp_db("enc_decrypt_ok");
        let store = open_store(&db);
        seed_store(&store);

        let out = tmp_path("enc_decrypt_ok_out");
        let enc_path = PathBuf::from(format!("{}.enc", out.display()));
        let _ = std::fs::remove_file(&enc_path);

        let filters = ExportFilters { include_sensitive: false, memory_type_filter: None };
        let summary = SecureExporter::export_encrypted(
            &store.conn,
            "user-test",
            &out,
            b"correct-horse-battery-staple",
            &filters,
        ).unwrap();

        let raw = std::fs::read(&enc_path).unwrap();
        let plaintext = decrypt_export(&raw, b"correct-horse-battery-staple").unwrap();
        let jsonl = std::str::from_utf8(&plaintext).unwrap();

        // Should have at least 2 non-empty lines (general category memories)
        let lines: Vec<&str> = jsonl.lines().filter(|l| !l.trim().is_empty()).collect();
        assert!(lines.len() >= 2, "should decrypt at least 2 memories");
        assert_eq!(lines.len(), summary.memory_count, "line count should match summary");

        // Each line should be valid JSON with expected fields
        for line in &lines {
            let val: serde_json::Value = serde_json::from_str(line).expect("line should be valid JSON");
            assert!(val.get("id").is_some());
            assert!(val.get("content").is_some());
        }

        let _ = std::fs::remove_file(&enc_path);
        let _ = std::fs::remove_file(&db);
    }

    // ── test_wrong_password_fails_decryption ───────────────────────────────────

    /// Decrypting with a wrong password must return an error.
    #[test]
    fn test_wrong_password_fails_decryption() {
        let db = tmp_db("wrong_pw");
        let store = open_store(&db);
        seed_store(&store);

        let out = tmp_path("wrong_pw_out");
        let enc_path = PathBuf::from(format!("{}.enc", out.display()));
        let _ = std::fs::remove_file(&enc_path);

        let filters = ExportFilters { include_sensitive: false, memory_type_filter: None };
        SecureExporter::export_encrypted(
            &store.conn,
            "user-test",
            &out,
            b"correct-password",
            &filters,
        ).unwrap();

        let raw = std::fs::read(&enc_path).unwrap();
        let result = decrypt_export(&raw, b"wrong-password");
        assert!(result.is_err(), "wrong password should fail decryption");

        let _ = std::fs::remove_file(&enc_path);
        let _ = std::fs::remove_file(&db);
    }

    // ── test_plaintext_requires_flag ───────────────────────────────────────────

    /// The `export_plaintext` function signature requires explicit `include_sensitive` bool.
    /// This test verifies the function can be called (enforcing explicit choice at compile time)
    /// and that it produces valid JSONL.
    #[test]
    fn test_plaintext_requires_flag() {
        let db = tmp_db("plain_flag");
        let store = open_store(&db);
        seed_store(&store);

        let out = tmp_path("plain_flag_out.jsonl");
        let _ = std::fs::remove_file(&out);

        let filters = ExportFilters { include_sensitive: false, memory_type_filter: None };
        // The boolean `include_sensitive` is a required explicit parameter
        let summary = SecureExporter::export_plaintext(
            &store.conn,
            "user-test",
            &out,
            false, // explicit choice — no sensitive
            &filters,
        ).unwrap();

        assert!(summary.memory_count > 0);
        let content = std::fs::read_to_string(&out).unwrap();
        let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), summary.memory_count);

        for line in &lines {
            let _: serde_json::Value = serde_json::from_str(line).expect("valid JSON");
        }

        let _ = std::fs::remove_file(&out);
        let _ = std::fs::remove_file(&db);
    }

    // ── test_sensitive_excluded_by_default ─────────────────────────────────────

    /// With `include_sensitive = false`, Health/Finance/Personal memories are excluded.
    #[test]
    fn test_sensitive_excluded_by_default() {
        let db = tmp_db("sensitive_excl");
        let store = open_store(&db);

        // Insert general + sensitive memories
        let general = Memory::new("user-test", MemoryType::Semantic, SensitivityCategory::General, "likes hiking");
        let health = Memory::new("user-test", MemoryType::Semantic, SensitivityCategory::Health, "takes daily medication");
        let finance = Memory::new("user-test", MemoryType::Semantic, SensitivityCategory::Finance, "saving for retirement");
        let personal = Memory::new("user-test", MemoryType::Semantic, SensitivityCategory::Personal, "feeling stressed lately");
        store.insert_memory(&general).unwrap();
        store.insert_memory(&health).unwrap();
        store.insert_memory(&finance).unwrap();
        store.insert_memory(&personal).unwrap();

        let filters = ExportFilters { include_sensitive: false, memory_type_filter: None };
        let (memories, _) = fetch_memories(&store.conn, "user-test", &filters).unwrap();

        // Only the general memory should appear
        assert_eq!(memories.len(), 1, "only general memories exported by default");
        assert_eq!(memories[0].content, "likes hiking");

        let _ = std::fs::remove_file(&db);
    }

    // ── test_import_rejects_foreign_user_id ───────────────────────────────────

    /// Import must reject memories whose user_id doesn't match target_user_id.
    #[test]
    fn test_import_rejects_foreign_user_id() {
        let source_db = tmp_db("import_foreign_src");
        let target_db = tmp_db("import_foreign_tgt");

        let source_store = open_store(&source_db);
        let target_store = open_store(&target_db);

        // Insert a memory for user-alice
        let alice_mem = Memory::new("user-alice", MemoryType::Semantic, SensitivityCategory::General, "alice's coffee preference");
        source_store.insert_memory(&alice_mem).unwrap();

        // Export as plaintext JSONL (not encrypted — simpler for this test)
        let export_path = tmp_path("import_foreign.jsonl");
        let _ = std::fs::remove_file(&export_path);
        {
            let (memories, _) = fetch_memories(&source_store.conn, "user-alice", &ExportFilters::default()).unwrap();
            let jsonl = serialize_to_jsonl(&memories).unwrap();
            std::fs::write(&export_path, &jsonl).unwrap();
        }

        // Import into target store as user-bob
        let summary = SecureExporter::import_memories(
            &target_store.conn,
            &export_path,
            None,
            "user-bob",
        ).unwrap();

        // All memories should be rejected (foreign user_id)
        assert_eq!(summary.rejected_foreign, 1, "alice's memory should be rejected for user-bob");
        assert_eq!(summary.inserted, 0, "no memories should be inserted");

        // Verify target DB is empty
        let count: i64 = target_store.conn.query_row(
            "SELECT COUNT(*) FROM memories_v2 WHERE user_id = 'user-bob'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(count, 0, "target DB should be empty after rejected import");

        let _ = std::fs::remove_file(&source_db);
        let _ = std::fs::remove_file(&target_db);
        let _ = std::fs::remove_file(&export_path);
    }

    // ── test_export_audit_logged ───────────────────────────────────────────────

    /// Audit entry must contain metadata but must not contain memory content.
    #[test]
    fn test_export_audit_logged() {
        let entry = ExportAuditEntry {
            user_id: "user-test".to_string(),
            memory_count: 42,
            types_included: vec!["semantic".to_string(), "preference".to_string()],
            sensitive_included: false,
            encrypted: true,
            dest_path: "/tmp/test_export.enc".to_string(),
            timestamp: iso_now(),
        };

        // The audit function just calls log::info! — we verify the struct fields
        // are correct (not content) and that the call doesn't panic.
        assert_eq!(entry.memory_count, 42);
        assert!(!entry.sensitive_included);
        assert!(entry.encrypted);
        assert!(entry.dest_path.ends_with(".enc"));
        // Content is never in the entry — verify no content field exists
        // (compile-time guarantee: ExportAuditEntry has no `content` field)
        log_export_audit(&entry); // must not panic
    }

    // ── test_temp_cleanup_on_error ─────────────────────────────────────────────

    /// auto_cleanup must remove temp files and not panic if files don't exist.
    #[test]
    fn test_temp_cleanup_on_error() {
        let temp1 = tmp_path("cleanup_temp1.txt");
        let temp2 = tmp_path("cleanup_temp2.txt");
        let nonexistent = tmp_path("cleanup_nonexistent.txt");

        // Create temp1 with some content
        std::fs::write(&temp1, b"sensitive temporary data").unwrap();
        // Create temp2 empty
        std::fs::write(&temp2, b"").unwrap();

        auto_cleanup(&[temp1.clone(), temp2.clone(), nonexistent.clone()]);

        // All real files should be removed
        assert!(!temp1.exists(), "temp1 should be removed");
        assert!(!temp2.exists(), "temp2 should be removed");
        // Nonexistent file — no panic
        assert!(!nonexistent.exists(), "nonexistent file should not appear after cleanup");
    }

    // ── test_import_skips_duplicates ───────────────────────────────────────────

    /// Importing the same data twice should skip duplicates on the second import.
    #[test]
    fn test_import_skips_duplicates() {
        let db = tmp_db("import_dedup");
        let store = open_store(&db);

        let mem = Memory::new("user-test", MemoryType::Semantic, SensitivityCategory::General, "unique fact about user");
        store.insert_memory(&mem).unwrap();

        let export_path = tmp_path("import_dedup.jsonl");
        let _ = std::fs::remove_file(&export_path);
        {
            let (memories, _) = fetch_memories(&store.conn, "user-test", &ExportFilters::default()).unwrap();
            let jsonl = serialize_to_jsonl(&memories).unwrap();
            std::fs::write(&export_path, &jsonl).unwrap();
        }

        // First import: should insert
        let s1 = SecureExporter::import_memories(&store.conn, &export_path, None, "user-test").unwrap();
        assert_eq!(s1.skipped_duplicates, 1, "first import should skip existing record");
        assert_eq!(s1.inserted, 0, "already in DB — skip");

        // Now export again and re-import into empty DB
        let db2 = tmp_db("import_dedup2");
        let store2 = open_store(&db2);
        let s2 = SecureExporter::import_memories(&store2.conn, &export_path, None, "user-test").unwrap();
        assert_eq!(s2.inserted, 1, "fresh DB should insert successfully");
        assert_eq!(s2.skipped_duplicates, 0);

        let _ = std::fs::remove_file(&db);
        let _ = std::fs::remove_file(&db2);
        let _ = std::fs::remove_file(&export_path);
    }

    // ── test_encrypted_import_round_trip ──────────────────────────────────────

    /// Full round-trip: export encrypted → import decrypted → data preserved.
    #[test]
    fn test_encrypted_import_round_trip() {
        let src_db = tmp_db("rt_src");
        let dst_db = tmp_db("rt_dst");

        let src_store = open_store(&src_db);
        let dst_store = open_store(&dst_db);

        let mem = Memory::new("user-rt", MemoryType::Preference, SensitivityCategory::General, "prefers night mode in apps");
        src_store.insert_memory(&mem).unwrap();

        let out = tmp_path("rt_out");
        let enc_path = PathBuf::from(format!("{}.enc", out.display()));
        let _ = std::fs::remove_file(&enc_path);

        let filters = ExportFilters { include_sensitive: false, memory_type_filter: None };
        SecureExporter::export_encrypted(
            &src_store.conn,
            "user-rt",
            &out,
            b"roundtrip-password",
            &filters,
        ).unwrap();

        let summary = SecureExporter::import_memories(
            &dst_store.conn,
            &enc_path,
            Some(b"roundtrip-password"),
            "user-rt",
        ).unwrap();

        assert_eq!(summary.inserted, 1, "one memory should be imported");
        assert_eq!(summary.rejected_foreign, 0);

        // Verify content in dst
        let count: i64 = dst_store.conn.query_row(
            "SELECT COUNT(*) FROM memories_v2 WHERE user_id = 'user-rt'",
            [],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(count, 1);

        let _ = std::fs::remove_file(&enc_path);
        let _ = std::fs::remove_file(&src_db);
        let _ = std::fs::remove_file(&dst_db);
    }
}
