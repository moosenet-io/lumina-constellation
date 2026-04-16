"""
agents.py — Soma Agents API. (BS.9)

PRIVACY REQUIREMENT (Doc 31 Part B — NON-NEGOTIABLE):
  Returns agent/user META only: name, status, last_active, session_count.
  NEVER returns conversation content, session content, or prompts.
  Operator sees presence and activity summary only.

Data source: IronClaw users + conversations tables (meta aggregation only).
"""

import json
import os
import subprocess
from datetime import datetime, timezone, timedelta

PVS_HOST = os.environ.get("PVS_SSH_HOST", os.environ.get("PVS_HOST", ""))

_AGENTS_QUERY = """
SELECT
    u.id,
    u.display_name,
    u.email,
    u.status,
    u.role,
    u.last_login_at,
    COUNT(DISTINCT c.id) AS session_count,
    MAX(c.last_activity) AS last_session_activity
FROM users u
LEFT JOIN conversations c ON c.user_id = u.id
GROUP BY u.id, u.display_name, u.email, u.status, u.role, u.last_login_at
ORDER BY last_session_activity DESC NULLS LAST
"""


def _run_query(query: str) -> list:
    if not PVS_HOST:
        return []
    script = (
        "import sqlite3,json; "
        "conn=sqlite3.connect('/root/.ironclaw/ironclaw.db'); "
        "conn.row_factory=sqlite3.Row; "
        f"rows=conn.execute({json.dumps(query)}).fetchall(); "
        "print(json.dumps([dict(r) for r in rows])); "
        "conn.close()"
    )
    try:
        r = subprocess.run(
            ["ssh", "-o", "ConnectTimeout=5", "-o", "BatchMode=yes", PVS_HOST,
             f"pct exec 305 -- python3 -c {json.dumps(script)} 2>/dev/null"],
            capture_output=True, text=True, timeout=12
        )
        if r.returncode == 0 and r.stdout.strip():
            return json.loads(r.stdout.strip())
    except Exception:
        pass
    return []


def _active_status(last_activity: str | None) -> str:
    """Derive status from last activity timestamp."""
    if not last_activity:
        return "inactive"
    try:
        ts = datetime.fromisoformat(last_activity.replace("Z", "+00:00"))
        age = datetime.now(timezone.utc) - ts
        if age < timedelta(minutes=30):
            return "active"
        if age < timedelta(hours=24):
            return "recent"
        return "inactive"
    except Exception:
        return "unknown"


def get_agents() -> dict:
    """
    Return agent/user list — meta only, never content.

    Returns:
        {ok, agents: [{id, display_name, role, status, last_active,
                       session_count, activity_status}], total}
    """
    rows = _run_query(_AGENTS_QUERY)

    agents = []
    for row in rows:
        last_active = row.get("last_session_activity") or row.get("last_login_at")
        agents.append({
            "id": row.get("id", ""),
            "display_name": row.get("display_name") or "Unnamed",
            "role": row.get("role", "user"),
            "db_status": row.get("status", ""),
            "activity_status": _active_status(last_active),
            "last_active": last_active,
            "session_count": row.get("session_count", 0),
            # email omitted from response — internal identifier only
            # conversation content never included
        })

    return {
        "ok": True,
        "agents": agents,
        "total": len(agents),
    }
