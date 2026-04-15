#!/usr/bin/env python3
"""
project_ideas.py — Evolving project ideas skill
MooseNet · Document addenda ADD.4 implementation

Queries Engram, Nexus, and Sentinel for recent signals, then uses local Qwen
to generate 3-5 ranked project ideas. Runs at 2AM via skill-evolution timer.
Stores results in Engram namespace: project-ideas.

Cost model:
  - Signal collection: Python, $0
  - Idea generation: local Qwen, $0 (fallback: template, $0)
  - Storage to Engram: Python DB write, $0

Usage:
    python3 project_ideas.py [--dry-run] [--verbose]

Skills format: stores each idea as an Engram entry with namespace=project-ideas.
"""

import argparse
import json
import os
import sqlite3
import sys
import time
import urllib.request
from datetime import datetime, timedelta
from pathlib import Path

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------

ENGRAM_DB_PATH = Path(os.environ.get("ENGRAM_DB_PATH", "/opt/lumina-fleet/engram/engram.db"))
NEXUS_DB_PATH  = Path(os.environ.get("NEXUS_DB_PATH", "/opt/lumina-fleet/nexus/nexus.db"))
SENTINEL_STATE = Path(os.environ.get("SENTINEL_STATE_PATH", "/opt/lumina-fleet/sentinel/state.json"))
OLLAMA_URL     = os.environ.get("OLLAMA_BASE_URL", "")
OLLAMA_MODEL   = os.environ.get("SKILL_LLM_MODEL", "qwen2.5:7b")

NAMESPACE = "project-ideas"


# ---------------------------------------------------------------------------
# Signal collection (Python, $0)
# ---------------------------------------------------------------------------

def _load_json(path: Path):
    try:
        with open(path) as f:
            return json.load(f)
    except Exception:
        return None


def collect_engram_signals(lookback_days: int = 30) -> list[str]:
    """Collect recent Engram facts + hub nodes as signal strings."""
    if not ENGRAM_DB_PATH.exists():
        return []
    signals = []
    try:
        conn = sqlite3.connect(str(ENGRAM_DB_PATH))
        cutoff = time.time() - lookback_days * 86400

        # Recent facts
        cur = conn.execute(
            "SELECT content FROM memories WHERE created_at > ? ORDER BY created_at DESC LIMIT 30",
            (cutoff,),
        )
        for (content,) in cur.fetchall():
            signals.append(content[:200])

        # Hub nodes (recurring topics)
        cur = conn.execute(
            "SELECT content, link_count FROM memories WHERE link_count >= 3 ORDER BY link_count DESC LIMIT 10"
        )
        for content, link_count in cur.fetchall():
            signals.append(f"[recurring x{link_count}] {content[:200]}")

        conn.close()
    except Exception:
        pass
    return signals


def collect_nexus_topics() -> list[str]:
    """Extract recurring message topics from Nexus inbox."""
    if not NEXUS_DB_PATH.exists():
        return []
    topics = []
    try:
        conn = sqlite3.connect(str(NEXUS_DB_PATH))
        cur = conn.execute(
            "SELECT subject FROM messages WHERE created_at > ? ORDER BY created_at DESC LIMIT 20",
            (time.time() - 30 * 86400,),
        )
        for (subject,) in cur.fetchall():
            topics.append(subject[:100])
        conn.close()
    except Exception:
        pass
    return topics


def collect_sentinel_issues() -> list[str]:
    """Get recurring infrastructure issues from Sentinel state."""
    state = _load_json(SENTINEL_STATE)
    if not state:
        return []
    issues = []
    for event in state.get("recurring_issues", [])[:5]:
        issues.append(event.get("description", str(event))[:150])
    return issues


# ---------------------------------------------------------------------------
# Idea generation
# ---------------------------------------------------------------------------

def _generate_via_ollama(signals: list[str], topics: list[str], issues: list[str]) -> list[dict]:
    """Generate project ideas via local Qwen. Returns list of idea dicts."""
    context = []
    if signals:
        context.append("Recent memory signals:\n" + "\n".join(f"- {s}" for s in signals[:15]))
    if topics:
        context.append("Recurring message topics:\n" + "\n".join(f"- {t}" for t in topics[:8]))
    if issues:
        context.append("Recurring infrastructure issues:\n" + "\n".join(f"- {i}" for i in issues[:5]))

    if not context:
        return []

    prompt = f"""You are Lumina, an AI assistant for a homelab + AI enthusiast (Peter).

Based on the signals below, generate 3-5 ranked project ideas. Each idea should be:
- Actionable and specific (not vague)
- Relevant to the signals (homelab, AI/ML, productivity, drones, hockey stats, etc.)
- Ranked 1 (highest value) to 5

Signals:
{chr(10).join(context)}

Return ONLY valid JSON in this format:
[
  {{
    "rank": 1,
    "title": "Short project title",
    "description": "1-2 sentence description of what to build and why",
    "effort": "small|medium|large",
    "category": "homelab|ai-ml|productivity|data|lifestyle|infrastructure"
  }}
]"""

    try:
        payload = json.dumps({
            "model": OLLAMA_MODEL,
            "prompt": prompt,
            "stream": False,
            "options": {"num_predict": 600, "temperature": 0.7},
        }).encode()
        req = urllib.request.Request(
            f"{OLLAMA_URL}/api/generate",
            data=payload,
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        with urllib.request.urlopen(req, timeout=30) as resp:
            result = json.loads(resp.read())
            text = result.get("response", "").strip()
            # Extract JSON array from response
            start = text.find("[")
            end = text.rfind("]") + 1
            if start >= 0 and end > start:
                return json.loads(text[start:end])
    except Exception:
        pass
    return []


def _generate_fallback_ideas() -> list[dict]:
    """Template-based fallback when Ollama is unavailable."""
    return [
        {
            "rank": 1,
            "title": "Ollama model health monitor",
            "description": "Automate checks for Ollama availability on VM901/ollama-cpu-host. Alert via Synapse when models go offline.",
            "effort": "small",
            "category": "infrastructure",
        },
        {
            "rank": 2,
            "title": "Synapse feedback dashboard",
            "description": "Add thumbs-up/down UI in Soma for Synapse messages to close the feedback loop.",
            "effort": "medium",
            "category": "productivity",
        },
        {
            "rank": 3,
            "title": "Weekly homelab cost digest",
            "description": "Automate weekly OpenRouter + Infisical cost summaries into a Vigil briefing section.",
            "effort": "small",
            "category": "homelab",
        },
    ]


# ---------------------------------------------------------------------------
# Engram storage
# ---------------------------------------------------------------------------

def store_ideas(ideas: list[dict], dry_run: bool = False):
    """Write each idea as an Engram memory entry."""
    if not ENGRAM_DB_PATH.exists():
        if not dry_run:
            print(f"[project_ideas] Engram DB not found at {ENGRAM_DB_PATH}", file=sys.stderr)
        return

    if dry_run:
        return

    try:
        conn = sqlite3.connect(str(ENGRAM_DB_PATH))
        now = time.time()
        run_date = datetime.now().strftime("%Y-%m-%d")

        for idea in ideas:
            key = f"project-idea-{run_date}-rank{idea['rank']}"
            content = json.dumps(idea)
            conn.execute(
                """INSERT OR REPLACE INTO memories
                   (key, namespace, content, tags, created_at, updated_at)
                   VALUES (?, ?, ?, ?, ?, ?)""",
                (
                    key,
                    NAMESPACE,
                    content,
                    json.dumps(["project-idea", idea.get("category", ""), f"rank-{idea['rank']}"]),
                    now,
                    now,
                ),
            )
        conn.commit()
        conn.close()
    except Exception as e:
        print(f"[project_ideas] Failed to store ideas: {e}", file=sys.stderr)


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(description="Generate project ideas from Engram/Nexus signals")
    parser.add_argument("--dry-run", action="store_true", help="Print ideas without storing")
    parser.add_argument("--verbose", action="store_true", help="Show collected signals")
    args = parser.parse_args()

    print(f"[project_ideas] Starting at {datetime.now().strftime('%Y-%m-%d %H:%M')}")

    # Collect signals
    engram_signals = collect_engram_signals(lookback_days=30)
    nexus_topics   = collect_nexus_topics()
    sentinel_issues = collect_sentinel_issues()

    if args.verbose:
        print(f"[project_ideas] Signals: {len(engram_signals)} Engram, "
              f"{len(nexus_topics)} Nexus, {len(sentinel_issues)} Sentinel")

    # Generate ideas
    ideas = _generate_via_ollama(engram_signals, nexus_topics, sentinel_issues)
    if not ideas:
        print("[project_ideas] Ollama unavailable or no ideas generated — using fallback templates")
        ideas = _generate_fallback_ideas()

    # Output
    print(f"[project_ideas] Generated {len(ideas)} ideas:")
    for idea in ideas:
        print(f"  #{idea['rank']} [{idea['effort']}] {idea['title']}")
        print(f"       {idea['description']}")

    # Store
    if args.dry_run:
        print("[project_ideas] dry-run — skipping Engram storage")
    else:
        store_ideas(ideas)
        print(f"[project_ideas] Stored to Engram namespace: {NAMESPACE}")


if __name__ == "__main__":
    main()
