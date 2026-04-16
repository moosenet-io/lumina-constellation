"""
scanner.py — Synapse Stage 1: Trigger Detection
MooseNet · Document 26 implementation

Pure Python, $0. Scans all trigger sources and returns candidate list.
Each candidate: {"type": str, "score": float, "source": str, "data": dict}

Sources:
  - Engram: new facts, hub nodes, needs_review tags, session STM, 2-hop graph
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
    "AI", "ML", "machine learning", "LLM", "homelab", "virtualization",
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
        candidates.extend(self._scan_engram_session_stm())
        candidates.extend(self._scan_engram_2hop())
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
        """
        Facts stored in knowledge_base in the last N hours that match
        operator interests. Uses ISO8601 created_at column.
        """
        conn = self._engram_connect()
        if not conn:
            return []
        candidates = []
        try:
            cutoff = datetime.utcnow() - timedelta(hours=self.lookback_hours)
            cutoff_str = cutoff.strftime('%Y-%m-%dT%H:%M:%SZ')
            cur = conn.execute(
                "SELECT id, key, content, tags, source_agent FROM knowledge_base "
                "WHERE created_at > ? AND (discarded IS NULL OR discarded = 0) "
                "ORDER BY created_at DESC LIMIT 20",
                (cutoff_str,)
            )
            for row in cur.fetchall():
                kb_id, key, content, tags, source_agent = row
                score = _matches_interests(content or '', self.interests)
                if score >= 0.3:
                    candidates.append({
                        "type": "engram_new_fact",
                        "score": score,
                        "source": "Engram",
                        "data": {
                            "id": kb_id,
                            "key": key,
                            "content": (content or '')[:300],
                            "tags": tags,
                            "source_agent": source_agent,
                        },
                    })
        except Exception:
            pass
        finally:
            conn.close()
        return candidates

    def _scan_engram_hub_nodes(self) -> list[dict]:
        """
        Zettelkasten hub nodes: IDs with 3+ links in memory_links table.
        Looks up content in knowledge_base by note_id field.
        Only surfaces nodes the operator hasn't seen (no surfaced_at equivalent —
        uses pulse marker engram_hub_surfaced_{note_id}).
        """
        conn = self._engram_connect()
        if not conn:
            return []
        candidates = []
        markers = _load_json(PULSE_MARKERS) or {}
        try:
            # Count bidirectional links per note
            cur = conn.execute(
                """
                SELECT note_id, COUNT(*) as link_count
                FROM (
                    SELECT note_id_1 AS note_id FROM memory_links
                    UNION ALL
                    SELECT note_id_2 AS note_id FROM memory_links
                )
                GROUP BY note_id
                HAVING link_count >= 3
                ORDER BY link_count DESC
                LIMIT 10
                """
            )
            hub_rows = cur.fetchall()
            for note_id, link_count in hub_rows:
                # Skip if recently surfaced (within last 7 days)
                marker_key = f"engram_hub_{note_id}"
                last_surfaced = markers.get(marker_key)
                if last_surfaced and (time.time() - last_surfaced) < 7 * 86400:
                    continue

                # Look up content in knowledge_base by note_id field
                kb_cur = conn.execute(
                    "SELECT key, content FROM knowledge_base WHERE note_id = ? LIMIT 1",
                    (note_id,)
                )
                kb_row = kb_cur.fetchone()
                key = kb_row[0] if kb_row else note_id
                content = (kb_row[1] if kb_row else '') or ''

                candidates.append({
                    "type": "engram_hub_node",
                    "score": min(1.0, 0.4 + link_count * 0.1),
                    "source": "Engram",
                    "data": {
                        "note_id": note_id,
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
        """
        Facts flagged with needs_review tag in knowledge_base.
        Tags column is JSON array string like '["needs_review", "infrastructure"]'.
        """
        conn = self._engram_connect()
        if not conn:
            return []
        candidates = []
        try:
            cur = conn.execute(
                "SELECT id, key, content, source_agent FROM knowledge_base "
                "WHERE tags LIKE '%needs_review%' "
                "AND (discarded IS NULL OR discarded = 0) "
                "ORDER BY updated_at DESC LIMIT 5"
            )
            for row in cur.fetchall():
                kb_id, key, content, source_agent = row
                candidates.append({
                    "type": "engram_needs_review",
                    "score": 0.7,
                    "source": "Engram",
                    "data": {
                        "id": kb_id,
                        "key": key,
                        "content": (content or '')[:300],
                        "source_agent": source_agent,
                    },
                })
        except Exception:
            pass
        finally:
            conn.close()
        return candidates

    def _scan_engram_session_stm(self) -> list[dict]:
        """
        Session short-term memory: check memory_entries for unresolved threads.
        An entry is 'unresolved' if its session_id exists but no follow-up
        memory_entry was written for that session in the last 48 hours.
        """
        conn = self._engram_connect()
        if not conn:
            return []
        candidates = []
        try:
            cutoff = datetime.utcnow() - timedelta(hours=48)
            cutoff_str = cutoff.strftime('%Y-%m-%dT%H:%M:%S')
            # Find sessions with entries older than 48h that have a session_id
            cur = conn.execute(
                "SELECT session_id, trigger_type, project, content, created_at "
                "FROM memory_entries "
                "WHERE session_id IS NOT NULL AND session_id != '' "
                "AND created_at < ? "
                "ORDER BY created_at DESC LIMIT 10",
                (cutoff_str,)
            )
            seen_sessions = set()
            for row in cur.fetchall():
                session_id, trigger_type, project, content, created_at = row
                if session_id in seen_sessions:
                    continue
                seen_sessions.add(session_id)
                # Check if there's a more recent entry for this session
                recent_cur = conn.execute(
                    "SELECT COUNT(*) FROM memory_entries "
                    "WHERE session_id = ? AND created_at >= ?",
                    (session_id, cutoff_str)
                )
                recent_count = recent_cur.fetchone()[0]
                if recent_count == 0:
                    # Session ended without follow-up — surface as unresolved thread
                    score = _matches_interests(
                        f"{trigger_type} {project} {content or ''}",
                        self.interests
                    )
                    candidates.append({
                        "type": "engram_session_stm",
                        "score": max(0.4, score),
                        "source": "Engram",
                        "data": {
                            "session_id": session_id,
                            "trigger_type": trigger_type,
                            "project": project,
                            "content": (content or '')[:200],
                            "created_at": created_at,
                        },
                    })
                if len(candidates) >= 3:
                    break
        except Exception:
            pass
        finally:
            conn.close()
        return candidates

    def _scan_engram_2hop(self) -> list[dict]:
        """
        2-hop serendipity: find pairs of knowledge_base facts that share a
        common intermediate node in memory_links, where the two facts match
        different interests — connecting seemingly unrelated topics.

        A → B → C where A matches interest-set-1 and C matches interest-set-2.
        Score based on combined interest overlap and link strength.
        """
        conn = self._engram_connect()
        if not conn:
            return []
        candidates = []
        try:
            # Get all links (both directions)
            cur = conn.execute(
                "SELECT note_id_1, note_id_2, link_strength FROM memory_links"
            )
            links = cur.fetchall()

            # Build adjacency: note_id → set of (neighbor, strength)
            adjacency: dict[str, list[tuple[str, int]]] = {}
            for n1, n2, strength in links:
                adjacency.setdefault(n1, []).append((n2, strength))
                adjacency.setdefault(n2, []).append((n1, strength))

            # Build note content map from knowledge_base
            kb_cur = conn.execute(
                "SELECT note_id, key, content FROM knowledge_base "
                "WHERE note_id IS NOT NULL"
            )
            note_content: dict[str, tuple[str, str]] = {}
            for note_id, key, content in kb_cur.fetchall():
                note_content[note_id] = (key or '', content or '')

            # Find 2-hop connections: A → hub → C
            seen_pairs: set[frozenset] = set()
            for hub_id, hub_neighbors in adjacency.items():
                if len(hub_neighbors) < 2:
                    continue
                # Try all pairs of hub's neighbors
                for i, (node_a, str_a) in enumerate(hub_neighbors):
                    for node_c, str_c in hub_neighbors[i + 1:]:
                        if node_a == node_c:
                            continue
                        pair = frozenset([node_a, node_c])
                        if pair in seen_pairs:
                            continue
                        seen_pairs.add(pair)

                        key_a, content_a = note_content.get(node_a, ('', ''))
                        key_c, content_c = note_content.get(node_c, ('', ''))

                        if not content_a or not content_c:
                            continue

                        score_a = _matches_interests(content_a, self.interests)
                        score_c = _matches_interests(content_c, self.interests)

                        # Only surface if both endpoints have some interest match
                        # and they're different enough (serendipity)
                        if score_a >= 0.3 and score_c >= 0.3:
                            combined_score = min(1.0, (score_a + score_c) / 2 + 0.1)
                            candidates.append({
                                "type": "engram_2hop",
                                "score": combined_score,
                                "source": "Engram",
                                "data": {
                                    "node_a": node_a,
                                    "key_a": key_a,
                                    "content_a": content_a[:150],
                                    "hub": hub_id,
                                    "node_c": node_c,
                                    "key_c": key_c,
                                    "content_c": content_c[:150],
                                },
                            })
                            if len(candidates) >= 3:
                                break
                    if len(candidates) >= 3:
                        break
                if len(candidates) >= 3:
                    break

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
