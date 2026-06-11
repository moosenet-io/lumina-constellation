//! P2-01: Per-role permission sets.
//!
//! Defines the baseline capability each `UserRole` grants. Granular per-user
//! tool overrides are deferred to P2-03 (`user_tools` table). This module
//! provides the role-level policy consulted when no per-user override exists.

use crate::users::UserRole;
use serde::{Deserialize, Serialize};

// ── Permission primitives ──────────────────────────────────────────────────

/// A single named capability.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Permission {
    /// Read own conversation history.
    ReadOwnHistory,
    /// Read any user's conversation history (admin only).
    ReadAllHistory,
    /// Execute MCP tool calls.
    ExecuteTools,
    /// Write to Engram memory.
    WriteMemory,
    /// Read own Engram memories.
    ReadOwnMemory,
    /// Manage user accounts (create, promote, disable).
    ManageUsers,
    /// Access admin-only dashboard views.
    AdminDashboard,
}

/// The set of permissions granted to a role.
#[derive(Debug, Clone)]
pub struct PermissionSet {
    pub role: UserRole,
    permissions: Vec<Permission>,
}

impl PermissionSet {
    /// Return the baseline `PermissionSet` for a given role.
    pub fn for_role(role: UserRole) -> Self {
        let permissions = match &role {
            UserRole::Admin => vec![
                Permission::ReadOwnHistory,
                Permission::ReadAllHistory,
                Permission::ExecuteTools,
                Permission::WriteMemory,
                Permission::ReadOwnMemory,
                Permission::ManageUsers,
                Permission::AdminDashboard,
            ],
            UserRole::Member => vec![
                Permission::ReadOwnHistory,
                Permission::ExecuteTools,
                Permission::WriteMemory,
                Permission::ReadOwnMemory,
            ],
            UserRole::Guest => vec![
                Permission::ReadOwnHistory,
                Permission::ReadOwnMemory,
                // Guests get NO tools and NO memory writes.
            ],
        };
        Self { role, permissions }
    }

    /// Return `true` if this permission set includes `perm`.
    pub fn has(&self, perm: &Permission) -> bool {
        self.permissions.contains(perm)
    }

    /// Iterate over all granted permissions.
    pub fn iter(&self) -> impl Iterator<Item = &Permission> {
        self.permissions.iter()
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_admin_has_all_permissions() {
        let ps = PermissionSet::for_role(UserRole::Admin);
        assert!(ps.has(&Permission::ReadAllHistory));
        assert!(ps.has(&Permission::ManageUsers));
        assert!(ps.has(&Permission::AdminDashboard));
        assert!(ps.has(&Permission::ExecuteTools));
        assert!(ps.has(&Permission::WriteMemory));
    }

    #[test]
    fn test_member_has_scoped_permissions() {
        let ps = PermissionSet::for_role(UserRole::Member);
        assert!(ps.has(&Permission::ReadOwnHistory));
        assert!(ps.has(&Permission::ExecuteTools));
        assert!(ps.has(&Permission::WriteMemory));
        // Not admin capabilities.
        assert!(!ps.has(&Permission::ReadAllHistory));
        assert!(!ps.has(&Permission::ManageUsers));
        assert!(!ps.has(&Permission::AdminDashboard));
    }

    #[test]
    fn test_guest_has_no_tools_no_memory_writes() {
        let ps = PermissionSet::for_role(UserRole::Guest);
        assert!(!ps.has(&Permission::ExecuteTools));
        assert!(!ps.has(&Permission::WriteMemory));
        assert!(!ps.has(&Permission::ManageUsers));
        // Guests can still read their own history.
        assert!(ps.has(&Permission::ReadOwnHistory));
    }

    #[test]
    fn test_role_hierarchy_via_permissions() {
        let admin = PermissionSet::for_role(UserRole::Admin);
        let member = PermissionSet::for_role(UserRole::Member);
        let guest = PermissionSet::for_role(UserRole::Guest);

        // Admin has strictly more permissions than Member.
        let admin_count = admin.iter().count();
        let member_count = member.iter().count();
        let guest_count = guest.iter().count();

        assert!(
            admin_count > member_count,
            "Admin should have more permissions than Member"
        );
        assert!(
            member_count > guest_count,
            "Member should have more permissions than Guest"
        );
    }
}
