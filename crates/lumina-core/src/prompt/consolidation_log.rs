//! DPROMPT-07: Consolidation audit log.
//!
//! Every sleep-time consolidation run (nightly, weekly, or immediate) appends a
//! single JSON line to a consolidation log so the system keeps a full audit
//! trail of *what* was reconstructed, *when*, *what changed*, and *what failed*.
//! The [`super::sleep_time::SleepTimeConsolidator`] writes one [`ConsolidationEntry`]
//! per run; the Soma dashboard (DPROMPT-10) reads the last N entries back.
//!
//! ## Design
//! * **JSON-lines** (one object per line) so appends are atomic-ish and the file
//!   can grow without rewriting; reading the last N is a cheap tail.
//! * **No chrono** — the timestamp (`ts_secs`, Unix seconds) is supplied by the
//!   caller, keeping the type deterministic and unit-testable.
//! * **Non-fatal** — a corrupt line is skipped on read, never panics.
//! * The log path is caller-supplied (per-user or global), so the type carries
//!   no infrastructure assumptions.

use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use crate::error::Result;

/// Which consolidation cycle produced an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsolidationKind {
    /// 4am daily: trait adjustments + knowledge-digest reconstruction.
    Nightly,
    /// 3am Sunday: personality vector + reflexa + principles.
    Weekly,
    /// Out-of-band: knowledge-digest reconstruction only.
    Immediate,
}

/// One audit record for a single consolidation run.
///
/// Per the spec: timestamp, layers updated, trait changes, digest length,
/// errors. Optional fields are omitted from JSON when absent so old entries
/// stay readable as the schema grows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConsolidationEntry {
    /// Unix-seconds timestamp of the run (caller-supplied).
    pub ts_secs: i64,
    /// Which cycle this was.
    pub kind: ConsolidationKind,
    /// The user this consolidation was for.
    pub user_id: String,
    /// Names of the prompt layers updated (e.g. `["style", "knowledge"]`).
    #[serde(default)]
    pub layers_updated: Vec<String>,
    /// Human-readable trait change summary (before→after), when traits moved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trait_changes: Option<String>,
    /// Length (in characters) of the reconstructed knowledge digest, when one
    /// was produced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest_len: Option<usize>,
    /// Errors encountered during the run (non-fatal steps that were skipped).
    #[serde(default)]
    pub errors: Vec<String>,
    /// Wall-clock duration of the run in milliseconds, when measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

impl ConsolidationEntry {
    /// Start a new entry for `kind`/`user_id` at `ts_secs`.
    pub fn new(kind: ConsolidationKind, user_id: impl Into<String>, ts_secs: i64) -> Self {
        ConsolidationEntry {
            ts_secs,
            kind,
            user_id: user_id.into(),
            layers_updated: Vec::new(),
            trait_changes: None,
            digest_len: None,
            errors: Vec::new(),
            duration_ms: None,
        }
    }

    /// Record that a layer was updated (idempotent on name).
    pub fn add_layer(&mut self, layer: impl Into<String>) {
        let layer = layer.into();
        if !self.layers_updated.iter().any(|l| l == &layer) {
            self.layers_updated.push(layer);
        }
    }

    /// Record a non-fatal error string.
    pub fn add_error(&mut self, err: impl Into<String>) {
        self.errors.push(err.into());
    }

    /// Whether any error was recorded.
    pub fn had_errors(&self) -> bool {
        !self.errors.is_empty()
    }
}

/// Append-only JSON-lines consolidation log at a caller-supplied path.
#[derive(Debug, Clone)]
pub struct ConsolidationLog {
    path: PathBuf,
}

impl ConsolidationLog {
    /// Open (lazily — no I/O here) a log at `path`. The file and parent
    /// directory are created on first [`append`](Self::append).
    pub fn at(path: impl Into<PathBuf>) -> Self {
        ConsolidationLog { path: path.into() }
    }

    /// The log file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one entry as a single JSON line. Creates parent dirs/file.
    pub fn append(&self, entry: &ConsolidationEntry) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let line = serde_json::to_string(entry)
            .map_err(|e| crate::error::LuminaError::Internal(format!("serialize log entry: {e}")))?;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(f, "{line}")?;
        Ok(())
    }

    /// Read the most recent `n` entries (oldest-first within the returned
    /// window). Corrupt lines are skipped. A missing file returns empty.
    pub fn read_last(&self, n: usize) -> Vec<ConsolidationEntry> {
        let mut all = self.read_all();
        if all.len() > n {
            all.drain(0..all.len() - n);
        }
        all
    }

    /// Read every (well-formed) entry, oldest-first. Missing file → empty.
    pub fn read_all(&self) -> Vec<ConsolidationEntry> {
        let file = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(_) => return Vec::new(),
        };
        let reader = std::io::BufReader::new(file);
        let mut out = Vec::new();
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => continue,
            };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<ConsolidationEntry>(&line) {
                Ok(e) => out.push(e),
                Err(e) => log::warn!("skipping corrupt consolidation-log line: {e}"),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn log_path(dir: &Path) -> PathBuf {
        dir.join("nested").join("consolidation.log")
    }

    #[test]
    fn append_then_read_roundtrip() {
        let dir = tempdir().unwrap();
        let log = ConsolidationLog::at(log_path(dir.path()));
        let mut e = ConsolidationEntry::new(ConsolidationKind::Nightly, "operator", 1_700_000_000);
        e.add_layer("style");
        e.add_layer("knowledge");
        e.trait_changes = Some("humor 0.65→0.67".into());
        e.digest_len = Some(1234);
        log.append(&e).unwrap();

        let back = log.read_all();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0], e);
        assert_eq!(back[0].kind, ConsolidationKind::Nightly);
        assert_eq!(back[0].layers_updated, vec!["style", "knowledge"]);
        assert_eq!(back[0].digest_len, Some(1234));
    }

    #[test]
    fn append_is_additive_and_creates_parent_dirs() {
        let dir = tempdir().unwrap();
        let log = ConsolidationLog::at(log_path(dir.path()));
        for i in 0..3 {
            log.append(&ConsolidationEntry::new(
                ConsolidationKind::Immediate,
                "operator",
                1_700_000_000 + i,
            ))
            .unwrap();
        }
        assert!(log.path().exists());
        let all = log.read_all();
        assert_eq!(all.len(), 3);
        // Oldest-first ordering preserved.
        assert_eq!(all[0].ts_secs, 1_700_000_000);
        assert_eq!(all[2].ts_secs, 1_700_000_002);
    }

    #[test]
    fn read_last_returns_tail_oldest_first() {
        let dir = tempdir().unwrap();
        let log = ConsolidationLog::at(log_path(dir.path()));
        for i in 0..10 {
            log.append(&ConsolidationEntry::new(ConsolidationKind::Nightly, "u", i))
                .unwrap();
        }
        let last3 = log.read_last(3);
        assert_eq!(last3.len(), 3);
        assert_eq!(last3.iter().map(|e| e.ts_secs).collect::<Vec<_>>(), vec![7, 8, 9]);
    }

    #[test]
    fn read_last_when_fewer_than_n() {
        let dir = tempdir().unwrap();
        let log = ConsolidationLog::at(log_path(dir.path()));
        log.append(&ConsolidationEntry::new(ConsolidationKind::Weekly, "u", 5)).unwrap();
        assert_eq!(log.read_last(50).len(), 1);
    }

    #[test]
    fn missing_file_reads_empty() {
        let dir = tempdir().unwrap();
        let log = ConsolidationLog::at(dir.path().join("does-not-exist.log"));
        assert!(log.read_all().is_empty());
        assert!(log.read_last(10).is_empty());
    }

    #[test]
    fn corrupt_lines_are_skipped() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("c.log");
        let good = serde_json::to_string(&ConsolidationEntry::new(
            ConsolidationKind::Nightly,
            "u",
            42,
        ))
        .unwrap();
        std::fs::write(&path, format!("{good}\nNOT JSON{{\n\n{good}\n")).unwrap();
        let log = ConsolidationLog::at(path);
        let all = log.read_all();
        assert_eq!(all.len(), 2, "two good lines survive, corrupt skipped");
    }

    #[test]
    fn errors_recorded_and_flagged() {
        let mut e = ConsolidationEntry::new(ConsolidationKind::Nightly, "u", 1);
        assert!(!e.had_errors());
        e.add_error("vram swap failed");
        assert!(e.had_errors());
        assert_eq!(e.errors, vec!["vram swap failed"]);
    }
}
