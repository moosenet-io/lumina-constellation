# PRIVACY: No conversation content, no secret values. See Doc 31 Part B.

import json
import os
import re
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from fastapi import APIRouter

router = APIRouter(prefix="/api/timers", tags=["timers"])

FLEET_DIR = Path(os.environ.get("LUMINA_FLEET_DIR", "/opt/lumina-fleet"))
REPO_FLEET_DIR = Path(__file__).resolve().parents[2]


def _microseconds_to_iso(value: Any) -> str | None:
    try:
        value = int(value or 0)
    except Exception:
        return None
    if value <= 0:
        return None
    return datetime.fromtimestamp(value / 1_000_000, tz=timezone.utc).isoformat()


def _relative_time(iso_value: str | None) -> str:
    if not iso_value:
        return ""
    try:
        dt = datetime.fromisoformat(iso_value)
        delta = dt - datetime.now(timezone.utc)
        seconds = int(delta.total_seconds())
    except Exception:
        return iso_value

    suffix = "" if seconds >= 0 else " ago"
    seconds = abs(seconds)
    prefix = "in " if suffix == "" else ""
    if seconds < 60:
        return f"{prefix}{seconds}s{suffix}"
    if seconds < 3600:
        return f"{prefix}{seconds // 60}m{suffix}"
    if seconds < 86400:
        hours, minutes = divmod(seconds // 60, 60)
        return f"{prefix}{hours}h {minutes}m{suffix}" if minutes else f"{prefix}{hours}h{suffix}"
    return f"{prefix}{seconds // 86400}d{suffix}"


def _systemd_timers() -> list[dict[str, Any]]:
    result = subprocess.run(
        ["systemctl", "list-timers", "--all", "--no-pager", "--output=json"],
        capture_output=True,
        text=True,
        timeout=10,
    )
    if result.returncode != 0 or not result.stdout.strip().startswith("["):
        return []

    rows = json.loads(result.stdout)
    timers = []
    for row in rows:
        name = row.get("unit", "")
        if not name.endswith(".timer"):
            continue
        next_run = _microseconds_to_iso(row.get("next"))
        last_run = _microseconds_to_iso(row.get("last"))
        state = "active" if next_run else "inactive"
        timers.append(
            {
                "name": name,
                "type": "systemd",
                "next_run": next_run,
                "last_run": last_run,
                "state": state,
                "description": row.get("description", ""),
                "unit": row.get("activates") or name.replace(".timer", ".service"),
                "active": state == "active",
                "enabled": state == "active",
                "next": _relative_time(next_run),
                "last": _relative_time(last_run),
            }
        )
    return timers


def _parse_scheduler_line(line: str) -> dict[str, Any] | None:
    match = re.match(r"\s*(?P<id>\S+)\s{2,}(?P<name>.*?)\s{2,}(?P<trigger>.+)$", line)
    if not match:
        return None
    job_id = match.group("id")
    if job_id == "Scheduled":
        return None
    return {
        "name": job_id,
        "type": "apscheduler",
        "next_run": None,
        "last_run": None,
        "state": "scheduled",
        "description": match.group("name").strip(),
        "trigger": match.group("trigger").strip(),
        "active": True,
        "enabled": True,
        "next": "",
        "last": "",
    }


def _apscheduler_jobs() -> list[dict[str, Any]]:
    scheduler = FLEET_DIR / "scheduler.py"
    if not scheduler.exists():
        scheduler = REPO_FLEET_DIR / "scheduler.py"
    if not scheduler.exists():
        return []

    try:
        result = subprocess.run(
            [sys.executable, str(scheduler), "--list"],
            capture_output=True,
            text=True,
            timeout=8,
            env={**os.environ, "FLEET_DIR": str(FLEET_DIR)},
        )
    except Exception:
        return []
    if result.returncode != 0:
        return []

    jobs = []
    for line in result.stdout.splitlines():
        parsed = _parse_scheduler_line(line)
        if parsed:
            jobs.append(parsed)
    return jobs


@router.get("")
async def get_timers():
    errors = []
    try:
        systemd = _systemd_timers()
    except Exception as exc:
        systemd = []
        errors.append(f"systemd: {str(exc)[:120]}")

    apscheduler = _apscheduler_jobs()
    timers = sorted(systemd + apscheduler, key=lambda row: (row.get("next_run") or "9999", row["name"]))
    return {
        "ok": True,
        "count": len(timers),
        "timers": timers,
        "sources": {
            "systemd": len(systemd),
            "apscheduler": len(apscheduler),
        },
        "errors": errors,
    }
