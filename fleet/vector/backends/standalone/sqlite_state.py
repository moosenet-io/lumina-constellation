"""Standalone StateBackend using SQLite."""

import sqlite3, json, os
from datetime import datetime
from typing import Optional, List, Dict
from interfaces import StateBackend, Task


class SQLiteState(StateBackend):
    def __init__(self, db_path: str):
        self.db_path = db_path
        self._init_db()

    def _init_db(self):
        with sqlite3.connect(self.db_path) as conn:
            conn.execute('''CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT,
                status TEXT NOT NULL DEFAULT 'queued',
                result_json TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                metadata_json TEXT
            )''')
            conn.execute('''CREATE TABLE IF NOT EXISTS model_performance (
                model TEXT NOT NULL PRIMARY KEY,
                iteration_count INTEGER DEFAULT 0,
                gate_pass_count INTEGER DEFAULT 0,
                gate_fail_count INTEGER DEFAULT 0,
                avg_context_pct REAL DEFAULT 0.0,
                current_prime_pct INTEGER DEFAULT 35,
                last_adjusted TEXT
            )''')
            conn.commit()

    def _row_to_task(self, row) -> Task:
        return Task(
            id=row[0], name=row[1], description=row[2] or '',
            status=row[3],
            result=json.loads(row[4]) if row[4] else None,
            created_at=row[5], updated_at=row[6],
            metadata=json.loads(row[7]) if row[7] else {}
        )

    def create_task(self, task: Task) -> Task:
        now = datetime.utcnow().isoformat() + 'Z'
        task.created_at = task.updated_at = now
        with sqlite3.connect(self.db_path) as conn:
            conn.execute('INSERT INTO tasks VALUES (?,?,?,?,?,?,?,?)',
                (task.id, task.name, task.description, task.status,
                 json.dumps(task.result) if task.result else None,
                 task.created_at, task.updated_at, json.dumps(task.metadata)))
            conn.commit()
        return task

    def update_status(self, task_id: str, status: str, result: Optional[Dict] = None) -> bool:
        now = datetime.utcnow().isoformat() + 'Z'
        with sqlite3.connect(self.db_path) as conn:
            conn.execute('UPDATE tasks SET status=?, result_json=?, updated_at=? WHERE id=?',
                (status, json.dumps(result) if result else None, now, task_id))
            conn.commit()
        return True

    def get_task(self, task_id: str) -> Optional[Task]:
        with sqlite3.connect(self.db_path) as conn:
            row = conn.execute('SELECT * FROM tasks WHERE id=?', (task_id,)).fetchone()
        return self._row_to_task(row) if row else None

    def list_tasks(self, status_filter: Optional[str] = None) -> List[Task]:
        with sqlite3.connect(self.db_path) as conn:
            if status_filter:
                rows = conn.execute('SELECT * FROM tasks WHERE status=? ORDER BY created_at', (status_filter,)).fetchall()
            else:
                rows = conn.execute('SELECT * FROM tasks ORDER BY created_at').fetchall()
        return [self._row_to_task(r) for r in rows]

    def complete_task(self, task_id: str, result: Dict) -> bool:
        return self.update_status(task_id, 'done', result)

    def record_iteration(self, model: str, gate_passed: bool, context_pct: float = 0.0):
        """Record model performance for one iteration."""
        with sqlite3.connect(self.db_path) as conn:
            conn.execute("""
                INSERT INTO model_performance (model, iteration_count, gate_pass_count, gate_fail_count, avg_context_pct, last_adjusted)
                VALUES (?, 1, ?, ?, ?, datetime('now'))
                ON CONFLICT(model) DO UPDATE SET
                    iteration_count = iteration_count + 1,
                    gate_pass_count = gate_pass_count + excluded.gate_pass_count,
                    gate_fail_count = gate_fail_count + excluded.gate_fail_count,
                    avg_context_pct = (avg_context_pct * (iteration_count - 1) + excluded.avg_context_pct) / iteration_count,
                    last_adjusted = excluded.last_adjusted
            """, (model, 1 if gate_passed else 0, 0 if gate_passed else 1, context_pct))
            conn.commit()

    def get_model_performance(self, model: str = None) -> List[Dict]:
        """Get performance stats for one or all models."""
        with sqlite3.connect(self.db_path) as conn:
            conn.row_factory = sqlite3.Row
            if model:
                rows = conn.execute("SELECT * FROM model_performance WHERE model = ?", (model,)).fetchall()
            else:
                rows = conn.execute("SELECT * FROM model_performance ORDER BY iteration_count DESC").fetchall()
        return [dict(r) for r in rows]
