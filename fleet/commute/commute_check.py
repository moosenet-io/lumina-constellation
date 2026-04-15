#!/usr/bin/env python3
"""
Commute Check — replaces 4 IronClaw commute routines
TomTom API + Engram baseline. Zero inference. Templated Matrix alerts.
fleet-host /opt/lumina-fleet/commute/commute_check.py
"""
import os, sys, json, argparse, sqlite3, urllib.request, urllib.parse
from datetime import datetime, date
from pathlib import Path

def _load_env():
    env_file = Path('/opt/lumina-fleet/axon/.env')
    if env_file.exists():
        for line in env_file.read_text().splitlines():
            if '=' in line and not line.startswith('#'):
                k, v = line.split('=', 1)
                os.environ.setdefault(k.strip(), v.strip())

TOMTOM_KEY = os.environ.get('TOMTOM_API_KEY', '')
ALERT_THRESHOLD = 1.25
DB_PATH = os.environ.get('ENGRAM_DB_PATH', '/opt/lumina-fleet/engram/engram.db')

# Routes (from the operator's commute)
ROUTES = {
    'morning': {
        'origin': '37.7765,-122.4172',   # SF area
        'dest': os.environ.get("COMMUTE_DEST_LATLON", ""),      # Foster City area
    },
    'evening': {
        'origin': os.environ.get("COMMUTE_DEST_LATLON", ""),
        'dest': '37.7765,-122.4172',
    }
}

def get_tomtom_time(origin, dest):
    """Get travel time in seconds from TomTom Routing API."""
    if not TOMTOM_KEY:
        return None, 'No TomTom API key'

    url = f'https://api.tomtom.com/routing/1/calculateRoute/{origin}:{dest}/json'
    params = {'key': TOMTOM_KEY, 'traffic': 'true', 'travelMode': 'car'}
    try:
        req = urllib.request.Request(url + '?' + urllib.parse.urlencode(params),
            headers={'User-Agent': 'Sentinel-Commute/1.0'})
        with urllib.request.urlopen(req, timeout=10) as r:
            d = json.load(r)
        travel_time = d['routes'][0]['summary']['travelTimeInSeconds']
        return travel_time, None
    except Exception as e:
        return None, str(e)[:80]

def get_baseline(direction):
    """Get rolling 14-day average from Engram DB."""
    try:
        import sqlite_vec
        conn = sqlite3.connect(DB_PATH)
        conn.enable_load_extension(True)
        sqlite_vec.load(conn)
        conn.enable_load_extension(False)

        rows = conn.execute(
            "SELECT content FROM knowledge_base WHERE key LIKE 'commute/%/%' ORDER BY key DESC LIMIT 14"
        ).fetchall()
        conn.close()

        times = []
        for row in rows:
            try:
                d = json.loads(row[0])
                if d.get('direction') == direction:
                    t = d.get('travel_time_seconds', 0)
                    if t > 0:
                        times.append(t)
            except Exception:
                pass

        return sum(times) / len(times) if times else 0
    except Exception:
        return 0

def store_commute_time(direction, travel_time_seconds):
    """Store today's commute time in Engram for baseline calculation."""
    try:
        sys.path.insert(0, '/opt/lumina-fleet/engram')
        import engram
        engram.LLM_KEY = os.environ.get('LITELLM_MASTER_KEY', '')
        key = f'commute/{direction}/{date.today().isoformat()}'
        engram.store(key, json.dumps({'direction': direction,
            'travel_time_seconds': travel_time_seconds,
            'date': date.today().isoformat()}),
            layer='kb', tags=['commute', direction])
    except Exception as e:
        print(f'[commute] Store failed: {e}', file=sys.stderr)

def send_matrix_alert(message):
    """Send alert via Matrix bridge bot HTTP API."""
    try:
        data = json.dumps({'message': message}).encode()
        req = urllib.request.Request('http://YOUR_MATRIX_IP:8080/send', data=data,
            headers={'Content-Type': 'application/json'}, method='POST')
        with urllib.request.urlopen(req, timeout=5) as r:
            return r.status == 200
    except Exception as e:
        print(f'[commute] Matrix failed: {e}', file=sys.stderr)
        return False

def check_commute(direction):
    _load_env()
    global TOMTOM_KEY
    TOMTOM_KEY = os.environ.get('TOMTOM_API_KEY', TOMTOM_KEY)

    route = ROUTES[direction]
    travel_time, error = get_tomtom_time(route['origin'], route['dest'])

    if travel_time is None:
        print(f'[commute] TomTom error: {error}')
        return

    travel_min = travel_time // 60
    store_commute_time(direction, travel_time)

    baseline = get_baseline(direction)
    baseline_min = baseline // 60 if baseline > 0 else 0

    if baseline > 0 and travel_time > baseline * ALERT_THRESHOLD:
        delta_pct = ((travel_time - baseline) / baseline) * 100
        msg = (f'Traffic alert: {direction} commute is {travel_min} min '
               f'(normally {baseline_min} min, +{delta_pct:.0f}%). Consider leaving earlier.')
        if send_matrix_alert(msg):
            print(f'[commute] Alert sent: {direction} {travel_min} min (+{delta_pct:.0f}%)')
        else:
            print(f'[commute] Alert failed to send')
    else:
        no_alert_reason = f'No baseline' if baseline == 0 else f'Normal traffic ({travel_min} min)'
        print(f'[commute] {direction}: {travel_min} min — {no_alert_reason}')

if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('direction', choices=['morning', 'evening'])
    args = parser.parse_args()
    check_commute(args.direction)
