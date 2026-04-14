#!/usr/bin/env python3
"""Initialize Myelin SQLite database schema."""
import sqlite3, os
from pathlib import Path

DB_PATH = os.environ.get('MYELIN_DB_PATH', '/opt/lumina-fleet/myelin/myelin.db')

def init_db():
    conn = sqlite3.connect(DB_PATH)
    conn.executescript("""
    CREATE TABLE IF NOT EXISTS usage_log (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        timestamp TEXT NOT NULL DEFAULT (datetime('now')),
        agent_id TEXT NOT NULL DEFAULT 'unknown',
        model TEXT NOT NULL,
        provider TEXT NOT NULL,
        route TEXT NOT NULL DEFAULT 'unknown',
        input_tokens INTEGER DEFAULT 0,
        output_tokens INTEGER DEFAULT 0,
        cost_usd REAL DEFAULT 0.0,
        latency_ms INTEGER DEFAULT 0,
        task_context TEXT DEFAULT ''
    );

    CREATE INDEX IF NOT EXISTS idx_usage_agent ON usage_log(agent_id, timestamp);
    CREATE INDEX IF NOT EXISTS idx_usage_model ON usage_log(model, timestamp);

    CREATE TABLE IF NOT EXISTS subscriptions (
        provider TEXT PRIMARY KEY,
        plan TEXT DEFAULT 'unknown',
        cycle_start TEXT,
        cycle_end TEXT,
        limit_value REAL DEFAULT 0,
        limit_unit TEXT DEFAULT 'usd',
        current_usage REAL DEFAULT 0,
        last_checked TEXT
    );

    INSERT OR IGNORE INTO subscriptions (provider, plan, limit_unit) VALUES
        ('anthropic', 'max_5x', 'percent'),
        ('openrouter', 'credits', 'usd'),
        ('local', 'unlimited', 'tokens');

    CREATE TABLE IF NOT EXISTS agent_baselines (
        agent_id TEXT PRIMARY KEY,
        avg_daily_cost REAL DEFAULT 0.0,
        avg_daily_tokens INTEGER DEFAULT 0,
        typical_models TEXT DEFAULT '[]',
        updated TEXT DEFAULT (datetime('now'))
    );
    """)
    conn.commit()
    conn.close()
    print(f"Myelin DB initialized at {DB_PATH}")

if __name__ == '__main__':
    init_db()
