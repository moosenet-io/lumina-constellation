//! DPROMPT-08 — Raw conversation archival for reconstruction ground truth.
//!
//! Stores complete conversation episodes (full transcript JSON, metadata, topics)
//! in a SQLCipher-encrypted `conversation_archive` table. These archives are the
//! GROUND TRUTH that knowledge-digest and personality-vector reconstruction read
//! from during sleep-time — extracted memories are derived, archives are canonical.
//!
//! Storage model:
//! - Append-only; episodes are never deleted.
//! - Per-user isolation: every query filters by `user_id`.
//! - Encrypted at rest via SQLCipher (ENGRAM_DB_KEY from vault).
//! - Old / large transcripts are gzip-compressed in the BLOB column (`compressed`
//!   flag), transparently decompressed on read.
//!
//! Timestamps are Unix seconds (i64) supplied by the caller — no chrono dependency.

use crate::error::{LuminaError, Result};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::Path;

/// One archived conversation turn. Captures user messages and Lumina responses
/// (NOT tool intermediates — those live in OperationalStore).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivedMessage {
    pub role: String,
    pub content: String,
}

impl ArchivedMessage {
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
        }
    }
}

/// A stored conversation episode with its decoded transcript and metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivedEpisode {
    pub session_id: String,
    pub user_id: String,
    pub transcript: Vec<ArchivedMessage>,
    pub turn_count: i64,
    pub started_at: i64,
    pub ended_at: i64,
    pub topics: Vec<String>,
}

/// Sampling strategies for reconstruction reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleStrategy {
    /// Last N sessions by `ended_at` — for active context.
    Recent,
    /// Sample weighted by `turn_count * recency` — for knowledge digest.
    Weighted,
    /// Every session for the user — for full personality-vector reconstruction.
    All,
}

// ── ConversationArchive ─────────────────────────────────────────────────────

/// SQLCipher-backed append-only archive of raw conversation episodes.
///
/// Open with `open(path, key)` where `key` is 32 raw bytes from
/// `vault::VaultStore::load().get("ENGRAM_DB_KEY")` (helper:
/// `crate::engram::engram_key()`). Key is never read directly from env in prod.
pub struct ConversationArchive {
    conn: Connection,
}

impl ConversationArchive {
    /// Open (or create) the SQLCipher archive at `db_path` with the raw 32-byte `key`.
    ///
    /// Mirrors `ConversationStore::open` / `EngramStore::open`: sets the PRAGMA key,
    /// verifies the key by touching `sqlite_master`, enables WAL, creates the schema.
    pub fn open(db_path: &Path, key: &[u8]) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                LuminaError::Config(format!("Cannot create archive dir: {e}"))
            })?;
        }

        let conn = Connection::open(db_path)
            .map_err(|e| LuminaError::Config(format!("Cannot open archive store: {e}")))?;

        let hex_key = hex::encode(key);
        conn.execute_batch(&format!("PRAGMA key = \"x'{hex_key}'\";"))
            .map_err(|e| LuminaError::Config(format!("Failed to set archive key: {e}")))?;

        conn.execute_batch("SELECT count(*) FROM sqlite_master;")
            .map_err(|_| {
                LuminaError::SecurityViolation(
                    "Archive store key is incorrect — cannot open database".to_string(),
                )
            })?;

        conn.execute_batch("PRAGMA journal_mode = WAL;")
            .map_err(|e| LuminaError::Config(format!("WAL mode failed: {e}")))?;

        let store = Self { conn };
        store.ensure_schema()?;
        Ok(store)
    }

    fn ensure_schema(&self) -> Result<()> {
        self.conn
            .execute_batch(
                "
            CREATE TABLE IF NOT EXISTS conversation_archive (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id  TEXT    NOT NULL,
                user_id     TEXT    NOT NULL,
                transcript  BLOB    NOT NULL,
                compressed  INTEGER NOT NULL DEFAULT 0,
                turn_count  INTEGER NOT NULL,
                started_at  INTEGER NOT NULL,
                ended_at    INTEGER NOT NULL,
                topics      TEXT    NOT NULL DEFAULT '[]'
            );
            CREATE INDEX IF NOT EXISTS idx_archive_user_ended
                ON conversation_archive (user_id, ended_at);
        ",
            )
            .map_err(|e| LuminaError::Config(format!("Archive schema creation failed: {e}")))?;
        Ok(())
    }

    /// Store one complete conversation episode.
    ///
    /// The transcript is serialized to a JSON array and stored as a BLOB.
    /// `turn_count` is derived as `transcript.len()`. `topics` is stored as a JSON
    /// array. Transcripts that are large (many turns) are gzip-compressed eagerly;
    /// otherwise they are stored as raw JSON and may be compressed later via
    /// `compress_old`. Append-only — every call inserts a new row.
    pub fn store_episode(
        &self,
        session_id: &str,
        user_id: &str,
        transcript: &[ArchivedMessage],
        started_at: i64,
        ended_at: i64,
        topics: &[String],
    ) -> Result<()> {
        let json = serde_json::to_vec(transcript)
            .map_err(|e| LuminaError::Config(format!("transcript serialize failed: {e}")))?;
        let topics_json = serde_json::to_string(topics)
            .map_err(|e| LuminaError::Config(format!("topics serialize failed: {e}")))?;

        // Eagerly compress large transcripts (>1000 turns) to keep storage bounded.
        let (blob, compressed): (Vec<u8>, i64) = if transcript.len() > LARGE_TURN_THRESHOLD {
            (gzip_compress(&json)?, 1)
        } else {
            (json, 0)
        };

        self.conn
            .execute(
                "INSERT INTO conversation_archive
                 (session_id, user_id, transcript, compressed, turn_count, started_at, ended_at, topics)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    session_id,
                    user_id,
                    blob,
                    compressed,
                    transcript.len() as i64,
                    started_at,
                    ended_at,
                    topics_json,
                ],
            )
            .map_err(|e| LuminaError::Config(format!("store_episode failed: {e}")))?;
        Ok(())
    }

    /// Return the most recent `limit` episodes for `user_id`, newest first.
    ///
    /// Per-user isolation: filtered by `user_id`. Transcripts are transparently
    /// decompressed if stored compressed.
    pub fn get_episodes(&self, user_id: &str, limit: usize) -> Result<Vec<ArchivedEpisode>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT session_id, user_id, transcript, compressed, turn_count,
                        started_at, ended_at, topics
                 FROM conversation_archive
                 WHERE user_id = ?1
                 ORDER BY ended_at DESC, id DESC
                 LIMIT ?2",
            )
            .map_err(|e| LuminaError::Config(format!("get_episodes prepare failed: {e}")))?;

        let rows = stmt
            .query_map(params![user_id, limit as i64], row_to_raw)
            .map_err(|e| LuminaError::Config(format!("get_episodes query failed: {e}")))?;

        collect_episodes(rows)
    }

    /// Sample episodes for reconstruction according to `strategy`.
    ///
    /// - `Recent` → last `count` sessions by `ended_at` (active context).
    /// - `Weighted` → ordered by `turn_count * recency` (knowledge digest); recency
    ///   is the row's `ended_at` relative to the user's newest/oldest span.
    /// - `All` → every session for the user (`count` is ignored), oldest first
    ///   (chronological — for full personality-vector reconstruction).
    ///
    /// Per-user isolation: every branch filters by `user_id`.
    pub fn get_sample(
        &self,
        user_id: &str,
        count: usize,
        strategy: SampleStrategy,
    ) -> Result<Vec<ArchivedEpisode>> {
        match strategy {
            SampleStrategy::Recent => self.get_episodes(user_id, count),
            SampleStrategy::All => {
                let mut stmt = self
                    .conn
                    .prepare(
                        "SELECT session_id, user_id, transcript, compressed, turn_count,
                                started_at, ended_at, topics
                         FROM conversation_archive
                         WHERE user_id = ?1
                         ORDER BY ended_at ASC, id ASC",
                    )
                    .map_err(|e| LuminaError::Config(format!("get_sample(all) prepare: {e}")))?;
                let rows = stmt
                    .query_map(params![user_id], row_to_raw)
                    .map_err(|e| LuminaError::Config(format!("get_sample(all) query: {e}")))?;
                collect_episodes(rows)
            }
            SampleStrategy::Weighted => {
                // weight = turn_count * recency, where recency normalizes ended_at
                // into [1, 2] across the user's span so newer + longer sessions rank
                // highest. COALESCE guards a single-row (zero-span) corpus.
                let mut stmt = self
                    .conn
                    .prepare(
                        "SELECT session_id, user_id, transcript, compressed, turn_count,
                                started_at, ended_at, topics
                         FROM conversation_archive
                         WHERE user_id = ?1
                         ORDER BY turn_count * (
                             1.0 + CAST(ended_at - (SELECT MIN(ended_at) FROM conversation_archive WHERE user_id = ?1) AS REAL)
                                 / NULLIF((SELECT MAX(ended_at) - MIN(ended_at) FROM conversation_archive WHERE user_id = ?1), 0)
                         ) DESC, ended_at DESC, id DESC
                         LIMIT ?2",
                    )
                    .map_err(|e| LuminaError::Config(format!("get_sample(weighted) prepare: {e}")))?;
                let rows = stmt
                    .query_map(params![user_id, count as i64], row_to_raw)
                    .map_err(|e| LuminaError::Config(format!("get_sample(weighted) query: {e}")))?;
                collect_episodes(rows)
            }
        }
    }

    /// Gzip-compress the transcripts of all not-yet-compressed episodes whose
    /// `ended_at` is older than (`now`-relative) `older_than_secs` boundary.
    ///
    /// Callers pass an absolute Unix-second cutoff: rows with `ended_at < cutoff`
    /// are compressed. Returns the number of rows compressed. Idempotent — already
    /// compressed rows are skipped.
    pub fn compress_old(&self, cutoff_ended_at: i64) -> Result<usize> {
        // Collect candidate (id, transcript) pairs first to avoid mutating while iterating.
        let candidates: Vec<(i64, Vec<u8>)> = {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT id, transcript FROM conversation_archive
                     WHERE compressed = 0 AND ended_at < ?1",
                )
                .map_err(|e| LuminaError::Config(format!("compress_old prepare: {e}")))?;
            let rows = stmt
                .query_map(params![cutoff_ended_at], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
                })
                .map_err(|e| LuminaError::Config(format!("compress_old query: {e}")))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| LuminaError::Config(format!("compress_old row: {e}")))?
        };

        let mut n = 0usize;
        for (id, raw) in candidates {
            let gz = gzip_compress(&raw)?;
            self.conn
                .execute(
                    "UPDATE conversation_archive SET transcript = ?1, compressed = 1 WHERE id = ?2",
                    params![gz, id],
                )
                .map_err(|e| LuminaError::Config(format!("compress_old update: {e}")))?;
            n += 1;
        }
        Ok(n)
    }
}

/// Turns above this length are compressed eagerly on store. Matches the spec's
/// "conversations >1000 turns" guidance.
const LARGE_TURN_THRESHOLD: usize = 1000;

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Raw row as read from the database (transcript still possibly compressed).
struct RawRow {
    session_id: String,
    user_id: String,
    transcript_blob: Vec<u8>,
    compressed: bool,
    turn_count: i64,
    started_at: i64,
    ended_at: i64,
    topics_json: String,
}

fn row_to_raw(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawRow> {
    Ok(RawRow {
        session_id: row.get(0)?,
        user_id: row.get(1)?,
        transcript_blob: row.get(2)?,
        compressed: row.get::<_, i64>(3)? != 0,
        turn_count: row.get(4)?,
        started_at: row.get(5)?,
        ended_at: row.get(6)?,
        topics_json: row.get(7)?,
    })
}

fn collect_episodes(
    rows: impl Iterator<Item = rusqlite::Result<RawRow>>,
) -> Result<Vec<ArchivedEpisode>> {
    let mut out = Vec::new();
    for row in rows {
        let raw = row.map_err(|e| LuminaError::Config(format!("archive row read: {e}")))?;
        let json = if raw.compressed {
            gzip_decompress(&raw.transcript_blob)?
        } else {
            raw.transcript_blob
        };
        let transcript: Vec<ArchivedMessage> = serde_json::from_slice(&json)
            .map_err(|e| LuminaError::Config(format!("transcript deserialize failed: {e}")))?;
        let topics: Vec<String> = serde_json::from_str(&raw.topics_json).unwrap_or_default();
        out.push(ArchivedEpisode {
            session_id: raw.session_id,
            user_id: raw.user_id,
            transcript,
            turn_count: raw.turn_count,
            started_at: raw.started_at,
            ended_at: raw.ended_at,
            topics,
        });
    }
    Ok(out)
}

fn gzip_compress(data: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(data)
        .map_err(|e| LuminaError::Config(format!("gzip write failed: {e}")))?;
    encoder
        .finish()
        .map_err(|e| LuminaError::Config(format!("gzip finish failed: {e}")))
}

fn gzip_decompress(data: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = GzDecoder::new(data);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|e| LuminaError::Config(format!("gzip decompress failed: {e}")))?;
    Ok(out)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_key() -> Vec<u8> {
        vec![0u8; 32]
    }

    fn msgs(n: usize) -> Vec<ArchivedMessage> {
        (0..n)
            .map(|i| {
                if i % 2 == 0 {
                    ArchivedMessage::new("user", format!("u{i}"))
                } else {
                    ArchivedMessage::new("assistant", format!("a{i}"))
                }
            })
            .collect()
    }

    #[test]
    fn test_store_and_read_roundtrip() {
        let dir = tempdir().unwrap();
        let arc = ConversationArchive::open(&dir.path().join("arc.db"), &test_key()).unwrap();

        let t = vec![
            ArchivedMessage::new("user", "hello"),
            ArchivedMessage::new("assistant", "hi there"),
        ];
        arc.store_episode("s1", "alice", &t, 1000, 1100, &["greeting".to_string()])
            .unwrap();

        let eps = arc.get_episodes("alice", 10).unwrap();
        assert_eq!(eps.len(), 1);
        let ep = &eps[0];
        assert_eq!(ep.session_id, "s1");
        assert_eq!(ep.user_id, "alice");
        assert_eq!(ep.turn_count, 2);
        assert_eq!(ep.started_at, 1000);
        assert_eq!(ep.ended_at, 1100);
        assert_eq!(ep.topics, vec!["greeting".to_string()]);
        assert_eq!(ep.transcript, t);
    }

    #[test]
    fn test_single_turn_session_archived() {
        let dir = tempdir().unwrap();
        let arc = ConversationArchive::open(&dir.path().join("arc.db"), &test_key()).unwrap();
        arc.store_episode("solo", "alice", &msgs(1), 5, 6, &[]).unwrap();
        let eps = arc.get_episodes("alice", 10).unwrap();
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].turn_count, 1);
    }

    #[test]
    fn test_get_episodes_recent_order_and_limit() {
        let dir = tempdir().unwrap();
        let arc = ConversationArchive::open(&dir.path().join("arc.db"), &test_key()).unwrap();
        for i in 0..5 {
            let ended = 1000 + i * 10;
            arc.store_episode(&format!("s{i}"), "alice", &msgs(2), 0, ended, &[])
                .unwrap();
        }
        let eps = arc.get_episodes("alice", 3).unwrap();
        assert_eq!(eps.len(), 3);
        // newest first
        assert_eq!(eps[0].session_id, "s4");
        assert_eq!(eps[1].session_id, "s3");
        assert_eq!(eps[2].session_id, "s2");
    }

    #[test]
    fn test_get_sample_recent() {
        let dir = tempdir().unwrap();
        let arc = ConversationArchive::open(&dir.path().join("arc.db"), &test_key()).unwrap();
        for i in 0..4 {
            arc.store_episode(&format!("s{i}"), "alice", &msgs(2), 0, 1000 + i, &[])
                .unwrap();
        }
        let eps = arc.get_sample("alice", 2, SampleStrategy::Recent).unwrap();
        assert_eq!(eps.len(), 2);
        assert_eq!(eps[0].session_id, "s3");
        assert_eq!(eps[1].session_id, "s2");
    }

    #[test]
    fn test_get_sample_all_returns_everything_chronological() {
        let dir = tempdir().unwrap();
        let arc = ConversationArchive::open(&dir.path().join("arc.db"), &test_key()).unwrap();
        for i in 0..4 {
            arc.store_episode(&format!("s{i}"), "alice", &msgs(2), 0, 1000 + i, &[])
                .unwrap();
        }
        // count ignored for All
        let eps = arc.get_sample("alice", 1, SampleStrategy::All).unwrap();
        assert_eq!(eps.len(), 4);
        assert_eq!(eps[0].session_id, "s0");
        assert_eq!(eps[3].session_id, "s3");
    }

    #[test]
    fn test_get_sample_weighted_favors_long_recent() {
        let dir = tempdir().unwrap();
        let arc = ConversationArchive::open(&dir.path().join("arc.db"), &test_key()).unwrap();
        // short+old, long+recent, medium+middle
        arc.store_episode("old_short", "alice", &msgs(2), 0, 1000, &[]).unwrap();
        arc.store_episode("mid", "alice", &msgs(10), 0, 1500, &[]).unwrap();
        arc.store_episode("recent_long", "alice", &msgs(40), 0, 2000, &[]).unwrap();

        let eps = arc.get_sample("alice", 1, SampleStrategy::Weighted).unwrap();
        assert_eq!(eps.len(), 1);
        // longest + most recent should win
        assert_eq!(eps[0].session_id, "recent_long");
    }

    #[test]
    fn test_get_sample_weighted_single_row_no_panic() {
        let dir = tempdir().unwrap();
        let arc = ConversationArchive::open(&dir.path().join("arc.db"), &test_key()).unwrap();
        arc.store_episode("only", "alice", &msgs(3), 0, 1000, &[]).unwrap();
        // Zero-span corpus must not divide-by-zero.
        let eps = arc.get_sample("alice", 5, SampleStrategy::Weighted).unwrap();
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].session_id, "only");
    }

    #[test]
    fn test_compression_roundtrip() {
        let dir = tempdir().unwrap();
        let arc = ConversationArchive::open(&dir.path().join("arc.db"), &test_key()).unwrap();
        let t = msgs(50);
        arc.store_episode("s1", "alice", &t, 0, 500, &["topic".to_string()])
            .unwrap();

        // Compress everything ended before cutoff 1000.
        let compressed = arc.compress_old(1000).unwrap();
        assert_eq!(compressed, 1);

        // Idempotent: nothing left to compress.
        assert_eq!(arc.compress_old(1000).unwrap(), 0);

        // Transparent decompression on read yields the identical transcript.
        let eps = arc.get_episodes("alice", 10).unwrap();
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].transcript, t);
        assert_eq!(eps[0].topics, vec!["topic".to_string()]);
    }

    #[test]
    fn test_compress_old_respects_cutoff() {
        let dir = tempdir().unwrap();
        let arc = ConversationArchive::open(&dir.path().join("arc.db"), &test_key()).unwrap();
        arc.store_episode("old", "alice", &msgs(4), 0, 100, &[]).unwrap();
        arc.store_episode("new", "alice", &msgs(4), 0, 5000, &[]).unwrap();
        // Only the old one is before the cutoff.
        assert_eq!(arc.compress_old(1000).unwrap(), 1);
        // Both still read back correctly.
        let eps = arc.get_episodes("alice", 10).unwrap();
        assert_eq!(eps.len(), 2);
    }

    #[test]
    fn test_per_user_isolation() {
        let dir = tempdir().unwrap();
        let arc = ConversationArchive::open(&dir.path().join("arc.db"), &test_key()).unwrap();

        arc.store_episode("a1", "alice", &msgs(2), 0, 100, &[]).unwrap();
        arc.store_episode("b1", "bob", &msgs(2), 0, 100, &[]).unwrap();

        // Alice cannot see Bob's episodes and vice versa.
        let alice = arc.get_episodes("alice", 100).unwrap();
        assert_eq!(alice.len(), 1);
        assert_eq!(alice[0].session_id, "a1");

        let bob = arc.get_episodes("bob", 100).unwrap();
        assert_eq!(bob.len(), 1);
        assert_eq!(bob[0].session_id, "b1");

        // Every sampling strategy is also isolated.
        for s in [SampleStrategy::Recent, SampleStrategy::Weighted, SampleStrategy::All] {
            let got = arc.get_sample("alice", 100, s).unwrap();
            assert!(got.iter().all(|e| e.user_id == "alice"));
            assert_eq!(got.len(), 1);
        }
    }

    #[test]
    fn test_survives_reopen_persistence() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("arc.db");
        {
            let arc = ConversationArchive::open(&path, &test_key()).unwrap();
            arc.store_episode("s1", "alice", &msgs(6), 10, 20, &["x".to_string()])
                .unwrap();
            arc.compress_old(1000).unwrap_or(0); // exercise both paths across reopen
        }
        // Reopen with the same key — data persists, transcript still decodes.
        let arc2 = ConversationArchive::open(&path, &test_key()).unwrap();
        let eps = arc2.get_episodes("alice", 10).unwrap();
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].session_id, "s1");
        assert_eq!(eps[0].transcript, msgs(6));
    }

    #[test]
    fn test_wrong_key_returns_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("arc.db");
        ConversationArchive::open(&path, &test_key()).unwrap();
        let bad = vec![0xFFu8; 32];
        assert!(ConversationArchive::open(&path, &bad).is_err());
    }

    #[test]
    fn test_empty_user_returns_empty() {
        let dir = tempdir().unwrap();
        let arc = ConversationArchive::open(&dir.path().join("arc.db"), &test_key()).unwrap();
        assert!(arc.get_episodes("ghost", 10).unwrap().is_empty());
        assert!(arc
            .get_sample("ghost", 10, SampleStrategy::Weighted)
            .unwrap()
            .is_empty());
    }
}
