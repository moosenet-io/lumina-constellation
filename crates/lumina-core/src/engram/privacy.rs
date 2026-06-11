//! EMEM-02: Privacy enforcement layer for Engram v2.
//!
//! The PrivacyEnforcer is the security-critical component ensuring:
//! 1. Private memories NEVER cross user boundaries
//! 2. Sensitive categories (Health, Finance, Personal) are ALWAYS private
//! 3. Shared memories require explicit action and cannot be sensitive
//! 4. Privacy violations are logged to audit
//!
//! Enforcement happens at both the application layer (PrivacyEnforcer methods)
//! AND the database layer (SQL WHERE clause in query helpers). Defense in depth.

use crate::error::{LuminaError, Result};
use super::types::{Memory, SensitivityCategory, Visibility};

// ── Privacy violation ─────────────────────────────────────────────────────────

/// Reason a privacy check was denied. Logged to audit but NEVER revealed to the
/// requesting user — they only see a generic "not accessible" message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrivacyViolation {
    /// Caller's user_id doesn't match the memory's user_id (private memory).
    CrossUserAccess { caller: String, owner: String },
    /// Shared memory, but caller is not a household member (future EMEM-08).
    NotHouseholdMember { caller: String },
    /// Attempt to share a Health/Finance/Personal memory.
    SensitiveCategoryShare { sensitivity: String },
    /// Caller tried to delete a memory they don't own.
    DeleteNotOwned { caller: String, owner: String },
    /// Attempt to share a memory by someone who isn't the creator.
    ShareNotOwned { caller: String, owner: String },
}

impl std::fmt::Display for PrivacyViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CrossUserAccess { caller, owner } => {
                write!(f, "cross-user access: caller={caller} owner={owner}")
            }
            Self::NotHouseholdMember { caller } => {
                write!(f, "not a household member: caller={caller}")
            }
            Self::SensitiveCategoryShare { sensitivity } => {
                write!(f, "cannot share sensitive category: {sensitivity}")
            }
            Self::DeleteNotOwned { caller, owner } => {
                write!(f, "delete not owned: caller={caller} owner={owner}")
            }
            Self::ShareNotOwned { caller, owner } => {
                write!(f, "share not owned: caller={caller} owner={owner}")
            }
        }
    }
}

impl PrivacyViolation {
    /// Convert to a LuminaError with a generic user-facing message.
    /// The violation details are in the error context (for audit) but the
    /// user-visible portion never reveals what data exists.
    pub fn to_error(&self) -> LuminaError {
        LuminaError::SecurityViolation(
            "This memory is not accessible.".to_string()
        )
    }
}

// ── PrivacyEnforcer ────────────────────────────────────────────────────────────

/// Stateless privacy validation for Engram memory operations.
///
/// All methods are pure — they take data, validate it, and return Ok or Err.
/// No database access. Callers integrate this into every read/write/share path.
pub struct PrivacyEnforcer;

impl PrivacyEnforcer {
    /// Validate that `caller_user_id` may ACCESS the given memory.
    ///
    /// Rules:
    /// - Private memory: caller must be the owner (exact match)
    /// - Shared memory: caller must be a household member (simplified: any user for now, EMEM-08 adds membership check)
    /// - System memory: accessible to all users
    ///
    /// On denial: logs the violation to audit and returns an opaque error.
    pub fn validate_access(caller_user_id: &str, memory: &Memory) -> Result<()> {
        match memory.visibility {
            Visibility::Private => {
                if caller_user_id != memory.user_id {
                    let violation = PrivacyViolation::CrossUserAccess {
                        caller: caller_user_id.to_string(),
                        owner: memory.user_id.clone(),
                    };
                    Self::log_violation(&violation, "read");
                    return Err(violation.to_error());
                }
                Ok(())
            }
            Visibility::Shared => {
                // EMEM-08 will add household membership check here.
                // For now, any authenticated user can read shared memories.
                Ok(())
            }
            Visibility::System => {
                // System memories are readable by all users.
                Ok(())
            }
        }
    }

    /// Enforce sensitivity rules on a memory BEFORE storage.
    ///
    /// Forces `visibility = Private` for Health/Finance/Personal regardless of
    /// what was requested. Mutates the memory in-place.
    ///
    /// This is the hard enforcement — no code path can bypass it by forgetting
    /// to check sensitivity before storing.
    pub fn enforce_sensitivity(memory: &mut Memory) {
        if memory.sensitivity.is_always_private() {
            if memory.visibility != Visibility::Private {
                // Log that we overrode a non-private visibility attempt
                let msg = format!(
                    "sensitivity enforcement: forced {} memory to Private (was {:?})",
                    memory.sensitivity.to_db(),
                    memory.visibility,
                );
                eprintln!("engram/privacy: {msg}");
            }
            memory.visibility = Visibility::Private;
        }
    }

    /// Validate that `caller_user_id` may SHARE a memory (change visibility to Shared).
    ///
    /// Rules:
    /// - Only the memory creator (user_id) may share it
    /// - Health/Finance/Personal memories cannot EVER be shared
    ///
    /// Returns the approved target visibility if sharing is allowed.
    pub fn validate_share(
        caller_user_id: &str,
        memory: &Memory,
    ) -> Result<Visibility> {
        // Sensitive categories can never be shared
        if memory.sensitivity.is_always_private() {
            let violation = PrivacyViolation::SensitiveCategoryShare {
                sensitivity: memory.sensitivity.to_db().to_string(),
            };
            Self::log_violation(&violation, "share");
            return Err(LuminaError::SecurityViolation(format!(
                "{}/financial/personal information cannot be shared for privacy protection.",
                capitalize_first(memory.sensitivity.to_db())
            )));
        }

        // Only the owner can share
        if caller_user_id != memory.user_id {
            let violation = PrivacyViolation::ShareNotOwned {
                caller: caller_user_id.to_string(),
                owner: memory.user_id.clone(),
            };
            Self::log_violation(&violation, "share");
            return Err(violation.to_error());
        }

        Ok(Visibility::Shared)
    }

    /// Validate that `caller_user_id` may DELETE a memory.
    ///
    /// Only the owner may delete their own memories.
    /// Shared memories: the creator (user_id) or admin can delete.
    pub fn validate_delete(caller_user_id: &str, memory: &Memory) -> Result<()> {
        if caller_user_id != memory.user_id {
            let violation = PrivacyViolation::DeleteNotOwned {
                caller: caller_user_id.to_string(),
                owner: memory.user_id.clone(),
            };
            Self::log_violation(&violation, "delete");
            return Err(violation.to_error());
        }
        Ok(())
    }

    /// Log a privacy violation.
    ///
    /// Details go to stderr as a structured security event — NEVER shown to the
    /// requesting user. Admins can monitor stderr/syslog for ENGRAM_PRIVACY lines.
    fn log_violation(violation: &PrivacyViolation, operation: &str) {
        eprintln!(
            "ENGRAM_PRIVACY VIOLATION op={operation} detail={}",
            violation
        );
    }
}

// ── Database-level privacy WHERE clause ──────────────────────────────────────

/// Build a SQL WHERE clause that enforces privacy at the database level.
///
/// Defense in depth: even if application code forgets to call PrivacyEnforcer,
/// this filter ensures results are always user-scoped. The clause returns:
/// - The user's own private memories
/// - All shared memories (EMEM-08 will refine to household members)
/// - All system memories
///
/// The caller appends `AND (...)` to their query. Placeholder index starts at
/// `user_id_param_idx` (typically 1).
pub fn privacy_where_clause(user_id_param_idx: usize) -> String {
    format!(
        "(user_id = ?{} AND visibility IN ('private', 'shared', 'system'))
         OR visibility = 'system'
         OR visibility = 'shared'",
        user_id_param_idx
    )
}

/// Simplified version: just `user_id = ?` for per-user private stores.
/// Use this when the store is already scoped to a single user (per-user DB file).
pub fn user_scoped_where() -> &'static str {
    "user_id = ?1"
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn capitalize_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engram::types::{Memory, MemoryType, SensitivityCategory, Visibility};

    fn private_memory(user_id: &str) -> Memory {
        let mut m = Memory::new(user_id, MemoryType::Semantic, SensitivityCategory::General, "test content");
        m.visibility = Visibility::Private;
        m
    }

    fn shared_memory(user_id: &str) -> Memory {
        let mut m = Memory::new(user_id, MemoryType::Semantic, SensitivityCategory::Household, "shared grocery list");
        m.visibility = Visibility::Shared;
        m
    }

    fn system_memory() -> Memory {
        let mut m = Memory::new("system", MemoryType::Semantic, SensitivityCategory::System, "tool catalog");
        m.visibility = Visibility::System;
        m
    }

    // ── validate_access ───────────────────────────────────────────────────────

    #[test]
    fn test_owner_can_access_own_private_memory() {
        let mem = private_memory("alice");
        assert!(PrivacyEnforcer::validate_access("alice", &mem).is_ok());
    }

    #[test]
    fn test_other_user_cannot_access_private_memory() {
        let mem = private_memory("alice");
        let result = PrivacyEnforcer::validate_access("bob", &mem);
        assert!(result.is_err(), "Bob should not access Alice's private memory");
    }

    #[test]
    fn test_cross_user_access_returns_opaque_error() {
        let mem = private_memory("alice");
        let err = PrivacyEnforcer::validate_access("bob", &mem).unwrap_err();
        let msg = err.to_string();
        // Error must NOT reveal the memory content or owner details
        assert!(!msg.contains("alice"), "Error should not reveal owner: {msg}");
        assert!(msg.contains("not accessible") || msg.contains("SecurityViolation"), "{msg}");
    }

    #[test]
    fn test_any_user_can_access_shared_memory() {
        let mem = shared_memory("alice");
        assert!(PrivacyEnforcer::validate_access("alice", &mem).is_ok());
        assert!(PrivacyEnforcer::validate_access("bob", &mem).is_ok());
        assert!(PrivacyEnforcer::validate_access("carol", &mem).is_ok());
    }

    #[test]
    fn test_any_user_can_access_system_memory() {
        let mem = system_memory();
        assert!(PrivacyEnforcer::validate_access("alice", &mem).is_ok());
        assert!(PrivacyEnforcer::validate_access("bob", &mem).is_ok());
        assert!(PrivacyEnforcer::validate_access("system", &mem).is_ok());
    }

    // ── enforce_sensitivity ───────────────────────────────────────────────────

    #[test]
    fn test_health_memory_forced_to_private() {
        let mut m = Memory::new("alice", MemoryType::Semantic, SensitivityCategory::Health, "has allergy");
        // Even if someone sets Shared...
        m.visibility = Visibility::Shared;
        PrivacyEnforcer::enforce_sensitivity(&mut m);
        assert_eq!(m.visibility, Visibility::Private, "Health must be forced to Private");
    }

    #[test]
    fn test_finance_memory_forced_to_private() {
        let mut m = Memory::new("alice", MemoryType::Semantic, SensitivityCategory::Finance, "salary info");
        m.visibility = Visibility::Shared;
        PrivacyEnforcer::enforce_sensitivity(&mut m);
        assert_eq!(m.visibility, Visibility::Private);
    }

    #[test]
    fn test_personal_memory_forced_to_private() {
        let mut m = Memory::new("alice", MemoryType::Semantic, SensitivityCategory::Personal, "private life");
        m.visibility = Visibility::Shared;
        PrivacyEnforcer::enforce_sensitivity(&mut m);
        assert_eq!(m.visibility, Visibility::Private);
    }

    #[test]
    fn test_general_memory_visibility_not_changed() {
        let mut m = Memory::new("alice", MemoryType::Semantic, SensitivityCategory::General, "fact");
        // Default is Private — confirm unchanged
        PrivacyEnforcer::enforce_sensitivity(&mut m);
        assert_eq!(m.visibility, Visibility::Private);

        // Shared general is allowed
        m.visibility = Visibility::Shared;
        PrivacyEnforcer::enforce_sensitivity(&mut m);
        assert_eq!(m.visibility, Visibility::Shared, "General shared should stay Shared");
    }

    #[test]
    fn test_household_memory_not_forced_to_private() {
        let mut m = Memory::new("alice", MemoryType::Semantic, SensitivityCategory::Household, "grocery list");
        m.visibility = Visibility::Shared;
        PrivacyEnforcer::enforce_sensitivity(&mut m);
        assert_eq!(m.visibility, Visibility::Shared, "Household shared should stay Shared");
    }

    // ── validate_share ────────────────────────────────────────────────────────

    #[test]
    fn test_owner_can_share_general_memory() {
        let mem = private_memory("alice");
        let result = PrivacyEnforcer::validate_share("alice", &mem);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Visibility::Shared);
    }

    #[test]
    fn test_non_owner_cannot_share_memory() {
        let mem = private_memory("alice");
        let result = PrivacyEnforcer::validate_share("bob", &mem);
        assert!(result.is_err(), "Bob cannot share Alice's memory");
    }

    #[test]
    fn test_health_memory_cannot_be_shared() {
        let mut mem = Memory::new("alice", MemoryType::Semantic, SensitivityCategory::Health, "allergy info");
        mem.visibility = Visibility::Private;
        let result = PrivacyEnforcer::validate_share("alice", &mem);
        assert!(result.is_err(), "Health memory must never be shareable");
        let msg = result.unwrap_err().to_string();
        // Error should mention privacy protection
        assert!(msg.contains("privacy protection") || msg.contains("SecurityViolation"),
            "Error should mention privacy protection: {msg}");
    }

    #[test]
    fn test_finance_memory_cannot_be_shared() {
        let mut mem = Memory::new("alice", MemoryType::Semantic, SensitivityCategory::Finance, "bank account");
        mem.visibility = Visibility::Private;
        let result = PrivacyEnforcer::validate_share("alice", &mem);
        assert!(result.is_err());
    }

    #[test]
    fn test_personal_memory_cannot_be_shared() {
        let mut mem = Memory::new("alice", MemoryType::Semantic, SensitivityCategory::Personal, "personal struggle");
        mem.visibility = Visibility::Private;
        let result = PrivacyEnforcer::validate_share("alice", &mem);
        assert!(result.is_err());
    }

    // ── validate_delete ───────────────────────────────────────────────────────

    #[test]
    fn test_owner_can_delete_own_memory() {
        let mem = private_memory("alice");
        assert!(PrivacyEnforcer::validate_delete("alice", &mem).is_ok());
    }

    #[test]
    fn test_other_user_cannot_delete_memory() {
        let mem = private_memory("alice");
        assert!(PrivacyEnforcer::validate_delete("bob", &mem).is_err());
    }

    // ── privacy_where_clause ──────────────────────────────────────────────────

    #[test]
    fn test_privacy_where_clause_format() {
        let clause = privacy_where_clause(1);
        assert!(clause.contains("user_id = ?1"), "Should include user_id param: {clause}");
        assert!(clause.contains("private"), "Should include private visibility: {clause}");
        assert!(clause.contains("system"), "Should include system visibility: {clause}");
    }

    #[test]
    fn test_privacy_where_clause_param_index() {
        let clause = privacy_where_clause(3);
        assert!(clause.contains("?3"), "Should use param index 3: {clause}");
    }

    // ── integration: privacy blocks cross-user ────────────────────────────────

    #[test]
    fn test_privacy_enforcer_blocks_all_cross_user_types() {
        // Health, Finance, Personal — all must reject cross-user access
        for sensitivity in [
            SensitivityCategory::Health,
            SensitivityCategory::Finance,
            SensitivityCategory::Personal,
        ] {
            let mut mem = Memory::new("alice", MemoryType::Semantic, sensitivity, "sensitive content");
            PrivacyEnforcer::enforce_sensitivity(&mut mem);
            // After enforcement, visibility must be Private
            assert_eq!(mem.visibility, Visibility::Private, "Sensitivity {:?} must be Private", mem.sensitivity);
            // And cross-user access must be denied
            assert!(PrivacyEnforcer::validate_access("bob", &mem).is_err(),
                "Bob must not access {:?} memory", mem.sensitivity);
        }
    }

    #[test]
    fn test_privacy_violation_detail_not_in_user_error() {
        let mem = private_memory("alice");
        let err = PrivacyEnforcer::validate_access("bob", &mem).unwrap_err();
        let user_msg = err.to_string();
        // The user-facing error must not contain "alice" or any identifying info
        assert!(!user_msg.contains("alice"), "User error should not leak owner: {user_msg}");
        assert!(!user_msg.contains("user-alice"), "User error should not leak owner: {user_msg}");
    }
}
