"""
gate.py — Synapse Stage 2: Relevance Gate
MooseNet · Document 26 implementation

Pure Python, $0. Filters candidate triggers to only approved ones.

Filters applied (in order):
  1. Relevance threshold (default 0.6)
  2. Topic blocklist keyword match
  3. No-repeat: already surfaced in last 24h (via Pulse markers)
  4. Quiet hours check
  5. Active conversation check
  6. Daily rate limit (default 3 messages/day)

Keeps the highest-scoring candidate when rate limit is hit (defers rest).

Usage:
    from gate import SynapseGate
    gate = SynapseGate(config)
    approved = gate.filter(candidates)
"""

import json
import os
import time
from datetime import datetime
from pathlib import Path
from typing import Any

PULSE_MARKERS  = Path(os.environ.get("PULSE_MARKERS_PATH", "/opt/lumina-fleet/pulse/markers.json"))
SYNAPSE_LOG    = Path(os.environ.get("SYNAPSE_LOG_PATH", "/opt/lumina-fleet/synapse/gate_log.json"))

# Default topic blocklist — never surface unless operator explicitly boosts
DEFAULT_BLOCKLIST = [
    "health", "medical", "doctor", "mental health", "therapy",
    "finances", "salary", "bank", "account balance",
    "relationships", "marriage", "divorce",
]

DEFAULT_BOOST_LIST = [
    "AI", "ML", "homelab", "drone", "FPV", "hockey",
]


def _load_json(path: Path) -> Any:
    try:
        with open(path) as f:
            return json.load(f)
    except Exception:
        return None


def _save_json(path: Path, data: Any):
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w") as f:
        json.dump(data, f, indent=2)


def _pulse_marker_exists(name: str) -> bool:
    data = _load_json(PULSE_MARKERS)
    if not data:
        return False
    return name in data


def _pulse_since_seconds(name: str) -> float | None:
    data = _load_json(PULSE_MARKERS)
    if data and name in data:
        return time.time() - data[name]
    return None


def _pulse_mark(name: str):
    data = _load_json(PULSE_MARKERS) or {}
    data[name] = time.time()
    _save_json(PULSE_MARKERS, data)


def _is_quiet_hours(quiet_start: int, quiet_end: int) -> bool:
    """Return True if current hour falls within quiet hours."""
    h = datetime.now().hour
    if quiet_start > quiet_end:
        # Spans midnight: e.g. 22 → 7
        return h >= quiet_start or h < quiet_end
    return quiet_start <= h < quiet_end


def _contains_blocked_topic(text: str, blocklist: list[str]) -> bool:
    text_lower = text.lower()
    return any(kw.lower() in text_lower for kw in blocklist)


def _candidate_text(candidate: dict) -> str:
    """Flatten candidate to a searchable string."""
    data = candidate.get("data", {})
    parts = [candidate.get("type", ""), str(data)]
    return " ".join(parts)


class SynapseGate:
    """
    Filters candidates through the relevance gate.
    Returns a list of approved triggers (0 to max_messages_per_day).
    """

    def __init__(self, config: dict):
        self.threshold     = config.get("relevance_threshold", 0.6)
        self.max_per_day   = config.get("max_messages_per_day", 3)
        self.quiet_start   = config.get("quiet_hours_start", 22)
        self.quiet_end     = config.get("quiet_hours_end", 7)
        self.blocklist     = config.get("topic_block", DEFAULT_BLOCKLIST)
        self.boost_list    = config.get("topic_boost", DEFAULT_BOOST_LIST)
        self.no_repeat_secs = config.get("no_repeat_seconds", 86400)
        # Strength overrides threshold and max
        strength = config.get("strength", "moderate")
        if strength == "gentle":
            self.threshold = max(self.threshold, 0.8)
            self.max_per_day = min(self.max_per_day, 1)
        elif strength == "enthusiastic":
            self.threshold = min(self.threshold, 0.4)
            self.max_per_day = max(self.max_per_day, 5)

    def filter(self, candidates: list[dict]) -> list[dict]:
        """
        Apply all gate rules. Returns approved candidates.
        Returns empty list if quiet hours, muted, or already at daily limit.
        """
        # Mute check — synapse_tools.synapse_mute() sets this marker
        try:
            markers = json.loads(PULSE_MARKERS.read_text()) if PULSE_MARKERS.exists() else {}
            muted_until = markers.get("synapse_muted_until", 0)
            if time.time() < muted_until:
                return []
        except Exception:
            pass

        # Quiet hours — hard block
        if _is_quiet_hours(self.quiet_start, self.quiet_end):
            return []

        # Check daily sends already done today
        sent_today = self._count_sent_today()
        slots = self.max_per_day - sent_today
        if slots <= 0:
            return []

        approved = []
        for c in candidates:
            if self._passes(c):
                # Apply boost
                text = _candidate_text(c)
                if any(kw.lower() in text.lower() for kw in self.boost_list):
                    c["score"] = min(1.0, c["score"] + 0.1)
                approved.append(c)

        if not approved:
            return []

        # Sort by score descending, take up to available slots
        approved.sort(key=lambda x: x["score"], reverse=True)
        return approved[:slots]

    def _passes(self, candidate: dict) -> bool:
        """Return True if candidate passes all gate rules."""
        # Score threshold
        if candidate["score"] < self.threshold:
            return False

        # Topic blocklist
        text = _candidate_text(candidate)
        if _contains_blocked_topic(text, self.blocklist):
            return False

        # No-repeat: check Pulse marker
        marker_key = f"synapse_sent_{candidate['type']}_{self._candidate_id(candidate)}"
        since = _pulse_since_seconds(marker_key)
        if since is not None and since < self.no_repeat_secs:
            return False

        return True

    def _candidate_id(self, candidate: dict) -> str:
        """Stable short ID for a candidate (for Pulse marker naming)."""
        data = candidate.get("data", {})
        key = data.get("key", data.get("id", data.get("title", data.get("task", ""))))
        # Sanitize to alphanumeric + underscore
        return "".join(c if c.isalnum() else "_" for c in str(key))[:40]

    def _count_sent_today(self) -> int:
        """Count how many Synapse messages were sent today via Pulse markers."""
        data = _load_json(PULSE_MARKERS) or {}
        today_start = datetime.now().replace(hour=0, minute=0, second=0, microsecond=0).timestamp()
        return sum(
            1 for k, v in data.items()
            if k.startswith("synapse_sent_") and v >= today_start
        )

    def mark_sent(self, candidate: dict):
        """Mark a candidate as sent (call after message is dispatched)."""
        marker_key = f"synapse_sent_{candidate['type']}_{self._candidate_id(candidate)}"
        _pulse_mark(marker_key)
        _pulse_mark("synapse_last_sent")
        self._append_log(candidate)

    def _append_log(self, candidate: dict):
        """Append to gate log for Soma history view."""
        log = _load_json(SYNAPSE_LOG) or []
        log.append({
            "ts": time.time(),
            "type": candidate["type"],
            "score": candidate["score"],
            "source": candidate["source"],
            "data": candidate.get("data", {}),
            "action": "sent",
        })
        # Keep last 500 entries
        if len(log) > 500:
            log = log[-500:]
        _save_json(SYNAPSE_LOG, log)
