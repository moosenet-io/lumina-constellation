# PRIVACY: No conversation content, no secret values. See Doc 31 Part B.

import importlib.util
import json
import os
import re
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from fastapi import APIRouter

router = APIRouter(prefix="/api/security", tags=["security"])

FLEET_DIR = Path(os.environ.get("LUMINA_FLEET_DIR", "/opt/lumina-fleet"))
REPO_FLEET_DIR = Path(__file__).resolve().parents[2]


def _load_json(path: Path, default: Any) -> Any:
    try:
        if path.exists():
            return json.loads(path.read_text(encoding="utf-8"))
    except Exception:
        pass
    return default


def _secret_ages() -> list[dict[str, Any]]:
    candidates = [
        FLEET_DIR / "security" / "rotation.py",
        REPO_FLEET_DIR / "security" / "rotation.py",
    ]
    for path in candidates:
        if not path.exists():
            continue
        try:
            spec = importlib.util.spec_from_file_location("lumina_security_rotation", path)
            if not spec or not spec.loader:
                continue
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
            rows = module.check_secret_ages()
            return [
                {
                    "name": row.get("name", ""),
                    "age_days": row.get("age_days"),
                    "rotation_due": row.get("status") in {"warn", "expired", "overdue"},
                    "source": "dura",
                    "status": row.get("status", "unknown"),
                    "max_age_days": row.get("max_age_days"),
                    "days_remaining": row.get("days_remaining"),
                }
                for row in rows
            ]
        except Exception:
            continue
    return []


def _sessions_status() -> dict[str, Any]:
    auth_db = FLEET_DIR / "soma" / "auth.db"
    if not auth_db.exists():
        auth_db = REPO_FLEET_DIR / "soma" / "auth.db"
    data = _load_json(auth_db, {"users": {}})
    users = data.get("users", {}) if isinstance(data, dict) else {}
    return {
        "active_count": 0,
        "longest_session_mins": 0,
        "configured_users": len(users),
        "note": "Soma JWT sessions are stateless; active session tracking is not persisted.",
    }


def _rate_limits() -> dict[str, int]:
    paths = [
        Path(os.environ.get("SOMA_ACCESS_LOG", "")),
        FLEET_DIR / "soma" / "logs" / "access.log",
        Path("/var/log/caddy/access.log"),
    ]
    requests_last_hour = 0
    blocked_count = 0
    cutoff = datetime.now(timezone.utc).timestamp() - 3600
    for path in paths:
        if not path or not path.exists():
            continue
        try:
            lines = path.read_text(encoding="utf-8", errors="replace").splitlines()[-5000:]
        except Exception:
            continue
        for line in lines:
            timestamp = _extract_timestamp(line)
            if timestamp and timestamp < cutoff:
                continue
            requests_last_hour += 1
            if re.search(r"\b(429|403)\b", line):
                blocked_count += 1
    return {"requests_last_hour": requests_last_hour, "blocked_count": blocked_count}


def _extract_timestamp(line: str) -> float | None:
    match = re.search(r"(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})?)", line)
    if not match:
        return None
    try:
        value = match.group(1).replace("Z", "+00:00")
        return datetime.fromisoformat(value).timestamp()
    except Exception:
        return None


def _pii_gate_status() -> dict[str, int]:
    paths = [
        Path(os.environ.get("PII_GATE_LOG", "")),
        FLEET_DIR / "security" / "pii_gate.log",
        FLEET_DIR / "terminus" / "logs" / "pii_gate.log",
    ]
    scans_today = 0
    findings_blocked = 0
    today = datetime.now(timezone.utc).date().isoformat()
    for path in paths:
        if not path or not path.exists():
            continue
        try:
            lines = path.read_text(encoding="utf-8", errors="replace").splitlines()[-5000:]
        except Exception:
            continue
        for line in lines:
            if today not in line:
                continue
            scans_today += 1
            if "BLOCKED" in line or "blocked" in line:
                findings_blocked += 1
    return {"scans_today": scans_today, "findings_blocked": findings_blocked}


@router.get("/status")
async def get_security_status():
    return {
        "ok": True,
        "secrets": _secret_ages(),
        "sessions": _sessions_status(),
        "rate_limits": _rate_limits(),
        "pii_gate": _pii_gate_status(),
    }
