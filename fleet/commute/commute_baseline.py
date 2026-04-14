"""
Commute baseline tracking using Engram.
Stores daily commute times and computes rolling average.
"""

import sys, os, json, argparse
from datetime import datetime, date, timedelta
from pathlib import Path

sys.path.insert(0, '/opt/lumina-fleet/engram')
import engram

DIRECTIONS = ('morning', 'evening')
ALERT_THRESHOLD = 1.25  # alert when current > baseline * 1.25


def _load_env():
    env_file = Path('/opt/lumina-fleet/axon/.env')
    if env_file.exists():
        for line in env_file.read_text().splitlines():
            if '=' in line and not line.startswith('#'):
                k, v = line.split('=', 1)
                os.environ.setdefault(k.strip(), v.strip())
    engram.LLM_KEY = os.environ.get('LITELLM_MASTER_KEY', engram.LLM_KEY)


def store_commute_time(direction: str, minutes: int, date_str: str = None) -> bool:
    """Store today's commute time in Engram activity journal."""
    if date_str is None:
        date_str = date.today().isoformat()
    key = f'commute/{direction}/{date_str}'
    content = json.dumps({'direction': direction, 'minutes': minutes, 'date': date_str})

    # Store as KB entry (queryable)
    ok = engram.store(key, content, layer='kb', tags=['commute', direction, date_str])

    # Also journal it
    engram.journal(
        agent='sentinel',
        action=f'commute_recorded',
        outcome=f'{direction} commute: {minutes} min on {date_str}',
        context=f'direction={direction}'
    )
    return ok


def get_baseline(direction: str, days: int = 14) -> float:
    """Get rolling average commute time for a direction over last N days."""
    results = []
    today = date.today()

    for i in range(days):
        check_date = (today - timedelta(days=i)).isoformat()
        key = f'commute/{direction}/{check_date}'
        try:
            import sqlite3, sqlite_vec as sv
            db_path = os.environ.get('ENGRAM_DB_PATH', '/opt/lumina-fleet/engram/engram.db')
            conn = sqlite3.connect(db_path)
            conn.enable_load_extension(True)
            sv.load(conn)
            conn.enable_load_extension(False)
            row = conn.execute(
                'SELECT content FROM knowledge_base WHERE key=?', (key,)
            ).fetchone()
            conn.close()
            if row:
                data = json.loads(row[0])
                results.append(data['minutes'])
        except Exception:
            pass

    if not results:
        return 0.0
    return sum(results) / len(results)


def check_alert(direction: str, current_minutes: int) -> dict:
    """Check if current commute warrants an alert. Returns alert info."""
    baseline = get_baseline(direction)

    if baseline == 0:
        return {
            'alert': False,
            'current': current_minutes,
            'baseline': 0,
            'ratio': 0,
            'note': 'No baseline established yet (need 2+ weeks of data)'
        }

    ratio = current_minutes / baseline
    alert = ratio >= ALERT_THRESHOLD
    delta_min = current_minutes - baseline
    delta_pct = (ratio - 1) * 100

    return {
        'alert': alert,
        'current': current_minutes,
        'baseline': round(baseline, 1),
        'ratio': round(ratio, 2),
        'delta_minutes': round(delta_min, 1),
        'delta_pct': round(delta_pct, 1),
        'message': (
            f'Traffic alert: {direction} commute is {current_minutes} min '
            f'(normally {baseline:.0f} min, +{delta_pct:.0f}%). Consider leaving earlier.'
        ) if alert else None
    }


if __name__ == '__main__':
    _load_env()
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest='cmd')

    p = sub.add_parser('store')
    p.add_argument('--direction', required=True, choices=DIRECTIONS)
    p.add_argument('--minutes', type=int, required=True)
    p.add_argument('--date', default=None)

    p = sub.add_parser('baseline')
    p.add_argument('--direction', required=True, choices=DIRECTIONS)
    p.add_argument('--days', type=int, default=14)

    p = sub.add_parser('check')
    p.add_argument('--direction', required=True, choices=DIRECTIONS)
    p.add_argument('--current', type=int, required=True)

    args = parser.parse_args()
    if args.cmd == 'store':
        ok = store_commute_time(args.direction, args.minutes, args.date)
        print('stored' if ok else 'failed')
    elif args.cmd == 'baseline':
        b = get_baseline(args.direction, args.days)
        print(f'{args.direction} baseline ({args.days}d): {b:.1f} min')
    elif args.cmd == 'check':
        result = check_alert(args.direction, args.current)
        print(json.dumps(result, indent=2))
