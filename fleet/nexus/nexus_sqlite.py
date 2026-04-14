#!/usr/bin/env python3
"""
Nexus SQLite fallback — enables Nexus inbox without PostgreSQL.
Drop-in replacement for the psycopg2 Nexus backend.
Used in standalone/public deployments where Postgres is not available.

Usage: set NEXUS_BACKEND=sqlite and NEXUS_SQLITE_PATH=/path/to/nexus.db
"""

import os
import json
import sqlite3
import uuid
import threading
from datetime import datetime
from pathlib import Path
from contextlib import contextmanager

SQLITE_PATH = os.environ.get('NEXUS_SQLITE_PATH', '/opt/lumina-fleet/nexus/nexus.db')

_local = threading.local()

SCHEMA = """
CREATE TABLE IF NOT EXISTS inbox_messages (
    id TEXT PRIMARY KEY DEFAULT (lower(hex(randomblob(16)))),
    from_agent TEXT NOT NULL,
    to_agent TEXT NOT NULL,
    message_type TEXT NOT NULL DEFAULT 'message',
    payload TEXT NOT NULL DEFAULT '{}',
    priority TEXT NOT NULL DEFAULT 'normal' CHECK(priority IN ('critical','urgent','normal','low')),
    status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','read','processed','failed')),
    correlation_id TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_to_agent ON inbox_messages(to_agent, status);
CREATE INDEX IF NOT EXISTS idx_from_agent ON inbox_messages(from_agent);
CREATE INDEX IF NOT EXISTS idx_correlation ON inbox_messages(correlation_id);
CREATE INDEX IF NOT EXISTS idx_created ON inbox_messages(created_at);
"""


@contextmanager
def _conn():
    """Thread-safe SQLite connection with WAL mode for concurrent access."""
    if not hasattr(_local, 'conn') or _local.conn is None:
        Path(SQLITE_PATH).parent.mkdir(parents=True, exist_ok=True)
        _local.conn = sqlite3.connect(SQLITE_PATH, check_same_thread=False)
        _local.conn.row_factory = sqlite3.Row
        _local.conn.execute('PRAGMA journal_mode=WAL')
        _local.conn.execute('PRAGMA synchronous=NORMAL')
        _local.conn.executescript(SCHEMA)
        _local.conn.commit()
    yield _local.conn


def nexus_send(from_agent: str, to_agent: str, message_type: str,
               payload: dict, priority: str = 'normal',
               correlation_id: str = None) -> dict:
    """Send a message to the inbox."""
    msg_id = str(uuid.uuid4())
    with _conn() as conn:
        conn.execute("""
            INSERT INTO inbox_messages
                (id, from_agent, to_agent, message_type, payload, priority, status, correlation_id)
            VALUES (?, ?, ?, ?, ?, ?, 'pending', ?)
        """, (msg_id, from_agent, to_agent, message_type,
              json.dumps(payload), priority, correlation_id))
        conn.commit()
    return {'id': msg_id, 'status': 'sent'}


def nexus_check(agent_id: str) -> dict:
    """Lightweight count of pending messages. ~0 cost."""
    with _conn() as conn:
        row = conn.execute(
            "SELECT COUNT(*) FROM inbox_messages WHERE to_agent=? AND status='pending'",
            (agent_id,)
        ).fetchone()
        critical = conn.execute(
            "SELECT COUNT(*) FROM inbox_messages WHERE to_agent=? AND status='pending' AND priority='critical'",
            (agent_id,)
        ).fetchone()
    return {'agent_id': agent_id, 'pending': row[0], 'critical': critical[0]}


def nexus_read(agent_id: str, type_filter: str = None,
               priority_filter: str = None, limit: int = 10) -> list:
    """Read pending messages for an agent. Returns list of messages."""
    query = "SELECT * FROM inbox_messages WHERE to_agent=? AND status='pending'"
    params = [agent_id]
    if type_filter:
        query += " AND message_type=?"
        params.append(type_filter)
    if priority_filter:
        query += " AND priority=?"
        params.append(priority_filter)
    query += " ORDER BY CASE priority WHEN 'critical' THEN 1 WHEN 'urgent' THEN 2 WHEN 'normal' THEN 3 ELSE 4 END, created_at ASC LIMIT ?"
    params.append(limit)

    with _conn() as conn:
        rows = conn.execute(query, params).fetchall()
        messages = []
        for row in rows:
            msg = dict(row)
            try:
                msg['payload'] = json.loads(msg['payload'])
            except Exception:
                pass
            messages.append(msg)

        # Mark as read
        if rows:
            ids = [r['id'] for r in rows]
            conn.execute(
                f"UPDATE inbox_messages SET status='read', updated_at=datetime('now') WHERE id IN ({','.join('?'*len(ids))})",
                ids
            )
            conn.commit()

    return messages


def nexus_ack(message_ids: str) -> dict:
    """Mark messages as processed. message_ids: comma-separated UUIDs."""
    ids = [mid.strip() for mid in message_ids.split(',') if mid.strip()]
    if not ids:
        return {'error': 'No message IDs provided'}
    with _conn() as conn:
        conn.execute(
            f"UPDATE inbox_messages SET status='processed', updated_at=datetime('now') WHERE id IN ({','.join('?'*len(ids))})",
            ids
        )
        conn.commit()
    return {'acknowledged': len(ids), 'message_ids': ids}


def nexus_history(agent_id: str, correlation_id: str = None, hours_back: int = 24) -> list:
    """Get message history for audit trail."""
    query = """
        SELECT id, from_agent, to_agent, message_type, priority, status, created_at, correlation_id
        FROM inbox_messages
        WHERE (from_agent=? OR to_agent=?)
          AND created_at > datetime('now', ?)
    """
    params = [agent_id, agent_id, f'-{hours_back} hours']
    if correlation_id:
        query += " AND correlation_id=?"
        params.append(correlation_id)
    query += " ORDER BY created_at DESC LIMIT 50"

    with _conn() as conn:
        rows = conn.execute(query, params).fetchall()
    return [dict(r) for r in rows]


# ── Compatibility shim for psycopg2-based code ─────────────────────────────

def get_nexus_connection():
    """
    Returns a context manager that mimics psycopg2 connection interface.
    Allows psycopg2-based code to work with SQLite backend with minimal changes.
    """
    class SQLiteCompat:
        """Minimal psycopg2-compatible wrapper around sqlite3."""

        class Cursor:
            def __init__(self, conn):
                self._conn = conn
                self._cur = conn.cursor()
                self.lastrowid = None

            def execute(self, query, params=None):
                # Translate %s to ? for SQLite
                query = query.replace('%s', '?').replace('%(', '%(')
                # Translate JSONB operators
                query = query.replace('::jsonb', '').replace('->>\'', "json_extract(payload, '$.").replace("')", "')")
                if params:
                    self._cur.execute(query, params)
                else:
                    self._cur.execute(query)
                self.lastrowid = self._cur.lastrowid

            def fetchone(self):
                row = self._cur.fetchone()
                return row

            def fetchall(self):
                return self._cur.fetchall()

        def __init__(self):
            Path(SQLITE_PATH).parent.mkdir(parents=True, exist_ok=True)
            self._conn = sqlite3.connect(SQLITE_PATH)
            self._conn.row_factory = sqlite3.Row
            self._conn.execute('PRAGMA journal_mode=WAL')
            self._conn.executescript(SCHEMA)
            self._conn.commit()

        def cursor(self):
            return self.Cursor(self._conn)

        def commit(self):
            self._conn.commit()

        def close(self):
            self._conn.close()

    return SQLiteCompat()


if __name__ == '__main__':
    import argparse, sys
    parser = argparse.ArgumentParser(description='Nexus SQLite CLI')
    sub = parser.add_subparsers(dest='cmd')
    sub.add_parser('init')
    p = sub.add_parser('send')
    p.add_argument('from_agent'); p.add_argument('to_agent'); p.add_argument('message_type')
    p.add_argument('--payload', default='{}'); p.add_argument('--priority', default='normal')
    p = sub.add_parser('check'); p.add_argument('agent_id')
    p = sub.add_parser('read'); p.add_argument('agent_id'); p.add_argument('--limit', type=int, default=5)
    p = sub.add_parser('stats')

    args = parser.parse_args()
    if args.cmd == 'init':
        with _conn():
            pass
        print(f'Nexus SQLite initialized at {SQLITE_PATH}')
    elif args.cmd == 'send':
        print(json.dumps(nexus_send(args.from_agent, args.to_agent, args.message_type,
                                    json.loads(args.payload), args.priority)))
    elif args.cmd == 'check':
        print(json.dumps(nexus_check(args.agent_id)))
    elif args.cmd == 'read':
        print(json.dumps(nexus_read(args.agent_id, limit=args.limit), indent=2))
    elif args.cmd == 'stats':
        with _conn() as conn:
            count = conn.execute('SELECT COUNT(*) FROM inbox_messages').fetchone()[0]
            pending = conn.execute("SELECT COUNT(*) FROM inbox_messages WHERE status='pending'").fetchone()[0]
        print(json.dumps({'total': count, 'pending': pending, 'backend': 'sqlite', 'path': SQLITE_PATH}))
