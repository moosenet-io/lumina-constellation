#!/usr/bin/env python3
"""
Crucible — Learning & Skills Tracker
Tracks courses, books, certs, hobbies, and reading queue.
All data in Engram (sqlite-vec). No external backend needed.
fleet-host /opt/lumina-fleet/crucible/crucible.py
"""

import os, sys, json, argparse, re
from datetime import datetime, date, timedelta
from pathlib import Path

sys.path.insert(0, '/opt/lumina-fleet/engram')
import engram

OUTPUT_DIR = Path('/opt/lumina-fleet/crucible/output/html')
OUTPUT_DIR.mkdir(parents=True, exist_ok=True)


def _load_env():
    env_file = Path('/opt/lumina-fleet/axon/.env')
    if env_file.exists():
        for line in env_file.read_text().splitlines():
            if '=' in line and not line.startswith('#'):
                k, v = line.split('=', 1)
                os.environ.setdefault(k.strip(), v.strip())
    engram.LLM_KEY = os.environ.get('LITELLM_MASTER_KEY', engram.LLM_KEY)


def _slug(name: str) -> str:
    return re.sub(r'[^a-z0-9-]', '-', name.lower()).strip('-')[:50]


def create_track(name: str, track_type: str, goal: str, milestones: list = None, target_date: str = '') -> dict:
    """Create a new learning track."""
    slug = _slug(name)
    track = {
        'name': name, 'type': track_type, 'goal': goal, 'slug': slug,
        'milestones': milestones or [], 'progress': 0, 'progress_unit': '',
        'start_date': date.today().isoformat(), 'target_date': target_date,
        'streak_days': 0, 'last_session': None, 'status': 'active',
        'sessions': [], 'notes': ''
    }
    engram.store(f'crucible/tracks/{slug}', json.dumps(track), layer='kb', tags=['crucible', 'track', track_type])
    engram.journal(agent='crucible', action='track_created', outcome=f'Created {track_type} track: {name}')
    return track


def log_session(track_slug: str, progress_update: str, notes: str = '', duration_min: int = 0) -> dict:
    """Log a learning session for a track."""
    key = f'crucible/tracks/{track_slug}'
    # Load track
    import sqlite3
    import sqlite_vec as sv
    db_path = os.environ.get('ENGRAM_DB_PATH', '/opt/lumina-fleet/engram/engram.db')
    conn = sqlite3.connect(db_path)
    conn.enable_load_extension(True); sv.load(conn); conn.enable_load_extension(False)
    row = conn.execute('SELECT content FROM knowledge_base WHERE key=?', (key,)).fetchone()
    conn.close()
    if not row:
        return {'error': f'Track not found: {track_slug}'}

    track = json.loads(row[0])
    today = date.today().isoformat()

    # Update streak
    if track.get('last_session') == (date.today() - timedelta(days=1)).isoformat():
        track['streak_days'] = track.get('streak_days', 0) + 1
    elif track.get('last_session') != today:
        track['streak_days'] = 1
    track['last_session'] = today

    # Add session
    session = {'date': today, 'progress': progress_update, 'notes': notes, 'duration_min': duration_min}
    track['sessions'] = track.get('sessions', [])[-49:] + [session]  # keep last 50

    # Update in Engram
    engram.store(key, json.dumps(track), layer='kb', tags=['crucible', 'track', track['type']])
    engram.journal(agent='crucible', action='session_logged',
                   outcome=f'{track["name"]}: {progress_update} ({duration_min}min)',
                   context=f'streak={track["streak_days"]}days')

    # Check milestone notifications
    result = {'track': track['name'], 'session': session, 'streak': track['streak_days']}
    if track['streak_days'] in (7, 14, 30, 60, 100):
        result['milestone'] = f"{track['streak_days']}-day learning streak!"
    return result


def get_status(track_slug: str = None) -> dict:
    """Get status of one or all active tracks."""
    import sqlite3, sqlite_vec as sv
    db_path = os.environ.get('ENGRAM_DB_PATH', '/opt/lumina-fleet/engram/engram.db')
    conn = sqlite3.connect(db_path)
    conn.enable_load_extension(True); sv.load(conn); conn.enable_load_extension(False)

    if track_slug:
        row = conn.execute('SELECT content FROM knowledge_base WHERE key=?', (f'crucible/tracks/{track_slug}',)).fetchone()
        conn.close()
        return json.loads(row[0]) if row else {'error': f'Track not found: {track_slug}'}

    rows = conn.execute("SELECT key, content FROM knowledge_base WHERE key LIKE 'crucible/tracks/%'").fetchall()
    conn.close()
    tracks = [json.loads(r[1]) for r in rows]
    active = [t for t in tracks if t.get('status') == 'active']
    return {'total': len(tracks), 'active': len(active), 'tracks': active}


def get_streak() -> dict:
    """Get overall learning streak across all tracks."""
    import sqlite3, sqlite_vec as sv
    db_path = os.environ.get('ENGRAM_DB_PATH', '/opt/lumina-fleet/engram/engram.db')
    conn = sqlite3.connect(db_path)
    conn.enable_load_extension(True); sv.load(conn); conn.enable_load_extension(False)
    rows = conn.execute("SELECT created_at FROM activity_journal WHERE action='session_logged' ORDER BY created_at DESC LIMIT 100").fetchall()
    conn.close()

    dates = sorted(set(r[0][:10] for r in rows), reverse=True)
    streak = 0
    today = date.today()
    for i, d in enumerate(dates):
        expected = (today - timedelta(days=i)).isoformat()
        if d == expected:
            streak += 1
        else:
            break
    return {'current_streak': streak, 'recent_active_days': len(dates[:30]), 'sessions_total': len(rows)}


def list_tracks(type_filter: str = '', active_only: bool = True) -> list:
    import sqlite3, sqlite_vec as sv
    db_path = os.environ.get('ENGRAM_DB_PATH', '/opt/lumina-fleet/engram/engram.db')
    conn = sqlite3.connect(db_path)
    conn.enable_load_extension(True); sv.load(conn); conn.enable_load_extension(False)
    rows = conn.execute("SELECT key, content FROM knowledge_base WHERE key LIKE 'crucible/tracks/%'").fetchall()
    conn.close()
    tracks = [json.loads(r[1]) for r in rows]
    if type_filter:
        tracks = [t for t in tracks if t.get('type') == type_filter]
    if active_only:
        tracks = [t for t in tracks if t.get('status') == 'active']
    return tracks


def add_to_reading_queue(title_or_url: str, priority: str = 'normal', notes: str = '') -> dict:
    slug = _slug(title_or_url[:40])
    item = {'title': title_or_url, 'slug': slug, 'priority': priority, 'notes': notes,
            'added': date.today().isoformat(), 'status': 'unread'}
    engram.store(f'crucible/reading/{slug}', json.dumps(item), layer='kb', tags=['crucible', 'reading', priority])
    return {'added': title_or_url, 'slug': slug}


def get_reading_list(status_filter: str = 'unread') -> list:
    import sqlite3, sqlite_vec as sv
    db_path = os.environ.get('ENGRAM_DB_PATH', '/opt/lumina-fleet/engram/engram.db')
    conn = sqlite3.connect(db_path)
    conn.enable_load_extension(True); sv.load(conn); conn.enable_load_extension(False)
    rows = conn.execute("SELECT content FROM knowledge_base WHERE key LIKE 'crucible/reading/%'").fetchall()
    conn.close()
    items = [json.loads(r[0]) for r in rows]
    if status_filter:
        items = [i for i in items if i.get('status') == status_filter]
    return sorted(items, key=lambda x: (x.get('priority','normal') != 'urgent', x.get('added','')))


def mark_reading_done(slug: str, notes: str = '') -> dict:
    import sqlite3, sqlite_vec as sv
    db_path = os.environ.get('ENGRAM_DB_PATH', '/opt/lumina-fleet/engram/engram.db')
    conn = sqlite3.connect(db_path)
    conn.enable_load_extension(True); sv.load(conn); conn.enable_load_extension(False)
    row = conn.execute('SELECT content FROM knowledge_base WHERE key=?', (f'crucible/reading/{slug}',)).fetchone()
    conn.close()
    if not row: return {'error': f'Not found: {slug}'}
    item = json.loads(row[0])
    item['status'] = 'read'; item['completed'] = date.today().isoformat(); item['completion_notes'] = notes
    engram.store(f'crucible/reading/{slug}', json.dumps(item), layer='kb', tags=['crucible', 'reading', 'done'])
    engram.journal(agent='crucible', action='article_read', outcome=f'Read: {item["title"]}', context=notes[:100])
    return {'marked_done': item['title']}


def log_hobby(project: str, entry_type: str, date_str: str, location: str = '', notes: str = '') -> dict:
    slug = _slug(f'{project}-{date_str}')
    entry = {'project': project, 'type': entry_type, 'date': date_str,
             'location': location, 'notes': notes, 'logged_at': datetime.utcnow().isoformat() + 'Z'}
    engram.store(f'crucible/hobbies/{_slug(project)}/{date_str}', json.dumps(entry),
                layer='kb', tags=['crucible', 'hobby', _slug(project), entry_type])
    engram.journal(agent='crucible', action=f'hobby_logged',
                   outcome=f'{project}: {entry_type} at {location or "home"}', context=notes[:100])
    return entry


def generate_dashboard() -> str:
    """Generate /learning/index.html dashboard."""
    status = get_status()
    streak = get_streak()
    reading = get_reading_list('unread')[:5]

    tracks_html = ''
    for t in status.get('tracks', []):
        sessions_count = len(t.get('sessions', []))
        last = t.get('last_session', 'never')
        streak_days = t.get('streak_days', 0)
        tracks_html += f'''
        <div class="track-card">
            <div class="track-header">
                <span class="track-icon">{"📚" if t["type"]=="book" else "🎓" if t["type"]=="course" else "🏆" if t["type"]=="cert" else "🛠"}</span>
                <div class="track-info">
                    <div class="track-name">{t["name"]}</div>
                    <div class="track-goal">{t["goal"]}</div>
                </div>
                <span class="track-streak">🔥 {streak_days}d</span>
            </div>
            <div class="track-meta">{sessions_count} sessions · last: {last}</div>
        </div>'''

    reading_html = ''.join(f'<li>{"⚡" if i.get("priority")=="urgent" else "📖"} {i["title"][:60]}</li>' for i in reading)

    html = f'''<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Crucible — Learning</title>
<style>
body{{font-family:system-ui,sans-serif;background:#111;color:#e5e5e5;padding:16px;max-width:640px;margin:0 auto}}
h1{{font-size:1.4em;margin-bottom:4px}}.subtitle{{color:#888;font-size:.85em;margin-bottom:20px}}
.streak-box{{background:#1a1a1a;border-radius:8px;padding:14px;margin-bottom:16px;text-align:center}}
.streak-num{{font-size:2.5em;font-weight:700;color:#F59E0B}}
.streak-label{{color:#888;font-size:.85em}}
.track-card{{background:#1a1a1a;border-radius:8px;padding:12px;margin-bottom:10px}}
.track-header{{display:flex;align-items:center;gap:10px;margin-bottom:6px}}
.track-icon{{font-size:1.2em}}
.track-info{{flex:1}}
.track-name{{font-weight:600;font-size:.95em}}
.track-goal{{color:#888;font-size:.8em}}
.track-streak{{font-size:.85em;color:#F59E0B}}
.track-meta{{color:#666;font-size:.75em}}
.section-title{{color:#aaa;font-size:.8em;font-weight:600;margin:16px 0 8px;text-transform:uppercase;letter-spacing:.05em}}
.reading-list{{list-style:none;padding:0;margin:0}}
.reading-list li{{background:#1a1a1a;border-radius:6px;padding:8px 12px;margin-bottom:6px;font-size:.85em}}
.footer{{text-align:center;color:#555;font-size:.75em;margin-top:20px}}
</style></head><body>
<h1>📚 Crucible</h1>
<div class="subtitle">Learning & Skills · {date.today().strftime('%B %-d, %Y')}</div>
<div class="streak-box">
    <div class="streak-num">🔥 {streak["current_streak"]}</div>
    <div class="streak-label">day streak · {streak["sessions_total"]} total sessions</div>
</div>
<div class="section-title">Active Tracks ({status["active"]})</div>
{tracks_html or '<p style="color:#555;font-size:.85em">No active tracks yet. Ask Lumina to create one!</p>'}
<div class="section-title">Reading Queue ({len(reading)} unread)</div>
<ul class="reading-list">{reading_html or '<li style="color:#555">Queue is empty</li>'}</ul>
<div class="footer">Crucible v1.0 · fleet-host · <a href="http://YOUR_FLEET_SERVER_IP/status/" style="color:#3B82F6">System Status</a></div>
</body></html>'''

    path = OUTPUT_DIR / 'index.html'
    path.write_text(html)
    return str(path)


if __name__ == '__main__':
    _load_env()
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest='cmd')

    p = sub.add_parser('create'); p.add_argument('--name', required=True); p.add_argument('--type', required=True); p.add_argument('--goal', required=True); p.add_argument('--target-date', default='')
    p = sub.add_parser('log'); p.add_argument('--track', required=True); p.add_argument('--progress', required=True); p.add_argument('--notes', default=''); p.add_argument('--duration', type=int, default=0)
    p = sub.add_parser('status'); p.add_argument('--track', default='')
    p = sub.add_parser('streak')
    p = sub.add_parser('tracks'); p.add_argument('--type', default=''); p.add_argument('--all', action='store_true')
    p = sub.add_parser('reading-add'); p.add_argument('--title', required=True); p.add_argument('--priority', default='normal'); p.add_argument('--notes', default='')
    p = sub.add_parser('reading-list'); p.add_argument('--status', default='unread')
    p = sub.add_parser('reading-done'); p.add_argument('--slug', required=True); p.add_argument('--notes', default='')
    p = sub.add_parser('hobby'); p.add_argument('--project', required=True); p.add_argument('--type', required=True); p.add_argument('--date', default=str(date.today())); p.add_argument('--location', default=''); p.add_argument('--notes', default='')
    p = sub.add_parser('dashboard')

    args = parser.parse_args()
    if args.cmd == 'create':
        print(json.dumps(create_track(args.name, args.type, args.goal, target_date=args.target_date)))
    elif args.cmd == 'log':
        print(json.dumps(log_session(args.track, args.progress, args.notes, args.duration)))
    elif args.cmd == 'status':
        print(json.dumps(get_status(args.track or None)))
    elif args.cmd == 'streak':
        print(json.dumps(get_streak()))
    elif args.cmd == 'tracks':
        print(json.dumps(list_tracks(args.type, not args.all)))
    elif args.cmd == 'reading-add':
        print(json.dumps(add_to_reading_queue(args.title, args.priority, args.notes)))
    elif args.cmd == 'reading-list':
        print(json.dumps(get_reading_list(args.status)))
    elif args.cmd == 'reading-done':
        print(json.dumps(mark_reading_done(args.slug, args.notes)))
    elif args.cmd == 'hobby':
        print(json.dumps(log_hobby(args.project, args.type, args.date, args.location, args.notes)))
    elif args.cmd == 'dashboard':
        print(generate_dashboard())
