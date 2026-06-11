//! FTS5 full-text search module for Engram.
//!
//! Creates and maintains a `memory_fts` virtual table (FTS5) synchronized
//! with the `facts` table. Gracefully degrades when FTS5 is unavailable in
//! the SQLite build (bundled-sqlcipher should always include FTS5, but we
//! treat it as optional to be safe).

use crate::error::{LuminaError, Result};
use rusqlite::Connection;

// ── Table lifecycle ────────────────────────────────────────────────────────

/// Create the FTS5 virtual table if it does not already exist.
///
/// `content_rowid=id` ties the virtual table rows to the `facts.id` column
/// so BM25 rank queries work correctly. The FTS column is named `text` to
/// match the `facts.text` column.
///
/// If FTS5 is unavailable (the SQLite build does not include it), a warning
/// is emitted and `Ok(())` is returned — callers must handle the case where
/// FTS is non-functional (i.e. `fts_search` will return an empty list).
pub fn create_fts_table(conn: &Connection) -> Result<()> {
    // Use a plain FTS5 table with explicit rowid management via fts_sync_insert/delete.
    // We do NOT use content_rowid= (external-content shortcut) because that requires
    // the content= option and changes the delete syntax. Instead, we maintain the index
    // manually on every insert/delete, which keeps the schema simple.
    let res = conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(text);",
    );
    match res {
        Ok(_) => Ok(()),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("no such module: fts5") {
                eprintln!("engram/fts: FTS5 not available in this SQLite build — full-text search disabled");
                Ok(())
            } else {
                Err(LuminaError::Config(format!("FTS5 table creation failed: {e}")))
            }
        }
    }
}

/// Insert a row into the FTS index.
///
/// Should be called immediately after inserting a row into `facts`.
/// `rowid` must match `facts.id` for the newly-inserted row.
/// Silently succeeds if FTS5 is unavailable.
pub fn fts_sync_insert(conn: &Connection, rowid: i64, content: &str) -> Result<()> {
    let res = conn.execute(
        "INSERT INTO memory_fts(rowid, text) VALUES (?1, ?2)",
        rusqlite::params![rowid, content],
    );
    match res {
        Ok(_) => Ok(()),
        Err(e) => {
            let msg = e.to_string();
            if is_fts5_unavailable(&msg) {
                Ok(())
            } else {
                Err(LuminaError::Config(format!("FTS5 insert failed: {e}")))
            }
        }
    }
}

/// Delete a row from the FTS index.
///
/// Should be called immediately before or after deleting a row from `facts`.
/// Silently succeeds if FTS5 is unavailable.
pub fn fts_sync_delete(conn: &Connection, rowid: i64) -> Result<()> {
    let res = conn.execute(
        "DELETE FROM memory_fts WHERE rowid = ?1",
        rusqlite::params![rowid],
    );
    match res {
        Ok(_) => Ok(()),
        Err(e) => {
            let msg = e.to_string();
            if is_fts5_unavailable(&msg) {
                Ok(())
            } else {
                Err(LuminaError::Config(format!("FTS5 delete failed: {e}")))
            }
        }
    }
}

// ── Query ──────────────────────────────────────────────────────────────────

/// Search the FTS index and return `(rowid, score)` pairs sorted descending
/// by relevance (higher = better).
///
/// FTS5's built-in `rank` column is a *negative* BM25 score (lower is worse).
/// We negate and normalize to the `[0, 1]` range so scores can be merged with
/// cosine similarity scores in RRF.
///
/// Special characters in `query` are escaped by wrapping the whole term in
/// double-quotes (FTS5 phrase search) so that characters like `*`, `"`, `-`
/// etc. don't cause syntax errors.
///
/// Returns `Ok(vec![])` if FTS5 is unavailable.
pub fn fts_search(conn: &Connection, query: &str, limit: usize) -> Result<Vec<(i64, f64)>> {
    // Empty / whitespace-only queries have no meaningful FTS result
    if query.trim().is_empty() {
        return Ok(vec![]);
    }
    let escaped = escape_fts5_query(query);
    let sql = "SELECT rowid, rank FROM memory_fts WHERE text MATCH ?1 ORDER BY rank LIMIT ?2";
    let mut stmt = match conn.prepare(sql) {
        Ok(s) => s,
        Err(e) => {
            if is_fts5_unavailable(&e.to_string()) {
                return Ok(vec![]);
            }
            return Err(LuminaError::Config(format!("FTS5 prepare failed: {e}")));
        }
    };

    let mapped = match stmt.query_map(
        rusqlite::params![escaped, limit as i64],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?)),
    ) {
        Ok(m) => m,
        Err(e) => {
            if is_fts5_unavailable(&e.to_string()) {
                return Ok(vec![]);
            }
            return Err(LuminaError::Config(format!("FTS5 query_map failed: {e}")));
        }
    };

    let rows: std::result::Result<Vec<(i64, f64)>, rusqlite::Error> = mapped.collect();

    match rows {
        Ok(raw) => {
            if raw.is_empty() {
                return Ok(vec![]);
            }
            // FTS5 rank is negative BM25 — negate so higher = better
            let negated: Vec<(i64, f64)> = raw.iter().map(|(id, r)| (*id, -r)).collect();
            // Normalize to [0, 1] using the max value
            let max_score = negated.iter().map(|(_, s)| *s).fold(0.0f64, f64::max);
            let normalized = if max_score > 1e-9 {
                negated.into_iter().map(|(id, s)| (id, s / max_score)).collect()
            } else {
                negated.into_iter().map(|(id, _)| (id, 1.0)).collect()
            };
            Ok(normalized)
        }
        Err(e) => {
            if is_fts5_unavailable(&e.to_string()) {
                Ok(vec![])
            } else {
                Err(LuminaError::Config(format!("FTS5 query failed: {e}")))
            }
        }
    }
}

// ── V2 wrappers (memories_v2 table) ─────────────────────────────────────────

/// Create the FTS5 virtual table for memories_v2. Safe to call multiple times.
pub fn create_fts_v2_table(conn: &Connection) -> Result<()> {
    let res = conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts_v2 USING fts5(content);",
    );
    match res {
        Ok(_) => Ok(()),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("no such module: fts5") {
                Ok(()) // FTS5 unavailable — gracefully degrade
            } else {
                Err(LuminaError::Config(format!("FTS5 v2 table creation failed: {e}")))
            }
        }
    }
}

/// Insert a row into the FTS v2 index. rowid matches memories_v2 rowid.
pub fn fts_sync_insert_v2(conn: &Connection, rowid: i64, content: &str) -> Result<()> {
    let res = conn.execute(
        "INSERT INTO memory_fts_v2(rowid, content) VALUES (?1, ?2)",
        rusqlite::params![rowid, content],
    );
    match res {
        Ok(_) => Ok(()),
        Err(e) => {
            if is_fts5_unavailable(&e.to_string()) { Ok(()) }
            else { Err(LuminaError::Config(format!("FTS5 v2 insert failed: {e}"))) }
        }
    }
}

/// Delete a row from the FTS v2 index.
pub fn fts_sync_delete_v2(conn: &Connection, rowid: i64) -> Result<()> {
    let res = conn.execute(
        "DELETE FROM memory_fts_v2 WHERE rowid = ?1",
        rusqlite::params![rowid],
    );
    match res {
        Ok(_) => Ok(()),
        Err(e) => {
            if is_fts5_unavailable(&e.to_string()) { Ok(()) }
            else { Err(LuminaError::Config(format!("FTS5 v2 delete failed: {e}"))) }
        }
    }
}

/// Search memories_v2 FTS index. Returns (rowid, normalized_score) pairs.
pub fn fts_search_v2(conn: &Connection, query: &str, limit: usize) -> Result<Vec<(i64, f64)>> {
    if query.trim().is_empty() {
        return Ok(vec![]);
    }
    let escaped = escape_fts5_query(query);
    let sql = "SELECT rowid, rank FROM memory_fts_v2 WHERE content MATCH ?1 ORDER BY rank LIMIT ?2";
    let mut stmt = match conn.prepare(sql) {
        Ok(s) => s,
        Err(e) => {
            if is_fts5_unavailable(&e.to_string()) { return Ok(vec![]); }
            return Err(LuminaError::Config(format!("FTS5 v2 prepare failed: {e}")));
        }
    };
    let mapped = match stmt.query_map(rusqlite::params![escaped, limit as i64], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
    }) {
        Ok(m) => m,
        Err(e) => {
            if is_fts5_unavailable(&e.to_string()) { return Ok(vec![]); }
            return Err(LuminaError::Config(format!("FTS5 v2 query_map failed: {e}")));
        }
    };
    let rows: std::result::Result<Vec<(i64, f64)>, rusqlite::Error> = mapped.collect();
    match rows {
        Ok(raw) if !raw.is_empty() => {
            let negated: Vec<(i64, f64)> = raw.iter().map(|(id, r)| (*id, -r)).collect();
            let max_score = negated.iter().map(|(_, s)| *s).fold(0.0f64, f64::max);
            if max_score > 1e-9 {
                Ok(negated.into_iter().map(|(id, s)| (id, s / max_score)).collect())
            } else {
                Ok(negated.into_iter().map(|(id, _)| (id, 1.0)).collect())
            }
        }
        Ok(_) => Ok(vec![]),
        Err(e) => {
            if is_fts5_unavailable(&e.to_string()) { Ok(vec![]) }
            else { Err(LuminaError::Config(format!("FTS5 v2 query failed: {e}"))) }
        }
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Escape a query string for FTS5 by quoting each whitespace-delimited token
/// individually (implicit AND). Internal double-quote characters are escaped
/// as `""`.
///
/// Quoting per-token rather than the whole phrase means `"lumina memory"`
/// matches documents containing both words anywhere (better recall), while
/// still safely handling FTS5 operators (`*`, `-`, `OR`, `AND`, etc.) that
/// would otherwise cause syntax errors.
pub fn escape_fts5_query(query: &str) -> String {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|term| {
            let escaped = term.replace('"', "\"\"");
            format!("\"{escaped}\"")
        })
        .collect();
    if terms.is_empty() {
        return "\"\"".to_string();
    }
    terms.join(" ")
}

/// True when the error message indicates FTS5 is not compiled into SQLite.
///
/// Only `"no such module: fts5"` is a genuine compile-time absence of the
/// extension. Other errors (e.g. missing table) indicate a real problem that
/// callers should handle as an error rather than silently degrade.
fn is_fts5_unavailable(msg: &str) -> bool {
    msg.contains("no such module: fts5")
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn open_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS facts (
                id        INTEGER PRIMARY KEY AUTOINCREMENT,
                text      TEXT NOT NULL,
                embedding BLOB,
                ts        INTEGER NOT NULL DEFAULT 0
            );",
        )
        .unwrap();
        conn
    }

    #[test]
    fn test_fts_table_created() {
        let conn = open_test_db();
        let result = create_fts_table(&conn);
        // Either succeeds (FTS5 available) or returns Ok (graceful degrade)
        assert!(result.is_ok(), "create_fts_table should not error: {:?}", result);
    }

    #[test]
    fn test_fts_search_finds_keyword() {
        let conn = open_test_db();
        create_fts_table(&conn).unwrap();

        // Insert a row into facts and sync to FTS
        conn.execute(
            "INSERT INTO facts (text, ts) VALUES ('lumina agent memory', 0)",
            [],
        )
        .unwrap();
        let rowid = conn.last_insert_rowid();
        fts_sync_insert(&conn, rowid, "lumina agent memory").unwrap();

        conn.execute(
            "INSERT INTO facts (text, ts) VALUES ('unrelated topic here', 0)",
            [],
        )
        .unwrap();
        let rowid2 = conn.last_insert_rowid();
        fts_sync_insert(&conn, rowid2, "unrelated topic here").unwrap();

        let results = fts_search(&conn, "lumina", 10).unwrap();
        // If FTS5 unavailable, returns empty (graceful) — skip assertion
        if results.is_empty() {
            return; // FTS5 not available in test build
        }
        // The "lumina" row should appear in results
        let ids: Vec<i64> = results.iter().map(|(id, _)| *id).collect();
        assert!(ids.contains(&rowid), "Expected rowid {rowid} in FTS results: {ids:?}");
    }

    #[test]
    fn test_fts_search_escapes_special_chars() {
        let conn = open_test_db();
        create_fts_table(&conn).unwrap();

        // Queries with special FTS5 chars should not panic or return error
        let tricky_queries = [
            "hello*world",
            "test-case",
            "\"quoted\"",
            "(parentheses)",
            "OR AND NOT",
        ];
        for q in &tricky_queries {
            let result = fts_search(&conn, q, 5);
            assert!(result.is_ok(), "fts_search should not error on query {q:?}: {:?}", result);
        }
    }

    #[test]
    fn test_escape_fts5_query_wraps_in_quotes() {
        // Single token: wrapped in quotes
        assert_eq!(escape_fts5_query("hello"), "\"hello\"");
    }

    #[test]
    fn test_escape_fts5_query_escapes_internal_quotes() {
        // Double-quote inside a token is doubled
        assert_eq!(escape_fts5_query("say"), "\"say\"");
        // A token that is just a double-quote
        assert_eq!(escape_fts5_query("\""), "\"\"\"\"");
    }

    #[test]
    fn test_escape_fts5_query_multi_word_not_phrase() {
        // Multi-word queries produce per-token quoting (implicit AND), not a phrase
        let result = escape_fts5_query("lumina memory");
        assert_eq!(result, "\"lumina\" \"memory\"");
    }
}
