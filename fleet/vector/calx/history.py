"""
CalxHistory — persists trigger activation logs.
Integrated mode: stores in Engram under vector/calx/.
Standalone mode: local SQLite.
"""
import json
import os
import sqlite3
from dataclasses import asdict
from datetime import datetime
from pathlib import Path
from typing import Optional
from .triggers import TriggerResult


class CalxHistory:
    """Records Calx trigger activations for pattern learning."""

    def __init__(self, db_path: Optional[str] = None):
        self.db_path = db_path or os.environ.get('CALX_HISTORY_DB', '/tmp/calx_history.db')
        self._init_db()

    def _init_db(self):
        con = sqlite3.connect(self.db_path)
        con.execute("""
            CREATE TABLE IF NOT EXISTS calx_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL,
                task_description TEXT,
                iteration INTEGER,
                trigger_type TEXT,
                trigger_level TEXT,
                description TEXT,
                file TEXT,
                correction TEXT,
                outcome TEXT DEFAULT 'pending'
            )
        """)
        con.commit()
        con.close()

    def log(self, iteration: int, trigger: TriggerResult, task_description: str = '', outcome: str = 'pending'):
        con = sqlite3.connect(self.db_path)
        con.execute("""
            INSERT INTO calx_log (timestamp, task_description, iteration, trigger_type, trigger_level,
                                   description, file, correction, outcome)
            VALUES (?,?,?,?,?,?,?,?,?)
        """, (
            datetime.utcnow().isoformat(),
            task_description,
            iteration,
            trigger.trigger_type.value,
            trigger.level.value,
            trigger.description,
            trigger.file,
            trigger.correction,
            outcome
        ))
        con.commit()
        con.close()

    def update_outcome(self, trigger_type: str, outcome: str, limit: int = 1):
        """Update outcome for the most recent trigger of this type."""
        con = sqlite3.connect(self.db_path)
        con.execute("""
            UPDATE calx_log SET outcome = ?
            WHERE trigger_type = ? AND outcome = 'pending'
            ORDER BY id DESC LIMIT ?
        """, (outcome, trigger_type, limit))
        con.commit()
        con.close()

    def query(self, limit: int = 20, trigger_type: Optional[str] = None) -> list[dict]:
        """Query recent trigger activations."""
        con = sqlite3.connect(self.db_path)
        con.row_factory = sqlite3.Row
        if trigger_type:
            rows = con.execute(
                "SELECT * FROM calx_log WHERE trigger_type = ? ORDER BY id DESC LIMIT ?",
                (trigger_type, limit)
            ).fetchall()
        else:
            rows = con.execute(
                "SELECT * FROM calx_log ORDER BY id DESC LIMIT ?", (limit,)
            ).fetchall()
        con.close()
        return [dict(r) for r in rows]

    def frequent_triggers(self, min_count: int = 3) -> list[dict]:
        """Find trigger patterns that fire frequently — candidates for skill proposals."""
        con = sqlite3.connect(self.db_path)
        rows = con.execute("""
            SELECT trigger_type, description, COUNT(*) as count
            FROM calx_log
            GROUP BY trigger_type, description
            HAVING count >= ?
            ORDER BY count DESC
        """, (min_count,)).fetchall()
        con.close()
        return [{'trigger_type': r[0], 'description': r[1], 'count': r[2]} for r in rows]

    def summary(self) -> dict:
        """Summary statistics."""
        con = sqlite3.connect(self.db_path)
        total = con.execute("SELECT COUNT(*) FROM calx_log").fetchone()[0]
        by_type = dict(con.execute(
            "SELECT trigger_type, COUNT(*) FROM calx_log GROUP BY trigger_type"
        ).fetchall())
        outcomes = dict(con.execute(
            "SELECT outcome, COUNT(*) FROM calx_log GROUP BY outcome"
        ).fetchall())
        con.close()
        return {'total': total, 'by_type': by_type, 'outcomes': outcomes}
