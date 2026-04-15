#!/usr/bin/env python3
"""
Odyssey — Trip Planning System
Destination research via Seer, bucket list, adventure log.
All data in Engram. HTML output on Apache /travel/
fleet-host /opt/lumina-fleet/odyssey/odyssey.py
"""

import os, sys, json, argparse, subprocess, re
from datetime import datetime, date
from pathlib import Path

sys.path.insert(0, '/opt/lumina-fleet/engram')
import engram

OUTPUT_DIR = Path('/opt/lumina-fleet/odyssey/output/html')
OUTPUT_DIR.mkdir(parents=True, exist_ok=True)

def _load_env():
    env_file = Path('/opt/lumina-fleet/axon/.env')
    if env_file.exists():
        for line in env_file.read_text().splitlines():
            if '=' in line and not line.startswith('#'):
                k, v = line.split('=', 1)
                os.environ.setdefault(k.strip(), v.strip())
    engram.LLM_KEY = os.environ.get('LITELLM_MASTER_KEY', engram.LLM_KEY)

def _slug(s):
    return re.sub(r'[^a-z0-9-]', '-', s.lower()).strip('-')[:50]


def research_destination(destination: str, dates: str = '', budget: str = '', travelers: int = 1) -> dict:
    """Trigger Seer deep research for a destination."""
    query = f"trip to {destination}"
    if dates: query += f" in {dates}"
    if budget: query += f" budget {budget}"
    if travelers > 1: query += f" for {travelers} people"

    cmd = [
        'python3', '/opt/lumina-fleet/seer/seer.py',
        '--query', f'Travel guide and trip planning: {query}',
        '--effort', 'standard',
        '--focus', 'comparison'
    ]
    env = {**os.environ}

    try:
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=300,
                               cwd='/opt/lumina-fleet/seer', env=env)
        if result.returncode == 0:
            # Parse JSON result
            for line in reversed(result.stdout.split('\n')):
                line = line.strip()
                if line.startswith('{'):
                    report = json.loads(line)
                    if 'report_id' in report:
                        # Store in Engram
                        engram.store(
                            f'travel/destinations/{_slug(destination)}',
                            f'Research report: {destination}. URL: {report["url"]}. Summary: {report.get("summary","")[:200]}',
                            layer='kb', tags=['travel', 'destination', _slug(destination)]
                        )
                        engram.journal(agent='odyssey', action='destination_researched',
                                      outcome=f'{destination}: {report.get("sources_used",0)} sources',
                                      context=f'dates={dates} budget={budget}')
                        # Update bucket list status if exists
                        _update_bucket_status(destination, 'researched')
                        return report
        return {'status': 'failed', 'error': result.stderr[:200]}
    except Exception as e:
        return {'status': 'error', 'error': str(e)}


def bucket_add(destination: str, priority: str = 'medium', season: str = '', budget: float = 0, notes: str = '') -> dict:
    """Add destination to bucket list."""
    slug = _slug(destination)
    entry = {
        'destination': destination, 'slug': slug, 'priority': priority,
        'best_season': season, 'budget': budget, 'notes': notes,
        'status': 'dream', 'added_date': date.today().isoformat(),
        'researched': False
    }
    engram.store(f'travel/bucket/{slug}', json.dumps(entry), layer='kb',
                tags=['travel', 'bucket-list', priority, destination.lower()])
    engram.journal(agent='odyssey', action='bucket_add', outcome=f'Added: {destination}',
                  context=f'priority={priority} season={season}')
    _generate_bucket_list_html()
    return {'added': destination, 'priority': priority, 'status': 'dream'}


def _update_bucket_status(destination: str, status: str):
    """Update bucket list entry status."""
    slug = _slug(destination)
    import sqlite3
    db_path = os.environ.get('ENGRAM_DB_PATH', '/opt/lumina-fleet/engram/engram.db')
    conn = sqlite3.connect(db_path)
    try:
        conn.enable_load_extension(True)
        try:
            import sqlite_vec as sv
            sv.load(conn)
        except Exception:
            pass
        conn.enable_load_extension(False)
    except Exception:
        pass
    row = conn.execute('SELECT content FROM knowledge_base WHERE key=?', (f'travel/bucket/{slug}',)).fetchone()
    conn.close()
    if row:
        entry = json.loads(row[0])
        entry['status'] = status
        if status == 'researched':
            entry['researched'] = True
        engram.store(f'travel/bucket/{slug}', json.dumps(entry), layer='kb',
                    tags=['travel', 'bucket-list', entry.get('priority', 'medium')])


def bucket_list(status_filter: str = '') -> list:
    """Get bucket list entries."""
    import sqlite3
    db_path = os.environ.get('ENGRAM_DB_PATH', '/opt/lumina-fleet/engram/engram.db')
    conn = sqlite3.connect(db_path)
    try:
        conn.enable_load_extension(True)
        try:
            import sqlite_vec as sv
            sv.load(conn)
        except Exception:
            pass
        conn.enable_load_extension(False)
    except Exception:
        pass
    rows = conn.execute("SELECT content FROM knowledge_base WHERE key LIKE 'travel/bucket/%'").fetchall()
    conn.close()
    entries = [json.loads(r[0]) for r in rows]
    if status_filter:
        entries = [e for e in entries if e.get('status') == status_filter]
    priority_order = {'urgent': 0, 'high': 1, 'medium': 2, 'low': 3}
    return sorted(entries, key=lambda x: priority_order.get(x.get('priority', 'medium'), 2))


def log_trip(destination: str, dates: str, highlights: str, rating: int = 5, cost: float = 0, notes: str = '') -> dict:
    """Log a completed trip to the adventure log."""
    slug = _slug(f'{destination}-{dates}')
    entry = {
        'destination': destination, 'dates': dates, 'highlights': highlights,
        'rating': rating, 'total_cost': cost, 'notes': notes,
        'logged_at': datetime.utcnow().isoformat() + 'Z'
    }
    engram.store(f'travel/trips/{slug}', json.dumps(entry), layer='kb',
                tags=['travel', 'trip', 'completed', _slug(destination)])
    engram.journal(agent='odyssey', action='trip_logged',
                  outcome=f'{destination} ({dates}): {rating}/5 stars',
                  context=f'cost=${cost}')
    _update_bucket_status(destination, 'completed')
    return {'logged': destination, 'rating': rating}


def update_points(program: str, balance: int, card_type: str = 'credit', tier: str = '', benefits: list = None) -> dict:
    """Store/update a loyalty program or credit card in Engram."""
    slug = _slug(program)
    card_data = {
        'program': program, 'slug': slug, 'type': card_type,
        'balance': balance, 'tier': tier,
        'benefits': benefits or [],
        'updated': date.today().isoformat()
    }
    engram.store(f'travel/cards/{slug}', json.dumps(card_data), layer='patterns',
                tags=['travel', 'loyalty', card_type, slug])
    engram.journal(agent='odyssey', action='points_updated',
                  outcome=f'{program}: {balance:,} points/miles', context=f'type={card_type}')
    return {'updated': program, 'balance': balance}

def list_cards() -> list:
    """List all stored loyalty programs and credit cards."""
    import sqlite3
    db_path = os.environ.get('ENGRAM_DB_PATH', '/opt/lumina-fleet/engram/engram.db')
    conn = sqlite3.connect(db_path)
    try:
        conn.enable_load_extension(True)
        try:
            import sqlite_vec as sv
            sv.load(conn)
        except Exception:
            pass
        conn.enable_load_extension(False)
    except Exception:
        pass
    # Cards are stored in the 'patterns' layer/table
    rows = conn.execute("SELECT content FROM patterns WHERE key LIKE 'travel/cards/%'").fetchall()
    conn.close()
    cards = [json.loads(r[0]) for r in rows]
    return sorted(cards, key=lambda x: x.get('balance', 0), reverse=True)

def optimize_trip(destination: str, spend_estimate: float) -> dict:
    """Use Mr. Wizard to recommend optimal card usage for a trip."""
    import urllib.request

    cards = list_cards()
    if not cards:
        return {'error': 'No cards in portfolio yet. Use odyssey_update_points to add cards first.'}

    cards_summary = '\n'.join(f'- {c["program"]} ({c["type"]}): {c["balance"]:,} points, tier: {c.get("tier","standard")}, benefits: {", ".join(c.get("benefits",[])[:3])}' for c in cards[:10])

    prompt = f"""Card optimization for a trip to {destination} with estimated spend of ${spend_estimate:,.0f}.

Card portfolio:
{cards_summary}

Provide:
1. Best card for flights (maximize miles/points per dollar)
2. Best card for hotels
3. Best card for dining/entertainment
4. Whether to use any existing points for this trip (and how many)
5. Total estimated points earned

Be specific about which card to use for each category."""

    try:
        data = json.dumps({'model': 'lumina-lead', 'messages': [{'role': 'user', 'content': prompt}], 'max_tokens': 400}).encode()
        req = urllib.request.Request(f'{os.environ.get("LITELLM_URL","http://YOUR_LITELLM_IP:4000")}/v1/chat/completions', data=data,
            headers={'Authorization': f'Bearer {os.environ.get("LITELLM_MASTER_KEY","")}', 'Content-Type': 'application/json'}, method='POST')
        with urllib.request.urlopen(req, timeout=60) as r:
            recommendation = json.load(r)['choices'][0]['message']['content']
        return {'destination': destination, 'spend_estimate': spend_estimate, 'recommendation': recommendation, 'cards_used': len(cards)}
    except Exception as e:
        return {'error': str(e)}


def _generate_bucket_list_html():
    """Generate /travel/bucket-list/index.html"""
    entries = bucket_list()
    (OUTPUT_DIR / 'bucket-list').mkdir(exist_ok=True)

    priority_colors = {'urgent': '#EF4444', 'high': '#F59E0B', 'medium': '#10B981', 'low': '#6B7280'}
    status_icons = {'dream': '&#x1F4AD;', 'researched': '&#x1F50D;', 'planned': '&#x1F4CB;', 'booked': '&#x2708;&#xFE0F;', 'completed': '&#x2705;'}

    rows = ''.join(f'''<tr>
        <td><strong>{e["destination"]}</strong></td>
        <td><span style="color:{priority_colors.get(e.get("priority","medium"),"#10B981")}">{e.get("priority","medium")}</span></td>
        <td>{e.get("best_season","any")}</td>
        <td>{f'${e["budget"]:,.0f}' if e.get("budget") else "&mdash;"}</td>
        <td>{status_icons.get(e.get("status","dream"),"&#x1F4AD;")} {e.get("status","dream")}</td>
        <td>{e.get("notes","")[:40]}</td>
    </tr>''' for e in entries)

    html = f'''<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Odyssey &mdash; Bucket List</title>
<style>
body{{font-family:system-ui,sans-serif;background:#111;color:#e5e5e5;padding:20px;max-width:900px;margin:0 auto}}
h1{{font-size:1.4em;margin-bottom:4px}}.subtitle{{color:#888;font-size:.85em;margin-bottom:20px}}
table{{width:100%;border-collapse:collapse;font-size:.85em}}
th{{text-align:left;padding:8px;border-bottom:2px solid #333;color:#aaa;font-weight:600}}
td{{padding:8px;border-bottom:1px solid #222}}
tr:hover td{{background:#1a1a1a}}
.footer{{text-align:center;color:#555;font-size:.75em;margin-top:20px}}
</style></head><body>
<h1>&#x2708;&#xFE0F; Odyssey &mdash; Bucket List</h1>
<div class="subtitle">{len(entries)} destinations &middot; updated {date.today()}</div>
<table>
<tr><th>Destination</th><th>Priority</th><th>Best Season</th><th>Budget</th><th>Status</th><th>Notes</th></tr>
{rows or '<tr><td colspan="6" style="color:#555;text-align:center">No destinations yet. Ask Lumina to add some!</td></tr>'}
</table>
<div class="footer">Odyssey v1.0 &middot; fleet-host &middot; <a href="http://YOUR_FLEET_SERVER_IP/" style="color:#3B82F6">Lumina Home</a></div>
</body></html>'''

    (OUTPUT_DIR / 'bucket-list' / 'index.html').write_text(html)


def generate_travel_index():
    """Generate /travel/index.html"""
    entries = bucket_list()

    # Get recent trips
    import sqlite3
    db_path = os.environ.get('ENGRAM_DB_PATH', '/opt/lumina-fleet/engram/engram.db')
    conn = sqlite3.connect(db_path)
    try:
        conn.enable_load_extension(True)
        try:
            import sqlite_vec as sv
            sv.load(conn)
        except Exception:
            pass
        conn.enable_load_extension(False)
    except Exception:
        pass
    trip_rows = conn.execute("SELECT content FROM knowledge_base WHERE key LIKE 'travel/trips/%' ORDER BY rowid DESC LIMIT 5").fetchall()
    conn.close()
    trips = [json.loads(r[0]) for r in trip_rows]

    trip_cards = ''
    for t in trips[:2]:
        trip_cards += f'<div class="card"><h3>&#x2705; {t["destination"]} ({t["dates"]})</h3><p>{t["highlights"][:80]} &middot; {t.get("rating",5)}&#x2B50;</p></div>'

    html = f'''<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Odyssey &mdash; Travel</title>
<style>
body{{font-family:system-ui,sans-serif;background:#111;color:#e5e5e5;padding:20px;max-width:640px;margin:0 auto}}
h1{{font-size:1.4em}}.subtitle{{color:#888;font-size:.85em;margin-bottom:20px}}
.card{{background:#1a1a1a;border-radius:8px;padding:14px;margin-bottom:10px}}
.card h3{{margin:0 0 6px;font-size:.95em}}
.card p{{color:#aaa;font-size:.82em;margin:0}}
a{{color:#3B82F6;text-decoration:none}}
.footer{{text-align:center;color:#555;font-size:.75em;margin-top:20px}}
</style></head><body>
<h1>&#x2708;&#xFE0F; Odyssey</h1>
<div class="subtitle">Trip planning &middot; {len(entries)} destinations in bucket list</div>
<div class="card"><h3>&#x1F4CB; <a href="bucket-list/">Bucket List ({len(entries)} destinations)</a></h3>
<p>{"Top: " + ", ".join(e["destination"] for e in entries[:3]) if entries else "No destinations yet"}</p></div>
<div class="card"><h3>&#x1F4F0; <a href="../research/">Recent Research Reports</a></h3><p>Destination research via Seer engine</p></div>
{trip_cards}
<div class="footer">Odyssey v1.0 &middot; fleet-host &middot; <a href="http://YOUR_FLEET_SERVER_IP/">Lumina Home</a></div>
</body></html>'''

    (OUTPUT_DIR / 'index.html').write_text(html)
    return str(OUTPUT_DIR / 'index.html')


if __name__ == '__main__':
    _load_env()
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest='cmd')

    p = sub.add_parser('research')
    p.add_argument('--destination', required=True)
    p.add_argument('--dates', default='')
    p.add_argument('--budget', default='')
    p.add_argument('--travelers', type=int, default=1)

    p = sub.add_parser('bucket-add')
    p.add_argument('--destination', required=True)
    p.add_argument('--priority', default='medium')
    p.add_argument('--season', default='')
    p.add_argument('--budget', type=float, default=0)
    p.add_argument('--notes', default='')

    p = sub.add_parser('bucket-list')
    p.add_argument('--status', default='')

    p = sub.add_parser('log-trip')
    p.add_argument('--destination', required=True)
    p.add_argument('--dates', required=True)
    p.add_argument('--highlights', required=True)
    p.add_argument('--rating', type=int, default=5)
    p.add_argument('--cost', type=float, default=0)

    p = sub.add_parser('update-points')
    p.add_argument('--program', required=True)
    p.add_argument('--balance', type=int, required=True)
    p.add_argument('--type', dest='card_type', default='credit')
    p.add_argument('--tier', default='')
    p.add_argument('--benefits', default='')

    p = sub.add_parser('list-cards')

    p = sub.add_parser('optimize')
    p.add_argument('--destination', required=True)
    p.add_argument('--spend', type=float, default=5000)

    p = sub.add_parser('dashboard')

    args = parser.parse_args()
    if args.cmd == 'research':
        print(json.dumps(research_destination(args.destination, args.dates, args.budget, args.travelers)))
    elif args.cmd == 'bucket-add':
        print(json.dumps(bucket_add(args.destination, args.priority, args.season, args.budget, args.notes)))
    elif args.cmd == 'bucket-list':
        print(json.dumps(bucket_list(args.status)))
    elif args.cmd == 'log-trip':
        print(json.dumps(log_trip(args.destination, args.dates, args.highlights, args.rating, args.cost)))
    elif args.cmd == 'update-points':
        benefits = [b.strip() for b in args.benefits.split(',')] if args.benefits else []
        print(json.dumps(update_points(args.program, args.balance, args.card_type, args.tier, benefits)))
    elif args.cmd == 'list-cards':
        print(json.dumps(list_cards()))
    elif args.cmd == 'optimize':
        print(json.dumps(optimize_trip(args.destination, args.spend)))
    elif args.cmd == 'dashboard':
        _generate_bucket_list_html()
        print(generate_travel_index())
    else:
        parser.print_help()
