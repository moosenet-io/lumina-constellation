//! P2-12: Shared household resources — SharedResource model, membership, access control.
//!
//! A SharedResource is a named resource (Calendar, List, or Memory) that is
//! owned by one user and visible/editable by a set of member users.
//!
//! ## Design
//!
//! - Owner: created the resource, can delete it or remove members.
//! - Members: can read and add items to the resource (read+write access).
//! - Non-members: no access — attempting to query returns `LuminaError::SecurityViolation`.
//!
//! ## Storage
//!
//! Two tables in the same SQLCipher database as UserStore (passed in as a
//! `rusqlite::Connection`):
//!
//! ```text
//! shared_resources (id, resource_type, name, owner_user_id, created_at)
//! shared_resource_members (resource_id, user_id)
//! ```
//!
//! ## Access rules (enforced in Rust — not just SQL)
//!
//! | Operation              | Owner | Member | Non-member |
//! |------------------------|-------|--------|------------|
//! | query (read)           | yes   | yes    | blocked    |
//! | add item / contribute  | yes   | yes    | blocked    |
//! | add member             | yes   | no     | blocked    |
//! | remove member          | yes   | no     | blocked    |
//! | delete resource        | yes   | no     | blocked    |

use crate::error::{LuminaError, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

// ── ResourceType ──────────────────────────────────────────────────────────────

/// The kind of shared resource.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourceType {
    /// A shared CalDAV calendar (additional URL for all members to query).
    Calendar,
    /// A shared list (grocery list, todo list, etc.) — stored as engram facts.
    List,
    /// A shared memory entry — engram facts visible to all members.
    Memory,
}

impl ResourceType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ResourceType::Calendar => "calendar",
            ResourceType::List => "list",
            ResourceType::Memory => "memory",
        }
    }

    pub fn from_db_str(s: &str) -> Self {
        match s {
            "calendar" => ResourceType::Calendar,
            "list" => ResourceType::List,
            _ => ResourceType::Memory,
        }
    }
}

impl std::fmt::Display for ResourceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ── SharedResource ────────────────────────────────────────────────────────────

/// A shared household resource definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedResource {
    /// Stable UUID for the resource.
    pub id: String,
    /// What kind of resource this is.
    pub resource_type: ResourceType,
    /// Human-readable name, e.g. "Family Calendar".
    pub name: String,
    /// The user who owns and administers this resource.
    pub owner_user_id: String,
    /// ISO-8601 UTC timestamp when the resource was created.
    pub created_at: String,
    /// Users (besides owner) who are members.
    pub member_user_ids: Vec<String>,
}

impl SharedResource {
    /// Return true if `user_id` is the owner.
    pub fn is_owner(&self, user_id: &str) -> bool {
        self.owner_user_id == user_id
    }

    /// Return true if `user_id` has access (owner or member).
    pub fn is_accessible_by(&self, user_id: &str) -> bool {
        self.owner_user_id == user_id || self.member_user_ids.iter().any(|m| m == user_id)
    }
}

// ── SharedResourceStore ───────────────────────────────────────────────────────

/// Store for shared household resources.
///
/// Uses the same SQLCipher database connection as `UserStore`. Schema is
/// created on first use via `SharedResourceStore::ensure_schema()`.
pub struct SharedResourceStore<'a> {
    conn: &'a Connection,
}

impl<'a> SharedResourceStore<'a> {
    /// Wrap an existing open connection (e.g. from `UserStore::conn()`).
    ///
    /// Applies the schema on every construction — `CREATE TABLE IF NOT EXISTS`
    /// makes this idempotent.
    pub fn new(conn: &'a Connection) -> Result<Self> {
        let store = Self { conn };
        store.ensure_schema()?;
        Ok(store)
    }

    // ── Schema ────────────────────────────────────────────────────────────────

    fn ensure_schema(&self) -> Result<()> {
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS shared_resources (
                    id              TEXT PRIMARY KEY,
                    resource_type   TEXT NOT NULL,
                    name            TEXT NOT NULL,
                    owner_user_id   TEXT NOT NULL,
                    created_at      TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_shared_resources_owner
                    ON shared_resources (owner_user_id);

                CREATE TABLE IF NOT EXISTS shared_resource_members (
                    resource_id  TEXT NOT NULL,
                    user_id      TEXT NOT NULL,
                    PRIMARY KEY (resource_id, user_id)
                );
                CREATE INDEX IF NOT EXISTS idx_shared_resource_members_user
                    ON shared_resource_members (user_id);",
            )
            .map_err(|e| {
                LuminaError::Config(format!("SharedResource schema creation failed: {}", e))
            })?;
        Ok(())
    }

    // ── Write operations ──────────────────────────────────────────────────────

    /// Create a new shared resource owned by `owner_user_id`.
    ///
    /// `name` is truncated to 200 characters. Initial membership list is
    /// empty (owner has implicit access — they don't appear in the members
    /// table).
    pub fn create(
        &self,
        resource_type: ResourceType,
        name: &str,
        owner_user_id: &str,
    ) -> Result<SharedResource> {
        let id = uuid::Uuid::new_v4().to_string();
        let created_at = utc_now_simple();
        let name = truncate(name, 200);

        self.conn
            .execute(
                "INSERT INTO shared_resources (id, resource_type, name, owner_user_id, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, resource_type.as_str(), name, owner_user_id, created_at],
            )
            .map_err(|e| {
                LuminaError::Config(format!("Failed to create shared resource: {}", e))
            })?;

        Ok(SharedResource {
            id,
            resource_type,
            name,
            owner_user_id: owner_user_id.to_string(),
            created_at,
            member_user_ids: vec![],
        })
    }

    /// Add `member_user_id` to the resource's membership list.
    ///
    /// Only the owner may add members — returns `SecurityViolation` if
    /// `requesting_user_id` is not the owner.
    ///
    /// No-op (returns `Ok`) if the user is already a member.
    pub fn add_member(
        &self,
        resource_id: &str,
        requesting_user_id: &str,
        member_user_id: &str,
    ) -> Result<()> {
        self.require_owner(resource_id, requesting_user_id)?;

        self.conn
            .execute(
                "INSERT OR IGNORE INTO shared_resource_members (resource_id, user_id)
                 VALUES (?1, ?2)",
                params![resource_id, member_user_id],
            )
            .map_err(|e| LuminaError::Config(format!("add_member failed: {}", e)))?;
        Ok(())
    }

    /// Remove `member_user_id` from the resource's membership list.
    ///
    /// Only the owner may remove members. The owner cannot remove themselves
    /// via this method (they're not in the members table).
    pub fn remove_member(
        &self,
        resource_id: &str,
        requesting_user_id: &str,
        member_user_id: &str,
    ) -> Result<()> {
        self.require_owner(resource_id, requesting_user_id)?;

        self.conn
            .execute(
                "DELETE FROM shared_resource_members WHERE resource_id = ?1 AND user_id = ?2",
                params![resource_id, member_user_id],
            )
            .map_err(|e| LuminaError::Config(format!("remove_member failed: {}", e)))?;
        Ok(())
    }

    /// Delete the resource and all its membership rows.
    ///
    /// Only the owner may delete the resource.
    pub fn delete(&self, resource_id: &str, requesting_user_id: &str) -> Result<bool> {
        self.require_owner(resource_id, requesting_user_id)?;

        // Remove membership rows first (SQLite doesn't cascade without pragma).
        self.conn
            .execute(
                "DELETE FROM shared_resource_members WHERE resource_id = ?1",
                params![resource_id],
            )
            .map_err(|e| LuminaError::Config(format!("delete members failed: {}", e)))?;

        let n = self
            .conn
            .execute(
                "DELETE FROM shared_resources WHERE id = ?1",
                params![resource_id],
            )
            .map_err(|e| LuminaError::Config(format!("delete resource failed: {}", e)))?;

        Ok(n > 0)
    }

    // ── Read operations ───────────────────────────────────────────────────────

    /// Fetch a single resource by ID, checking that `requesting_user_id` has access.
    ///
    /// Returns `None` if the resource does not exist.
    /// Returns `SecurityViolation` if the resource exists but the user is not
    /// the owner or a member.
    pub fn get(
        &self,
        resource_id: &str,
        requesting_user_id: &str,
    ) -> Result<Option<SharedResource>> {
        let resource = self.get_unchecked(resource_id)?;
        match resource {
            None => Ok(None),
            Some(r) => {
                if r.is_accessible_by(requesting_user_id) {
                    Ok(Some(r))
                } else {
                    Err(LuminaError::SecurityViolation(format!(
                        "User '{}' does not have access to shared resource '{}'",
                        requesting_user_id, resource_id
                    )))
                }
            }
        }
    }

    /// List all shared resources accessible by `user_id` (owner or member).
    pub fn list_for_user(&self, user_id: &str) -> Result<Vec<SharedResource>> {
        // Resources where user is owner
        let mut ids: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id FROM shared_resources WHERE owner_user_id = ?1")
                .map_err(|e| LuminaError::Config(format!("list_for_user prepare 1 failed: {}", e)))?;
            let x = stmt.query_map(params![user_id], |r| r.get(0))
                .map_err(|e| LuminaError::Config(format!("list_for_user query 1 failed: {}", e)))?
                .collect::<std::result::Result<Vec<String>, _>>()
                .map_err(|e| LuminaError::Config(format!("list_for_user row 1 failed: {}", e)))?;
            x
        };

        // Resources where user is a member
        let member_ids: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT resource_id FROM shared_resource_members WHERE user_id = ?1")
                .map_err(|e| LuminaError::Config(format!("list_for_user prepare 2 failed: {}", e)))?;
            let x = stmt.query_map(params![user_id], |r| r.get(0))
                .map_err(|e| LuminaError::Config(format!("list_for_user query 2 failed: {}", e)))?
                .collect::<std::result::Result<Vec<String>, _>>()
                .map_err(|e| LuminaError::Config(format!("list_for_user row 2 failed: {}", e)))?;
            x
        };

        // Merge and deduplicate
        for id in member_ids {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }

        let mut resources = Vec::new();
        for id in &ids {
            if let Some(r) = self.get_unchecked(id)? {
                resources.push(r);
            }
        }

        // Sort by created_at for deterministic output
        resources.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(resources)
    }

    /// List all shared Calendar resources accessible by `user_id`.
    ///
    /// Used by the calendar integration to find additional CalDAV URLs to query.
    pub fn list_shared_calendars_for_user(
        &self,
        user_id: &str,
    ) -> Result<Vec<SharedResource>> {
        let all = self.list_for_user(user_id)?;
        Ok(all
            .into_iter()
            .filter(|r| r.resource_type == ResourceType::Calendar)
            .collect())
    }

    /// List all shared Memory resources accessible by `user_id`.
    pub fn list_shared_memories_for_user(&self, user_id: &str) -> Result<Vec<SharedResource>> {
        let all = self.list_for_user(user_id)?;
        Ok(all
            .into_iter()
            .filter(|r| r.resource_type == ResourceType::Memory)
            .collect())
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    /// Fetch a resource by ID without access check, including its member list.
    fn get_unchecked(&self, resource_id: &str) -> Result<Option<SharedResource>> {
        let row = self
            .conn
            .query_row(
                "SELECT id, resource_type, name, owner_user_id, created_at
                 FROM shared_resources WHERE id = ?1",
                params![resource_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| LuminaError::Config(format!("get_unchecked failed: {}", e)))?;

        match row {
            None => Ok(None),
            Some((id, rtype_str, name, owner, created_at)) => {
                let member_user_ids = self.get_members(&id)?;
                Ok(Some(SharedResource {
                    id,
                    resource_type: ResourceType::from_db_str(&rtype_str),
                    name,
                    owner_user_id: owner,
                    created_at,
                    member_user_ids,
                }))
            }
        }
    }

    /// Fetch member user IDs for a resource.
    fn get_members(&self, resource_id: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT user_id FROM shared_resource_members WHERE resource_id = ?1 ORDER BY user_id ASC",
            )
            .map_err(|e| LuminaError::Config(format!("get_members prepare failed: {}", e)))?;

        let rows = stmt
            .query_map(params![resource_id], |r| r.get(0))
            .map_err(|e| LuminaError::Config(format!("get_members query failed: {}", e)))?
            .collect::<std::result::Result<Vec<String>, _>>()
            .map_err(|e| LuminaError::Config(format!("get_members row error: {}", e)))?;

        // Force collection into owned Vec before stmt is dropped
        Ok(rows.into_iter().collect())
    }

    /// Check that `user_id` is the owner of `resource_id`, return `SecurityViolation` otherwise.
    fn require_owner(&self, resource_id: &str, user_id: &str) -> Result<()> {
        let resource = self
            .get_unchecked(resource_id)?
            .ok_or_else(|| LuminaError::Config(format!("Resource '{}' not found", resource_id)))?;

        if !resource.is_owner(user_id) {
            return Err(LuminaError::SecurityViolation(format!(
                "User '{}' is not the owner of resource '{}' — only the owner can perform this operation",
                user_id, resource_id
            )));
        }
        Ok(())
    }
}

// ── Calendar integration ──────────────────────────────────────────────────────

/// Merge two lists of `CalendarEvent`s and sort chronologically by `dtstart`.
///
/// This is the pure merge step for P2-12 calendar integration. The caller
/// fetches events from personal and shared calendars; this function merges them.
///
/// Events are sorted by the `dtstart` string lexicographically (ISO 8601 / iCal
/// compact format sorts correctly as strings: `YYYYMMDD` and `YYYYMMDDTHHMMSSZ`
/// both sort in chronological order).
pub fn merge_events_chronological(
    mut personal: Vec<crate::caldav::CalendarEvent>,
    shared: Vec<crate::caldav::CalendarEvent>,
) -> Vec<crate::caldav::CalendarEvent> {
    personal.extend(shared);
    personal.sort_by(|a, b| a.dtstart.cmp(&b.dtstart));
    personal
}

// ── Shared memory helpers ─────────────────────────────────────────────────────

/// Tag a memory entry as shared by prefixing the text with `[SHARED:<resource_id>] `.
///
/// This is the convention for shared memories stored in the engram:
/// all members of the resource can search for and retrieve these entries.
pub fn tag_shared_memory(resource_id: &str, text: &str) -> String {
    format!("[SHARED:{}] {}", resource_id, text)
}

/// Strip the shared tag prefix from a memory entry, returning the raw text.
///
/// Returns `None` if the entry does not have the expected tag format.
pub fn untag_shared_memory<'a>(resource_id: &str, tagged: &'a str) -> Option<&'a str> {
    let prefix = format!("[SHARED:{}] ", resource_id);
    tagged.strip_prefix(&prefix)
}

/// Check whether a memory entry is tagged as shared for any resource.
pub fn is_shared_memory(text: &str) -> bool {
    text.starts_with("[SHARED:")
}

// ── Private helpers ───────────────────────────────────────────────────────────

fn utc_now_simple() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let sec = secs % 60;
    let min = (secs / 60) % 60;
    let hour = (secs / 3600) % 24;
    let days = secs / 86400;
    let (year, month, day) = days_to_ymd(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hour, min, sec
    )
}

fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    let mut year = 1970u64;
    loop {
        let year_days = if is_leap(year) { 366 } else { 365 };
        if days < year_days {
            break;
        }
        days -= year_days;
        year += 1;
    }
    let month_days: [u64; 12] = [
        31,
        if is_leap(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1u64;
    for &md in &month_days {
        if days < md {
            break;
        }
        days -= md;
        month += 1;
    }
    (year, month, days + 1)
}

fn is_leap(year: u64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn truncate(s: &str, max_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() > max_chars {
        chars[..max_chars].iter().collect()
    } else {
        s.to_string()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Open an in-memory SQLite connection (no SQLCipher in unit tests).
    fn test_conn() -> Connection {
        Connection::open_in_memory().expect("open in-memory connection")
    }

    fn make_store(conn: &Connection) -> SharedResourceStore<'_> {
        SharedResourceStore::new(conn).expect("create SharedResourceStore")
    }

    // ── ResourceType ───────────────────────────────────────────────────────────

    #[test]
    fn test_resource_type_round_trip() {
        for rt in [ResourceType::Calendar, ResourceType::List, ResourceType::Memory] {
            let s = rt.as_str();
            let back = ResourceType::from_db_str(s);
            assert_eq!(rt, back, "round-trip failed for {:?}", rt);
        }
    }

    #[test]
    fn test_resource_type_display() {
        assert_eq!(ResourceType::Calendar.to_string(), "calendar");
        assert_eq!(ResourceType::List.to_string(), "list");
        assert_eq!(ResourceType::Memory.to_string(), "memory");
    }

    // ── Create shared resource ─────────────────────────────────────────────────

    #[test]
    fn test_create_shared_resource() {
        let conn = test_conn();
        let store = make_store(&conn);

        let resource = store
            .create(ResourceType::Calendar, "Family Calendar", "user-alice")
            .unwrap();

        assert!(!resource.id.is_empty(), "id should be set");
        assert_eq!(resource.resource_type, ResourceType::Calendar);
        assert_eq!(resource.name, "Family Calendar");
        assert_eq!(resource.owner_user_id, "user-alice");
        assert!(resource.member_user_ids.is_empty(), "no members initially");
    }

    #[test]
    fn test_create_name_truncated_to_200_chars() {
        let conn = test_conn();
        let store = make_store(&conn);
        let long_name: String = "A".repeat(300);
        let resource = store
            .create(ResourceType::List, &long_name, "user-alice")
            .unwrap();
        assert_eq!(resource.name.chars().count(), 200);
    }

    // ── Add/remove members ─────────────────────────────────────────────────────

    #[test]
    fn test_add_member_by_owner() {
        let conn = test_conn();
        let store = make_store(&conn);

        let resource = store
            .create(ResourceType::Calendar, "Family Calendar", "user-alice")
            .unwrap();

        store
            .add_member(&resource.id, "user-alice", "user-bob")
            .unwrap();

        let fetched = store.get(&resource.id, "user-alice").unwrap().unwrap();
        assert!(
            fetched.member_user_ids.contains(&"user-bob".to_string()),
            "Bob should be a member"
        );
    }

    #[test]
    fn test_add_member_by_non_owner_rejected() {
        let conn = test_conn();
        let store = make_store(&conn);

        let resource = store
            .create(ResourceType::Calendar, "Family Calendar", "user-alice")
            .unwrap();

        // Bob is NOT the owner
        let result = store.add_member(&resource.id, "user-bob", "user-carol");
        assert!(
            result.is_err(),
            "Non-owner should not be able to add members"
        );
        assert!(matches!(result, Err(LuminaError::SecurityViolation(_))));
    }

    #[test]
    fn test_remove_member_by_owner() {
        let conn = test_conn();
        let store = make_store(&conn);

        let resource = store
            .create(ResourceType::Calendar, "Family Calendar", "user-alice")
            .unwrap();

        store
            .add_member(&resource.id, "user-alice", "user-bob")
            .unwrap();

        // Now remove Bob
        store
            .remove_member(&resource.id, "user-alice", "user-bob")
            .unwrap();

        let fetched = store.get(&resource.id, "user-alice").unwrap().unwrap();
        assert!(
            !fetched.member_user_ids.contains(&"user-bob".to_string()),
            "Bob should no longer be a member"
        );
    }

    #[test]
    fn test_remove_member_by_non_owner_rejected() {
        let conn = test_conn();
        let store = make_store(&conn);

        let resource = store
            .create(ResourceType::Calendar, "Family Calendar", "user-alice")
            .unwrap();
        store
            .add_member(&resource.id, "user-alice", "user-bob")
            .unwrap();

        // Carol is NOT the owner
        let result = store.remove_member(&resource.id, "user-carol", "user-bob");
        assert!(result.is_err());
        assert!(matches!(result, Err(LuminaError::SecurityViolation(_))));
    }

    // ── Member can query shared resource ──────────────────────────────────────

    #[test]
    fn test_member_can_query_shared_calendar() {
        let conn = test_conn();
        let store = make_store(&conn);

        let resource = store
            .create(ResourceType::Calendar, "Family Calendar", "user-alice")
            .unwrap();

        store
            .add_member(&resource.id, "user-alice", "user-bob")
            .unwrap();

        // Bob (member) can access the resource
        let fetched = store.get(&resource.id, "user-bob").unwrap();
        assert!(fetched.is_some(), "Member should be able to query shared calendar");
    }

    // ── Non-member cannot access shared resource ──────────────────────────────

    #[test]
    fn test_non_member_cannot_access_shared_resource() {
        let conn = test_conn();
        let store = make_store(&conn);

        let resource = store
            .create(ResourceType::Calendar, "Family Calendar", "user-alice")
            .unwrap();

        // Carol is NOT a member
        let result = store.get(&resource.id, "user-carol");
        assert!(
            result.is_err(),
            "Non-member should not be able to access shared resource"
        );
        assert!(matches!(result, Err(LuminaError::SecurityViolation(_))));
    }

    // ── Delete resource ────────────────────────────────────────────────────────

    #[test]
    fn test_delete_by_owner() {
        let conn = test_conn();
        let store = make_store(&conn);

        let resource = store
            .create(ResourceType::List, "Grocery List", "user-alice")
            .unwrap();
        store
            .add_member(&resource.id, "user-alice", "user-bob")
            .unwrap();

        let deleted = store.delete(&resource.id, "user-alice").unwrap();
        assert!(deleted, "delete should return true");

        // Resource no longer exists
        let fetched = store.get(&resource.id, "user-alice").unwrap();
        assert!(fetched.is_none(), "Resource should be gone after deletion");
    }

    #[test]
    fn test_delete_by_non_owner_rejected() {
        let conn = test_conn();
        let store = make_store(&conn);

        let resource = store
            .create(ResourceType::List, "Grocery List", "user-alice")
            .unwrap();

        let result = store.delete(&resource.id, "user-bob");
        assert!(result.is_err());
        assert!(matches!(result, Err(LuminaError::SecurityViolation(_))));
    }

    // ── Shared memory visible to all members ──────────────────────────────────

    #[test]
    fn test_shared_memory_visible_to_members() {
        let conn = test_conn();
        let store = make_store(&conn);

        let resource = store
            .create(ResourceType::Memory, "Household Notes", "user-alice")
            .unwrap();
        store
            .add_member(&resource.id, "user-alice", "user-bob")
            .unwrap();

        // Both owner and member can access the resource
        let alice_view = store.get(&resource.id, "user-alice").unwrap();
        let bob_view = store.get(&resource.id, "user-bob").unwrap();

        assert!(alice_view.is_some(), "Owner should see shared memory resource");
        assert!(bob_view.is_some(), "Member should see shared memory resource");
        assert_eq!(
            alice_view.unwrap().id,
            bob_view.unwrap().id,
            "Both should see the same resource"
        );
    }

    // ── list_for_user ──────────────────────────────────────────────────────────

    #[test]
    fn test_list_for_user_includes_owned_and_member_resources() {
        let conn = test_conn();
        let store = make_store(&conn);

        // Alice creates a calendar (she owns it)
        let cal = store
            .create(ResourceType::Calendar, "Alice's Calendar", "user-alice")
            .unwrap();

        // Bob creates a grocery list and adds Alice as a member
        let list = store
            .create(ResourceType::List, "Grocery List", "user-bob")
            .unwrap();
        store
            .add_member(&list.id, "user-bob", "user-alice")
            .unwrap();

        let alice_resources = store.list_for_user("user-alice").unwrap();
        assert_eq!(alice_resources.len(), 2, "Alice should see both resources");

        let ids: Vec<&str> = alice_resources.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&cal.id.as_str()), "Alice's owned calendar should appear");
        assert!(ids.contains(&list.id.as_str()), "Grocery list (member) should appear");
    }

    #[test]
    fn test_list_for_user_excludes_others() {
        let conn = test_conn();
        let store = make_store(&conn);

        // Alice creates a resource; Carol is not involved
        store
            .create(ResourceType::Calendar, "Private Calendar", "user-alice")
            .unwrap();

        let carol_resources = store.list_for_user("user-carol").unwrap();
        assert!(
            carol_resources.is_empty(),
            "Carol should not see Alice's private resource"
        );
    }

    // ── Shared calendar/memory type filters ───────────────────────────────────

    #[test]
    fn test_list_shared_calendars_filters_type() {
        let conn = test_conn();
        let store = make_store(&conn);

        store
            .create(ResourceType::Calendar, "Family Calendar", "user-alice")
            .unwrap();
        store
            .create(ResourceType::List, "Grocery List", "user-alice")
            .unwrap();

        let calendars = store.list_shared_calendars_for_user("user-alice").unwrap();
        assert_eq!(calendars.len(), 1);
        assert_eq!(calendars[0].resource_type, ResourceType::Calendar);
    }

    #[test]
    fn test_list_shared_memories_filters_type() {
        let conn = test_conn();
        let store = make_store(&conn);

        store
            .create(ResourceType::Memory, "Notes", "user-alice")
            .unwrap();
        store
            .create(ResourceType::Calendar, "Calendar", "user-alice")
            .unwrap();

        let memories = store.list_shared_memories_for_user("user-alice").unwrap();
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].resource_type, ResourceType::Memory);
    }

    // ── SharedResource helper methods ─────────────────────────────────────────

    #[test]
    fn test_is_owner() {
        let r = SharedResource {
            id: "r1".to_string(),
            resource_type: ResourceType::Calendar,
            name: "Test".to_string(),
            owner_user_id: "user-alice".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            member_user_ids: vec!["user-bob".to_string()],
        };
        assert!(r.is_owner("user-alice"));
        assert!(!r.is_owner("user-bob"));
        assert!(!r.is_owner("user-carol"));
    }

    #[test]
    fn test_is_accessible_by() {
        let r = SharedResource {
            id: "r1".to_string(),
            resource_type: ResourceType::Calendar,
            name: "Test".to_string(),
            owner_user_id: "user-alice".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            member_user_ids: vec!["user-bob".to_string()],
        };
        assert!(r.is_accessible_by("user-alice"), "owner has access");
        assert!(r.is_accessible_by("user-bob"), "member has access");
        assert!(!r.is_accessible_by("user-carol"), "non-member denied");
    }

    // ── merge_events_chronological ────────────────────────────────────────────

    #[test]
    fn test_merge_events_chronological_ordering() {
        use crate::caldav::CalendarEvent;

        let personal = vec![
            CalendarEvent {
                uid: "p1".to_string(),
                summary: "Personal 9am".to_string(),
                dtstart: "20260601T090000Z".to_string(),
                dtend: "20260601T100000Z".to_string(),
                description: None,
                location: None,
            },
            CalendarEvent {
                uid: "p2".to_string(),
                summary: "Personal 2pm".to_string(),
                dtstart: "20260601T140000Z".to_string(),
                dtend: "20260601T150000Z".to_string(),
                description: None,
                location: None,
            },
        ];

        let shared = vec![CalendarEvent {
            uid: "s1".to_string(),
            summary: "Family Lunch".to_string(),
            dtstart: "20260601T120000Z".to_string(),
            dtend: "20260601T130000Z".to_string(),
            description: None,
            location: None,
        }];

        let merged = merge_events_chronological(personal, shared);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].uid, "p1", "9am should be first");
        assert_eq!(merged[1].uid, "s1", "12pm (shared) should be second");
        assert_eq!(merged[2].uid, "p2", "2pm should be last");
    }

    #[test]
    fn test_merge_events_empty_shared() {
        use crate::caldav::CalendarEvent;

        let personal = vec![CalendarEvent {
            uid: "p1".to_string(),
            summary: "Personal".to_string(),
            dtstart: "20260601T090000Z".to_string(),
            dtend: "20260601T100000Z".to_string(),
            description: None,
            location: None,
        }];

        let merged = merge_events_chronological(personal, vec![]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].uid, "p1");
    }

    #[test]
    fn test_merge_events_empty_personal() {
        use crate::caldav::CalendarEvent;

        let shared = vec![CalendarEvent {
            uid: "s1".to_string(),
            summary: "Shared".to_string(),
            dtstart: "20260601T090000Z".to_string(),
            dtend: "20260601T100000Z".to_string(),
            description: None,
            location: None,
        }];

        let merged = merge_events_chronological(vec![], shared);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].uid, "s1");
    }

    // ── Shared memory tagging helpers ─────────────────────────────────────────

    #[test]
    fn test_tag_shared_memory() {
        let tagged = tag_shared_memory("resource-123", "Grocery list: milk, eggs, bread");
        assert_eq!(tagged, "[SHARED:resource-123] Grocery list: milk, eggs, bread");
    }

    #[test]
    fn test_untag_shared_memory_valid() {
        let tagged = "[SHARED:resource-123] Grocery list: milk";
        let text = untag_shared_memory("resource-123", tagged);
        assert_eq!(text, Some("Grocery list: milk"));
    }

    #[test]
    fn test_untag_shared_memory_wrong_resource() {
        let tagged = "[SHARED:resource-123] Grocery list: milk";
        let text = untag_shared_memory("resource-456", tagged);
        assert!(text.is_none(), "Wrong resource ID should return None");
    }

    #[test]
    fn test_untag_shared_memory_not_tagged() {
        let plain = "Just a plain memory";
        let text = untag_shared_memory("resource-123", plain);
        assert!(text.is_none());
    }

    #[test]
    fn test_is_shared_memory() {
        assert!(is_shared_memory("[SHARED:r1] some memory"));
        assert!(!is_shared_memory("plain memory"));
        assert!(!is_shared_memory(""));
    }

    // ── Owner removed: resource becomes orphaned (get returns SecurityViolation
    //    for former member since owner no longer matches a real user, but the
    //    resource is still readable by remaining members and admin can reassign) ──

    #[test]
    fn test_user_removed_from_resource_loses_access() {
        let conn = test_conn();
        let store = make_store(&conn);

        let resource = store
            .create(ResourceType::Calendar, "Family Calendar", "user-alice")
            .unwrap();
        store
            .add_member(&resource.id, "user-alice", "user-bob")
            .unwrap();

        // Alice removes Bob
        store
            .remove_member(&resource.id, "user-alice", "user-bob")
            .unwrap();

        // Bob should no longer have access
        let result = store.get(&resource.id, "user-bob");
        assert!(
            result.is_err(),
            "Removed member should lose access to the resource"
        );
    }

    // ── Add member idempotent ─────────────────────────────────────────────────

    #[test]
    fn test_add_member_idempotent() {
        let conn = test_conn();
        let store = make_store(&conn);

        let resource = store
            .create(ResourceType::Calendar, "Family Calendar", "user-alice")
            .unwrap();

        // Add Bob twice — should not error or duplicate
        store
            .add_member(&resource.id, "user-alice", "user-bob")
            .unwrap();
        store
            .add_member(&resource.id, "user-alice", "user-bob")
            .unwrap();

        let fetched = store.get(&resource.id, "user-alice").unwrap().unwrap();
        let bob_count = fetched
            .member_user_ids
            .iter()
            .filter(|m| *m == "user-bob")
            .count();
        assert_eq!(bob_count, 1, "Bob should appear exactly once in members");
    }

    // ── Schema idempotency ────────────────────────────────────────────────────

    #[test]
    fn test_schema_idempotent() {
        // Creating two stores from the same connection should not fail.
        let conn = test_conn();
        let _s1 = SharedResourceStore::new(&conn).unwrap();
        let _s2 = SharedResourceStore::new(&conn).unwrap();
    }
}
