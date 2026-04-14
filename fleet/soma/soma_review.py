#!/usr/bin/env python3
"""
Soma Review — Weekly conversation pattern detector.
Runs Sunday 6 PM via systemd timer.
Reads Engram journal, detects patterns, stores suggestions in soma.db.
"""

import os, sys, json, sqlite3, re
from datetime import datetime, timedelta
from pathlib import Path

sys.path.insert(0, '/opt/lumina-fleet')

DB_PATH = Path('/opt/lumina-fleet/soma/soma.db')
FLEET_DIR = Path('/opt/lumina-fleet')

# Initialize soma.db schema
def init_db():
    DB_PATH.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(DB_PATH)
    conn.executescript("""
    CREATE TABLE IF NOT EXISTS insights (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        type TEXT NOT NULL,
        title TEXT NOT NULL,
        pattern TEXT NOT NULL,
        evidence TEXT,
        status TEXT DEFAULT 'pending' CHECK(status IN ('pending','accepted','dismissed','later')),
        calx_rule TEXT,
        created_at TEXT DEFAULT (datetime('now')),
        acted_on TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_status ON insights(status);
    """)
    conn.commit()
    conn.close()

def _get_journal_entries(days_back=7):
    """Get recent journal entries from Engram DB."""
    try:
        import sqlite3 as sq
        engram_db = FLEET_DIR / 'engram/engram.db'
        if not engram_db.exists():
            return []
        conn = sq.connect(str(engram_db))
        cutoff = (datetime.utcnow() - timedelta(days=days_back)).isoformat()
        cur = conn.cursor()
        cur.execute("""
            SELECT content, created_at FROM memories
            WHERE layer='journal' AND created_at >= ?
            ORDER BY created_at DESC LIMIT 500
        """, (cutoff,))
        entries = [{'content': r[0], 'created_at': r[1]} for r in cur.fetchall()]
        conn.close()
        return entries
    except Exception:
        return []

# Pattern detector 1: Repeated questions
def detect_repeated_questions(entries):
    questions = []
    for e in entries:
        content = e.get('content', '').lower()
        if any(kw in content for kw in ['what is', 'how do', 'can you', '?']):
            questions.append(content[:100])

    from collections import Counter
    words = []
    for q in questions:
        words.extend(re.findall(r'\b\w{4,}\b', q))

    common = Counter(words).most_common(5)
    if common and common[0][1] >= 3:
        return {
            'type': 'repeated_question',
            'title': f'Frequent topic: "{common[0][0]}" ({common[0][1]}x)',
            'pattern': 'Question about same topic asked multiple times',
            'evidence': json.dumps([f'{w}: {c}x' for w, c in common[:3]]),
        }
    return None

# Pattern detector 2: Time clustering
def detect_time_clustering(entries):
    if not entries:
        return None
    hours = []
    for e in entries:
        try:
            dt = datetime.fromisoformat(e['created_at'].replace('Z', ''))
            hours.append(dt.hour)
        except Exception:
            pass

    if not hours:
        return None

    from collections import Counter
    hour_counts = Counter(hours)
    peak_hour, peak_count = hour_counts.most_common(1)[0]

    if peak_count >= len(hours) * 0.3:  # 30% of activity in one hour
        return {
            'type': 'time_clustering',
            'title': f'Peak activity at {peak_hour:02d}:00 ({peak_count} events)',
            'pattern': 'Most AI activity concentrated in specific time window',
            'evidence': json.dumps({'peak_hour': peak_hour, 'count': peak_count, 'total': len(hours)}),
        }
    return None

# Pattern detector 3: Tool gaps (common requests with no tool)
def detect_tool_gaps(entries):
    gap_patterns = [
        ('weather', 'weather information requested — no dedicated weather tool'),
        ('remind me', 'reminder requests — no reminder/todo tool deployed'),
        ('translate', 'translation requests — no translation tool'),
        ('summarize this', 'text summarization requests — consider Seer for batch processing'),
        ('send email', 'email send requests — check google_tools email is configured'),
    ]

    text = ' '.join(e.get('content', '') for e in entries).lower()
    gaps = []
    for pattern, suggestion in gap_patterns:
        if text.count(pattern) >= 2:
            gaps.append({'pattern': pattern, 'count': text.count(pattern), 'suggestion': suggestion})

    if gaps:
        return {
            'type': 'tool_gap',
            'title': f'Unmet tool need: {gaps[0]["pattern"]} ({gaps[0]["count"]}x)',
            'pattern': 'Recurring request type with no matching tool',
            'evidence': json.dumps(gaps[:3]),
        }
    return None

# Pattern detector 4: Workflow sequences
def detect_workflow_sequences(entries):
    sequence_patterns = [
        (['briefing', 'commute', 'tasks'], 'Morning routine sequence detected — consider morning_workflow macro'),
        (['health', 'steps', 'sleep'], 'Health review sequence — consider health_summary shortcut'),
        (['backup', 'test', 'status'], 'Ops check sequence — consider ops_check macro'),
    ]

    contents = [e.get('content', '').lower() for e in entries[-50:]]
    for keywords, suggestion in sequence_patterns:
        if all(any(kw in c for c in contents) for kw in keywords):
            return {
                'type': 'workflow_sequence',
                'title': f'Workflow pattern: {" → ".join(keywords)}',
                'pattern': 'Recurring sequence of related activities',
                'evidence': json.dumps({'sequence': keywords, 'suggestion': suggestion}),
            }
    return None

# Pattern detector 5: Expensive routes
def detect_expensive_routes(entries):
    """Check Myelin data for expensive cloud calls that could use local models."""
    try:
        myelin_usage = FLEET_DIR / 'myelin/output/usage.json'
        if not myelin_usage.exists():
            return None

        usage = json.loads(myelin_usage.read_text())
        per_agent = usage.get('per_agent', {})

        for agent_id, data in per_agent.items():
            cost = data.get('cost', 0)
            if cost > 0.50:  # Agent spending >50 cents today
                return {
                    'type': 'expensive_route',
                    'title': f'{agent_id} spending ${cost:.2f}/day — consider local model',
                    'pattern': 'Cloud inference for tasks that could use local Qwen',
                    'evidence': json.dumps({'agent_id': agent_id, 'daily_cost': cost}),
                }
    except Exception:
        pass
    return None

# Pattern detector 6: Disabled features
def detect_disabled_features(entries):
    """Check if frequently-used patterns reference disabled/unconfigured features."""
    text = ' '.join(e.get('content', '') for e in entries).lower()

    disabled_checks = [
        ('calendar', 'google_calendar', 'Calendar references found — is Google Calendar configured?'),
        ('email', 'google_email', 'Email references — is Gmail forwarding active?'),
        ('budget', 'ledger', 'Budget references — is Actual Budget initialized with budget_id?'),
    ]

    for topic, tool, suggestion in disabled_checks:
        if text.count(topic) >= 3:
            return {
                'type': 'disabled_feature',
                'title': f'Frequent {topic} references — {tool} check needed',
                'pattern': 'Topic discussed but corresponding tool may not be configured',
                'evidence': json.dumps({'topic': topic, 'count': text.count(topic), 'suggestion': suggestion}),
            }
    return None

def run_review(days_back=7):
    """Run all pattern detectors and store new suggestions."""
    init_db()
    entries = _get_journal_entries(days_back)

    if not entries:
        print(f'[soma_review] No journal entries found for last {days_back} days')
        return {'suggestions': 0, 'note': 'No journal data'}

    detectors = [
        detect_repeated_questions,
        detect_time_clustering,
        detect_tool_gaps,
        detect_workflow_sequences,
        detect_expensive_routes,
        detect_disabled_features,
    ]

    suggestions = []
    for detector in detectors:
        try:
            result = detector(entries)
            if result:
                suggestions.append(result)
        except Exception as e:
            print(f'[soma_review] Detector {detector.__name__} failed: {e}')

    if not suggestions:
        print(f'[soma_review] No patterns detected in {len(entries)} entries')
        return {'suggestions': 0, 'entries_analyzed': len(entries)}

    conn = sqlite3.connect(DB_PATH)
    stored = 0
    for s in suggestions:
        # Check if similar suggestion already exists (not dismissed)
        cur = conn.cursor()
        cur.execute("SELECT id FROM insights WHERE type=? AND status='pending'", (s['type'],))
        if not cur.fetchone():
            conn.execute("""
                INSERT INTO insights (type, title, pattern, evidence)
                VALUES (?, ?, ?, ?)
            """, (s['type'], s['title'], s['pattern'], s.get('evidence', '')))
            stored += 1

    conn.commit()
    conn.close()

    print(f'[soma_review] Analyzed {len(entries)} entries, stored {stored} new suggestions')
    return {'suggestions': stored, 'entries_analyzed': len(entries)}


if __name__ == '__main__':
    # Load env
    env_file = Path('/opt/lumina-fleet/axon/.env')
    if env_file.exists():
        for line in env_file.read_text().splitlines():
            if '=' in line and not line.startswith('#'):
                k, v = line.split('=', 1)
                os.environ.setdefault(k.strip(), v.strip().strip('"').strip("'"))

    import argparse
    parser = argparse.ArgumentParser()
    parser.add_argument('--days', type=int, default=7)
    parser.add_argument('--status', action='store_true')
    args = parser.parse_args()

    if args.status:
        conn = sqlite3.connect(DB_PATH)
        cur = conn.cursor()
        cur.execute("SELECT type, title, status FROM insights ORDER BY created_at DESC LIMIT 10")
        for t, title, status in cur.fetchall():
            print(f"[{status}] {t}: {title}")
        conn.close()
    else:
        result = run_review(args.days)
        print(json.dumps(result, indent=2))
