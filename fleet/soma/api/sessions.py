"""
sessions.py — Soma Sessions API. (BS.5)

PRIVACY REQUIREMENT (Doc 31 Part B — NON-NEGOTIABLE):
  Returns session META only: timestamps, duration, message count, token usage, cost.
  NEVER returns session content, prompts, or model responses.
  NEVER exposes conversation_messages.content or any prompt/response text.
  In multi-claw mode: operator sees own sessions only. Aggregate counts
  only for other agents, never their content.

Data source: IronClaw SQLite DB at /root/.ironclaw/ironclaw.db.
Access pattern: provider-neutral remote target command via environment template.
"""

import json
import os
import subprocess
import sys
from datetime import datetime, timezone

REMOTE_SSH_HOST = os.environ.get("REMOTE_SSH_HOST", "")
IRONCLAW_REMOTE_TARGET = os.environ.get("IRONCLAW_REMOTE_TARGET", "")
REMOTE_EXEC_TEMPLATE = os.environ.get("REMOTE_EXEC_TEMPLATE", "")


def _remote_exec(target: str, command: str) -> str:
    if not REMOTE_EXEC_TEMPLATE:
        return ""
    return REMOTE_EXEC_TEMPLATE.format(target=target, command=command)

# Privacy-safe columns only — content columns explicitly excluded
_SESSION_QUERY = """
SELECT
    c.id,
    c.channel,
    c.started_at,
    c.last_activity,
    COUNT(DISTINCT cm.id) AS message_count,
    COALESCE(SUM(lc.input_tokens + lc.output_tokens), 0) AS total_tokens,
    COALESCE(SUM(lc.cost), 0.0) AS cost_usd,
    GROUP_CONCAT(DISTINCT lc.model) AS models_used
FROM conversations c
LEFT JOIN conversation_messages cm ON cm.conversation_id = c.id
LEFT JOIN llm_calls lc ON lc.conversation_id = c.id
GROUP BY c.id, c.channel, c.started_at, c.last_activity
ORDER BY c.last_activity DESC
LIMIT {limit} OFFSET {offset}
"""

_COUNT_QUERY = "SELECT COUNT(*) FROM conversations"


def _run_sqlite_query(query: str) -> list:
    """Execute query on ironclaw-host DB via SSH. Returns list of row dicts."""
    if not (REMOTE_SSH_HOST and IRONCLAW_REMOTE_TARGET):
        return []

    script = (
        "import sqlite3, json; "
        "conn = sqlite3.connect('/root/.ironclaw/ironclaw.db'); "
        "conn.row_factory = sqlite3.Row; "
        f"rows = conn.execute({json.dumps(query)}).fetchall(); "
        "print(json.dumps([dict(r) for r in rows])); "
        "conn.close()"
    )

    try:
        remote_cmd = _remote_exec(
            IRONCLAW_REMOTE_TARGET,
            f"python3 -c {json.dumps(script)} 2>/dev/null",
        )
        if not remote_cmd:
            return []
        result = subprocess.run(
            ["ssh", "-o", "ConnectTimeout=5", "-o", "BatchMode=yes", REMOTE_SSH_HOST, remote_cmd],
            capture_output=True, text=True, timeout=12
        )
        if result.returncode == 0 and result.stdout.strip():
            return json.loads(result.stdout.strip())
    except Exception:
        pass
    return []


def _format_duration(started: str, ended: str) -> int:
    """Return duration in seconds between two ISO timestamps. 0 on parse error."""
    try:
        fmt = "%Y-%m-%dT%H:%M:%S.%fZ"
        t0 = datetime.strptime(started[:26] + "Z", fmt).replace(tzinfo=timezone.utc)
        t1 = datetime.strptime(ended[:26] + "Z", fmt).replace(tzinfo=timezone.utc)
        return max(0, int((t1 - t0).total_seconds()))
    except Exception:
        return 0


def get_sessions(limit: int = 50, offset: int = 0) -> dict:
    """
    Return session metadata list. Privacy-safe — no content.

    Returns:
        {ok, sessions: [{id, channel, started_at, last_activity,
                         duration_s, message_count, total_tokens, cost_usd,
                         models_used}],
         total, limit, offset}
    """
    limit = min(max(1, limit), 200)
    offset = max(0, offset)

    rows = _run_sqlite_query(_SESSION_QUERY.format(limit=limit, offset=offset))
    count_rows = _run_sqlite_query(_COUNT_QUERY)
    total = count_rows[0].get("COUNT(*)", 0) if count_rows else 0

    sessions = []
    for row in rows:
        # Sanitise models_used — comma-separated, may be None
        models_raw = row.get("models_used") or ""
        models = list({m.strip() for m in models_raw.split(",") if m.strip()})

        sessions.append({
            "id": row.get("id", ""),
            "channel": row.get("channel", ""),
            "started_at": row.get("started_at", ""),
            "last_activity": row.get("last_activity", ""),
            "duration_s": _format_duration(
                row.get("started_at", ""),
                row.get("last_activity", "")
            ),
            "message_count": row.get("message_count", 0),
            "total_tokens": row.get("total_tokens", 0),
            "cost_usd": round(float(row.get("cost_usd", 0.0)), 6),
            "models_used": models,
            # content_preview intentionally omitted — privacy
        })

    return {
        "ok": True,
        "sessions": sessions,
        "total": total,
        "limit": limit,
        "offset": offset,
    }


def get_sessions_error(msg: str) -> dict:
    return {"ok": False, "sessions": [], "total": 0, "error": msg}
