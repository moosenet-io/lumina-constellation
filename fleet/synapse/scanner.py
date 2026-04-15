"""
scanner.py — Synapse Stage 1: Trigger Detection
MooseNet · Document 26 implementation

Pure Python, $0. Scans all trigger sources and returns candidate list.
Each candidate: {"type": str, "score": float, "source": str, "data": dict}

Sources:
  - Engram: new facts, hub nodes, needs_review tags
  - Pulse: temporal patterns (time-since, time-of-day)
  - Sentinel: resolved health issues
  - Vigil: news items matching interests
  - Vector: completed tasks not yet acknowledged
  - Nexus: agent messages awaiting operator attention

Usage:
    from scanner import SynapseScanner
    scanner = SynapseScanner(config)
    candidates = scanner.scan()
"""

import json
import os
import re
import sqlite3
import time
from datetime import datetime, timedelta
from pathlib import Path
from typing import Any

# ---------------------------------------------------------------------------
# Paths (env-overridable)
# ---------------------------------------------------------------------------

ENGRAM_DB_PATH    = Path(os.environ.get("ENGRAM_DB_PATH", "/opt/lumina-fleet/engram/engram.db"))
SENTINEL_STATE    = Path(os.environ.get("SENTINEL_STATE_PATH", "/opt/lumina-fleet/sentinel/state.json"))
VIGIL_STATE       = Path(os.environ.get("VIGIL_STATE_PATH", "/opt/lumina-fleet/vigil/briefing_state.json"))
VECTOR_STATE      = Path(os.environ.get("VECTOR_STATE_PATH", "/opt/lumina-fleet/vector/completed_tasks.json"))
PULSE_MARKERS     = Path(os.environ.get("PULSE_MARKERS_PATH", "/opt/lumina-fleet/pulse/markers.json"))
NEXUS_DB_PATH     = Path(os.environ.get("NEXUS_DB_PATH", "/opt/lumina-fleet/nexus/nexus.db"))


def _load_json(path: Path) -> Any:
    """Load JSON file, return None on any error."""
    try:
        with open(path) as f:
            return json.load(f)
    except Exception:
        return None


def _pulse_since_seconds(marker_name: str) -> float | None:
    """Return seconds since a Pulse marker was set, or None."""
    data = _load_json(PULSE_MARKERS)
    if data and marker_name in data:
        return time.time() - data[marker_name]
    return None


# ---------------------------------------------------------------------------
# Interest keyword matching (Python, $0)
# ---------------------------------------------------------------------------

_DEFAULT_INTERESTS = [
    "AI", "ML", "machine learning", "LLM", "homelab", "proxmox",
    "drone", "FPV", "hockey", "NHL", "leafs", "canucks",
    "budget", "finance", "qwen", "ollama",
]


def _matches_interests(text: str, interests: list[str]) -> float:
    """Return a 0.0–1.0 relevance score based on keyword overlap."""
    if not text or not interests:
        return 0.0
    text_lower = text.lower()
    matched = sum(1 for kw in interests if kw.lower() in text_lower)
    return min(1.0, matched * 0.3)


# ---------------------------------------------------------------------------
# Scanner
# ---------------------------------------------------------------------------

class SynapseScanner:
    """
    Scans all trigger sources and returns a list of candidates.
    Pure Python, no LLM calls.
    """

    def __init__(self, config: dict):
        self.config = config
        self.interests = config.get("interests", _DEFAULT_INTERESTS)
        self.lookback_hours = config.get("engram_lookback_hours", 24)

    def scan(self) -> list[dict]:
        """Run all scanners, return combined candidate list."""
        candidates = []
        candidates.extend(self._scan_engram_new_facts())
        candidates.extend(self._scan_engram_hub_nodes())
        candidates.extend(self._scan_engram_needs_review())
        candidates.extend(self._scan_pulse_temporal())
        candidates.extend(self._scan_sentinel())
        candidates.extend(self._scan_vigil())
        candidates.extend(self._scan_vector())
        candidates.extend(self._scan_nexus())
        return candidates

    # ── Engram ──────────────────────────────────────────────────────────────

    def _engram_connect(self):
        """Return sqlite3 connection to Engram DB, or None if unavailable."""
        if not ENGRAM_DB_PATH.exists():
            return None
        try:
            return sqlite3.connect(str(ENGRAM_DB_PATH))
        except Exception:
            return None

    def _scan_engram_new_facts(self) -> list[dict]:
        """Facts stored in the last N hours that match operator interests."""
        conn = self._engram_connect()
        if not conn:
            return []
        candidates = []
        try:
            cutoff = time.time() - self.lookback_hours * 3600
            cur = conn.execute(
                "SELECT key, content, namespace, tags FROM memories "
                "WHERE created_at > ? ORDER BY created_at DESC LIMIT 20",
                (cutoff,)
            )
            for row in cur.fetchall():
                key, content, ns, tags = row
                score = _matches_interests(content, self.interests)
                if score >= 0.3:
                    candidates.append({
                        "type": "engram_new_fact",
                        "score": score,
                        "source": "Engram",
                        "data": {
                            "key": key,
                            "content": content[:300],
                            "namespace": ns,
                            "tags": tags,
                        },
                    })
        except Exception:
            pass
        finally:
            conn.close()
        return candidates

    def _scan_engram_hub_nodes(self) -> list[dict]:
        """Zettelkasten nodes with 3+ links the operator hasn't seen."""
        conn = self._engram_connect()
        if not conn:
            return []
        candidates = []
        try:
            cur = conn.execute(
                "SELECT key, content, link_count FROM memories "
                "WHERE link_count >= 3 AND surfaced_at IS NULL "
                "ORDER BY link_count DESC LIMIT 5"
            )
            for row in cur.fetchall():
                key, content, link_count = row
                candidates.append({
                    "type": "engram_hub_node",
                    "score": min(1.0, 0.4 + link_count * 0.1),
                    "source": "Engram",
                    "data": {
                        "key": key,
                        "content": content[:300],
                        "link_count": link_count,
                    },
                })
        except Exception:
            pass
        finally:
            conn.close()
        return candidates

    def _scan_engram_needs_review(self) -> list[dict]:
        """Facts flagged as contradictions or needing operator review."""
        conn = self._engram_connect()
        if not conn:
            return []
        candidates = []
        try:
            cur = conn.execute(
                "SELECT key, content, namespace FROM memories "
                "WHERE tags LIKE '%needs_review%' "
                "ORDER BY updated_at DESC LIMIT 5"
            )
            for row in cur.fetchall():
                key, content, ns = row
                candidates.append({
                    "type": "engram_needs_review",
                    "score": 0.7,
                    "source": "Engram",
                    "data": {
                        "key": key,
                        "content": content[:300],
                        "namespace": ns,
                    },
                })
        except Exception:
            pass
        finally:
            conn.close()
        return candidates

    # ── Pulse temporal ──────────────────────────────────────────────────────

    def _scan_pulse_temporal(self) -> list[dict]:
        """Time-based triggers: stale topics, weekly wrap, open windows."""
        candidates = []
        markers = _load_json(PULSE_MARKERS) or {}
        now_ts = time.time()

        # Check for topics untouched for > 7 days
        for marker, ts in markers.items():
            if marker.startswith("topic_"):
                elapsed_days = (now_ts - ts) / 86400
                if elapsed_days > 7:
                    topic = marker[6:]
                    candidates.append({
                        "type": "pulse_stale_topic",
                        "score": min(0.8, 0.4 + elapsed_days * 0.05),
                        "source": "Pulse",
                        "data": {
                            "topic": topic,
                            "elapsed_days": round(elapsed_days, 1),
                        },
                    })

        # Friday afternoon check-in (if it's Friday 3-6 PM)
        now_dt = datetime.now()
        if now_dt.weekday() == 4 and 15 <= now_dt.hour < 18:
            last_weekly = _pulse_since_seconds("synapse_weekly_recap")
            if last_weekly is None or last_weekly > 6 * 86400:
                candidates.append({
                    "type": "pulse_weekly_recap",
                    "score": 0.65,
                    "source": "Pulse",
                    "data": {"day": "Friday", "hour": now_dt.hour},
                })

        return candidates

    # ── Sentinel ────────────────────────────────────────────────────────────

    def _scan_sentinel(self) -> list[dict]:
        """Resolved infrastructure issues worth mentioning."""
        state = _load_json(SENTINEL_STATE)
        if not state:
            return []
        candidates = []
        resolved = state.get("recently_resolved", [])
        for event in resolved:
            if not event.get("notified_synapse", False):
                candidates.append({
                    "type": "sentinel_resolved",
                    "score": 0.7,
                    "source": "Sentinel",
                    "data": {
                        "host": event.get("host", "unknown"),
                        "issue": event.get("issue", ""),
                        "resolved_at": event.get("resolved_at", ""),
                    },
                })
        return candidates

    # ── Vigil ────────────────────────────────────────────────────────────────

    def _scan_vigil(self) -> list[dict]:
        """News/briefing items matching declared interests."""
        state = _load_json(VIGIL_STATE)
        if not state:
            return []
        candidates = []
        items = state.get("undelivered_items", [])
        for item in items[:5]:
            title = item.get("title", "")
            score = _matches_interests(title, self.interests)
            if score >= 0.3:
                candidates.append({
                    "type": "vigil_news",
                    "score": score,
                    "source": "Vigil",
                    "data": {
                        "title": title,
                        "source": item.get("source", ""),
                        "url": item.get("url", ""),
                    },
                })
        return candidates

    # ── Vector ───────────────────────────────────────────────────────────────

    def _scan_vector(self) -> list[dict]:
        """Completed dev tasks not yet acknowledged by operator."""
        state = _load_json(VECTOR_STATE)
        if not state:
            return []
        candidates = []
        tasks = state.get("completed_unacked", [])
        for task in tasks[:3]:
            candidates.append({
                "type": "vector_completed",
                "score": 0.6,
                "source": "Vector",
                "data": {
                    "task": task.get("title", ""),
                    "completed_at": task.get("completed_at", ""),
                    "id": task.get("id", ""),
                },
            })
        return candidates

    # ── Nexus ────────────────────────────────────────────────────────────────

    def _scan_nexus(self) -> list[dict]:
        """Agent messages in Nexus inbox awaiting operator attention."""
        if not NEXUS_DB_PATH.exists():
            return []
        candidates = []
        try:
            conn = sqlite3.connect(str(NEXUS_DB_PATH))
            cur = conn.execute(
                "SELECT id, sender, subject, created_at FROM messages "
                "WHERE status = 'unread' AND recipient = 'lumina' "
                "ORDER BY created_at DESC LIMIT 5"
            )
            for row in cur.fetchall():
                msg_id, sender, subject, created_at = row
                candidates.append({
                    "type": "nexus_pending",
                    "score": 0.65,
                    "source": "Nexus",
                    "data": {
                        "id": msg_id,
                        "sender": sender,
                        "subject": subject,
                        "created_at": created_at,
                    },
                })
            conn.close()
        except Exception:
            pass
        return candidates
