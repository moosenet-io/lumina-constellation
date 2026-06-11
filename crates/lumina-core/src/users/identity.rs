//! P2-01: Channel identity types and `UserStore` extension for channel lookups.
//!
//! A single Lumina user can be reached over multiple channels (Matrix,
//! Telegram, HTTP). This module defines the channel identity types and
//! provides the `channel_identities` table schema.
//!
//! **Important:** The `channel_identities` table lives in the same SQLCipher
//! database as the `users` table. All mutations go through `UserStore` to
//! share the same `Connection` and avoid WAL lock contention.

use crate::error::{LuminaError, Result};
use crate::users::UserStore;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

// ── Public data types ──────────────────────────────────────────────────────

/// The channel over which a message arrived.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChannelType {
    Matrix,
    Telegram,
    Http,
    /// Catch-all for future channel types.
    Other(String),
}

impl ChannelType {
    pub fn as_str(&self) -> &str {
        match self {
            ChannelType::Matrix => "matrix",
            ChannelType::Telegram => "telegram",
            ChannelType::Http => "http",
            ChannelType::Other(s) => s.as_str(),
        }
    }

    pub fn from_db_str(s: &str) -> Self {
        match s {
            "matrix" => ChannelType::Matrix,
            "telegram" => ChannelType::Telegram,
            "http" => ChannelType::Http,
            other => ChannelType::Other(other.to_string()),
        }
    }
}

impl std::fmt::Display for ChannelType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A link between a channel-specific identifier and a Lumina `user_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelIdentity {
    pub user_id: String,
    pub channel_type: ChannelType,
    /// The identifier as the channel understands it, e.g. `@alice:home.org`.
    pub channel_user_id: String,
    /// Whether the identity has been confirmed (e.g. via a verification flow).
    pub verified: bool,
}

// ── Schema helper ──────────────────────────────────────────────────────────

/// Called from `UserStore::ensure_schema` to add the `channel_identities`
/// table to the same connection. This is not public — use `UserStore`'s
/// channel methods instead.
pub(super) fn ensure_channel_schema(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS channel_identities (
            user_id         TEXT NOT NULL,
            channel_type    TEXT NOT NULL,
            channel_user_id TEXT NOT NULL,
            verified        INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (channel_type, channel_user_id)
        );
        CREATE INDEX IF NOT EXISTS idx_ci_user_id
            ON channel_identities (user_id);",
    )
    .map_err(|e| LuminaError::Config(format!("Channel identity schema creation failed: {}", e)))?;
    Ok(())
}

// ── UserStore channel-identity methods ────────────────────────────────────

impl UserStore {
    /// Link a channel identity to an existing user.
    ///
    /// If the same `channel_user_id` is already linked to the same `user_id`,
    /// the `verified` flag is updated and the identity is returned. Returns an
    /// error if the channel identity is already claimed by a different user.
    pub fn link_channel(
        &self,
        user_id: &str,
        channel_type: ChannelType,
        channel_user_id: &str,
        verified: bool,
    ) -> Result<ChannelIdentity> {
        if let Some(existing) =
            self.get_by_channel(channel_type.clone(), channel_user_id)?
        {
            if existing.user_id != user_id {
                return Err(LuminaError::Config(format!(
                    "Channel identity '{}' is already linked to a different user ({}). \
                     Unlink first.",
                    channel_user_id, existing.user_id
                )));
            }
            // Same user re-linking: update verified flag.
            self.conn()
                .execute(
                    "UPDATE channel_identities SET verified = ?1
                     WHERE channel_type = ?2 AND channel_user_id = ?3",
                    params![verified as i32, channel_type.as_str(), channel_user_id],
                )
                .map_err(|e| {
                    LuminaError::Config(format!(
                        "Failed to update verified on re-link: {}",
                        e
                    ))
                })?;
            return Ok(ChannelIdentity {
                user_id: user_id.to_string(),
                channel_type,
                channel_user_id: channel_user_id.to_string(),
                verified,
            });
        }

        self.conn()
            .execute(
                "INSERT INTO channel_identities
                     (user_id, channel_type, channel_user_id, verified)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    user_id,
                    channel_type.as_str(),
                    channel_user_id,
                    verified as i32,
                ],
            )
            .map_err(|e| {
                LuminaError::Config(format!("Failed to link channel identity: {}", e))
            })?;

        Ok(ChannelIdentity {
            user_id: user_id.to_string(),
            channel_type,
            channel_user_id: channel_user_id.to_string(),
            verified,
        })
    }

    /// Remove a channel identity link. Returns `true` if a row was removed.
    pub fn unlink_channel(
        &self,
        channel_type: ChannelType,
        channel_user_id: &str,
    ) -> Result<bool> {
        let n = self
            .conn()
            .execute(
                "DELETE FROM channel_identities
                 WHERE channel_type = ?1 AND channel_user_id = ?2",
                params![channel_type.as_str(), channel_user_id],
            )
            .map_err(|e| LuminaError::Config(format!("Failed to unlink identity: {}", e)))?;
        Ok(n > 0)
    }

    /// Mark an identity as verified.
    pub fn set_channel_verified(
        &self,
        channel_type: ChannelType,
        channel_user_id: &str,
        verified: bool,
    ) -> Result<()> {
        self.conn()
            .execute(
                "UPDATE channel_identities SET verified = ?1
                 WHERE channel_type = ?2 AND channel_user_id = ?3",
                params![verified as i32, channel_type.as_str(), channel_user_id],
            )
            .map_err(|e| LuminaError::Config(format!("Failed to set verified: {}", e)))?;
        Ok(())
    }

    /// Look up a channel identity by type and channel-specific user ID.
    pub fn get_by_channel(
        &self,
        channel_type: ChannelType,
        channel_user_id: &str,
    ) -> Result<Option<ChannelIdentity>> {
        let result = self
            .conn()
            .query_row(
                "SELECT user_id, channel_type, channel_user_id, verified
                 FROM channel_identities
                 WHERE channel_type = ?1 AND channel_user_id = ?2",
                params![channel_type.as_str(), channel_user_id],
                row_to_identity,
            )
            .optional()
            .map_err(|e| LuminaError::Config(format!("get_by_channel failed: {}", e)))?;
        Ok(result)
    }

    /// Return all channel identities for a given user.
    pub fn list_channels_for_user(&self, user_id: &str) -> Result<Vec<ChannelIdentity>> {
        let mut stmt = self
            .conn()
            .prepare(
                "SELECT user_id, channel_type, channel_user_id, verified
                 FROM channel_identities WHERE user_id = ?1",
            )
            .map_err(|e| {
                LuminaError::Config(format!("list_channels_for_user prepare failed: {}", e))
            })?;

        let rows = stmt
            .query_map(params![user_id], row_to_identity)
            .map_err(|e| {
                LuminaError::Config(format!("list_channels_for_user query failed: {}", e))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| {
                LuminaError::Config(format!("list_channels_for_user row error: {}", e))
            })?;

        Ok(rows)
    }
}

fn row_to_identity(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChannelIdentity> {
    let ct_str: String = row.get(1)?;
    let verified_int: i32 = row.get(3)?;
    Ok(ChannelIdentity {
        user_id: row.get(0)?,
        channel_type: ChannelType::from_db_str(&ct_str),
        channel_user_id: row.get(2)?,
        verified: verified_int != 0,
    })
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::users::{UserRole, UserStore};
    use std::path::PathBuf;

    fn test_key() -> Vec<u8> {
        vec![11u8; 32]
    }

    fn tmp_db(name: &str) -> PathBuf {
        let p = PathBuf::from(format!("/tmp/lumina_identity_test_{}.db", name));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn open_store(name: &str) -> UserStore {
        UserStore::new(&tmp_db(name), &test_key()).expect("open UserStore for identity test")
    }

    #[test]
    fn test_link_and_lookup() {
        let store = open_store("link_lookup");
        store.create_user("Alice", None, UserRole::Admin).unwrap();
        let user = store.list().unwrap().into_iter().next().unwrap();

        store
            .link_channel(&user.user_id, ChannelType::Matrix, "@alice:home.org", true)
            .unwrap();

        let found = store
            .get_by_channel(ChannelType::Matrix, "@alice:home.org")
            .unwrap()
            .unwrap();
        assert_eq!(found.user_id, user.user_id);
        assert!(found.verified);
        let _ = std::fs::remove_file(tmp_db("link_lookup"));
    }

    #[test]
    fn test_link_same_identity_twice_updates_verified() {
        let store = open_store("link_idempotent");
        store.create_user("Alice", None, UserRole::Admin).unwrap();
        let user = store.list().unwrap().into_iter().next().unwrap();

        store
            .link_channel(&user.user_id, ChannelType::Matrix, "@alice:home.org", false)
            .unwrap();
        // Re-link with verified = true.
        let result = store
            .link_channel(&user.user_id, ChannelType::Matrix, "@alice:home.org", true)
            .unwrap();
        assert!(result.verified, "verified flag should be updated on re-link");

        // Confirm the database reflects the update.
        let found = store
            .get_by_channel(ChannelType::Matrix, "@alice:home.org")
            .unwrap()
            .unwrap();
        assert!(found.verified);
        let _ = std::fs::remove_file(tmp_db("link_idempotent"));
    }

    #[test]
    fn test_link_different_user_rejected() {
        let store = open_store("link_conflict");
        store.create_user("Alice", None, UserRole::Admin).unwrap();
        store.create_user("Bob", None, UserRole::Member).unwrap();
        let users = store.list().unwrap();

        store
            .link_channel(&users[0].user_id, ChannelType::Matrix, "@alice:home.org", false)
            .unwrap();
        let result =
            store.link_channel(&users[1].user_id, ChannelType::Matrix, "@alice:home.org", false);
        assert!(result.is_err(), "Duplicate channel identity should fail");
        let _ = std::fs::remove_file(tmp_db("link_conflict"));
    }

    #[test]
    fn test_unlink() {
        let store = open_store("unlink");
        store.create_user("Alice", None, UserRole::Admin).unwrap();
        let user = store.list().unwrap().into_iter().next().unwrap();

        store
            .link_channel(&user.user_id, ChannelType::Telegram, "123456789", false)
            .unwrap();
        let removed = store.unlink_channel(ChannelType::Telegram, "123456789").unwrap();
        assert!(removed);
        let found = store
            .get_by_channel(ChannelType::Telegram, "123456789")
            .unwrap();
        assert!(found.is_none());
        let _ = std::fs::remove_file(tmp_db("unlink"));
    }

    #[test]
    fn test_unlink_nonexistent_returns_false() {
        let store = open_store("unlink_none");
        let removed = store
            .unlink_channel(ChannelType::Matrix, "@nobody:home.org")
            .unwrap();
        assert!(!removed);
        let _ = std::fs::remove_file(tmp_db("unlink_none"));
    }

    #[test]
    fn test_list_channels_for_user_multiple_channels() {
        let store = open_store("list_channels");
        store.create_user("Multi", None, UserRole::Admin).unwrap();
        let user = store.list().unwrap().into_iter().next().unwrap();

        store
            .link_channel(&user.user_id, ChannelType::Matrix, "@multi:home.org", true)
            .unwrap();
        store
            .link_channel(&user.user_id, ChannelType::Telegram, "999", false)
            .unwrap();

        let identities = store.list_channels_for_user(&user.user_id).unwrap();
        assert_eq!(identities.len(), 2);
        let _ = std::fs::remove_file(tmp_db("list_channels"));
    }

    #[test]
    fn test_set_channel_verified() {
        let store = open_store("set_verified");
        store.create_user("Eve", None, UserRole::Admin).unwrap();
        let user = store.list().unwrap().into_iter().next().unwrap();

        store
            .link_channel(&user.user_id, ChannelType::Http, "token-abc", false)
            .unwrap();
        store
            .set_channel_verified(ChannelType::Http, "token-abc", true)
            .unwrap();
        let found = store
            .get_by_channel(ChannelType::Http, "token-abc")
            .unwrap()
            .unwrap();
        assert!(found.verified);
        let _ = std::fs::remove_file(tmp_db("set_verified"));
    }

    #[test]
    fn test_channel_type_display_and_from() {
        let types = [
            ChannelType::Matrix,
            ChannelType::Telegram,
            ChannelType::Http,
            ChannelType::Other("custom".to_string()),
        ];
        for ct in &types {
            let s = ct.to_string();
            let back = ChannelType::from_db_str(&s);
            assert_eq!(ct, &back);
        }
    }
}
