"""
logs.py — Soma Multi-Source Log Aggregator API. (BS.7)

PRIVACY REQUIREMENT (Doc 31 Part B — NON-NEGOTIABLE):
  Returns SYSTEM EVENTS ONLY: service starts, timer fires, API errors,
  health check results, deployment events.
  NEVER logs conversation content, prompts, or model responses.
  Messages are filtered to strip any content that looks like inference output.
  When in doubt, drop the log line.

Sources:
  - fleet-host: soma, axon, vigil, sentinel, spectra
  - terminus-host: ai-mcp
  - ironclaw-host: ironclaw (service events only, not conversation logs)

Access: SSH via REMOTE_SSH_HOST and REMOTE_EXEC_TEMPLATE.
"""

import os
import subprocess
import json
import time
from datetime import datetime, timezone, timedelta
from typing import Optional

REMOTE_SSH_HOST = os.environ.get("REMOTE_SSH_HOST", "")
REMOTE_EXEC_TEMPLATE = os.environ.get("REMOTE_EXEC_TEMPLATE", "")


def _remote_exec(target: str, command: str) -> str:
    if not REMOTE_EXEC_TEMPLATE:
        return ""
    return REMOTE_EXEC_TEMPLATE.format(target=target, command=command)

PRIORITY_MAP = {
    "0": "emergency", "1": "alert", "2": "critical",
    "3": "error", "4": "warn", "5": "notice", "6": "info", "7": "debug",
}
LEVEL_PRIORITY = {
    "error": ["0","1","2","3"],
    "warn":  ["0","1","2","3","4"],
    "info":  ["0","1","2","3","4","5","6"],
    "all":   ["0","1","2","3","4","5","6","7"],
    "debug": ["0","1","2","3","4","5","6","7"],
}

# Services to collect per source — only system-event services, never agent chat
SOURCES = {
    "fleet": {
        "target": os.environ.get("FLEET_REMOTE_TARGET", ""),
        "services": ["soma", "axon", "vigil", "sentinel-health", "sentinel-metrics",
                     "spectra", "synapse-scan", "inbox-monitor", "skill-evolution",
                     "secret-rotation-check"],
    },
    "terminus": {
        "target": os.environ.get("TERMINUS_REMOTE_TARGET", ""),
        "services": ["ai-mcp"],
    },
    "ironclaw": {
        "target": os.environ.get("IRONCLAW_REMOTE_TARGET", ""),
        # IronClaw service events only — conversation logs excluded by MESSAGE filter
        "services": ["ironclaw"],
    },
}

# Patterns that suggest conversation/inference content — drop these lines
_CONTENT_PATTERNS = [
    "conversation_id", "prompt:", "response:", "completion:",
    "user_message", "assistant_message", "tool_result",
]


def _is_system_event(message: str) -> bool:
    """Return True if the message is a system event, not inference content."""
    msg_lower = message.lower()
    return not any(p in msg_lower for p in _CONTENT_PATTERNS)


def _fetch_logs(target: str, services: list, since_iso: str, limit: int) -> list:
    """Fetch journalctl logs from a remote target via SSH."""
    if not (REMOTE_SSH_HOST and target):
        return []

    # Build journalctl command with service filters
    service_args = " ".join(f"-u {s}" for s in services)
    cmd = (
        f"journalctl {service_args} "
        f"--since '{since_iso}' "
        f"--output=json --no-pager -n {limit} 2>/dev/null"
    )

    try:
        remote_cmd = _remote_exec(target, cmd)
        if not remote_cmd:
            return []
        result = subprocess.run(
            ["ssh", "-o", "ConnectTimeout=5", "-o", "BatchMode=yes", REMOTE_SSH_HOST, remote_cmd],
            capture_output=True, text=True, timeout=15
        )
        lines = []
        for line in result.stdout.splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                entry = json.loads(line)
                # Extract fields
                ts_us = int(entry.get("__REALTIME_TIMESTAMP", 0))
                ts = datetime.fromtimestamp(ts_us / 1_000_000, tz=timezone.utc).isoformat()
                message = entry.get("MESSAGE", "")
                if isinstance(message, list):
                    message = " ".join(chr(b) for b in message if b < 128)
                priority = entry.get("PRIORITY", "6")
                service = entry.get("SYSLOG_IDENTIFIER", entry.get("_SYSTEMD_UNIT", ""))

                # Privacy filter
                if not _is_system_event(message):
                    continue

                lines.append({
                    "timestamp": ts,
                    "source": f"ct{ct}",
                    "service": service.replace(".service", ""),
                    "level": LEVEL_MAP.get(priority, "info"),
                    "priority": priority,
                    "message": message[:500],  # cap message length
                })
            except (json.JSONDecodeError, ValueError):
                continue
        return lines
    except Exception:
        return []


LEVEL_MAP = {
    "0": "error", "1": "error", "2": "error", "3": "error",
    "4": "warn", "5": "info", "6": "info", "7": "debug",
}


def get_logs(
    source: str = "all",
    level: str = "all",
    since_minutes: int = 60,
    limit: int = 500,
) -> dict:
    """
    Aggregate logs from all configured sources.

    Args:
        source: 'all' | 'fleet' | 'terminus' | 'ironclaw'
        level: 'all' | 'error' | 'warn' | 'info' | 'debug'
        since_minutes: look back N minutes (default 60, max 1440)
        limit: max entries to return (default 500, max 2000)

    Returns:
        {ok, entries: [{timestamp, source, service, level, message}],
         total, truncated, sources_queried}
    """
    limit = min(max(1, limit), 2000)
    since_minutes = min(max(1, since_minutes), 1440)

    since_dt = datetime.now(timezone.utc) - timedelta(minutes=since_minutes)
    since_iso = since_dt.strftime("%Y-%m-%d %H:%M:%S")

    # Determine which sources to query
    if source == "all":
        query_sources = list(SOURCES.keys())
    elif source in SOURCES:
        query_sources = [source]
    else:
        return {"ok": False, "entries": [], "error": f"Unknown source: {source}"}

    # Allowed priority levels for filter
    allowed_priorities = set(LEVEL_PRIORITY.get(level, LEVEL_PRIORITY["all"]))

    all_entries = []
    sources_queried = []

    for src_name in query_sources:
        src_cfg = SOURCES[src_name]
        entries = _fetch_logs(
            target=src_cfg["target"],
            services=src_cfg["services"],
            since_iso=since_iso,
            limit=limit,
        )
        # Filter by level
        filtered = [e for e in entries if e["priority"] in allowed_priorities]
        all_entries.extend(filtered)
        sources_queried.append(src_name)

    # Sort by timestamp descending, truncate
    all_entries.sort(key=lambda e: e["timestamp"], reverse=True)
    truncated = len(all_entries) > limit
    all_entries = all_entries[:limit]

    return {
        "ok": True,
        "entries": all_entries,
        "total": len(all_entries),
        "truncated": truncated,
        "sources_queried": sources_queried,
        "since_iso": since_iso,
    }
