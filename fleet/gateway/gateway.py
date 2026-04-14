#!/usr/bin/env python3
"""
Lumina API Gateway — aggregates all module data for Homepage dashboard.
FastAPI on CT310 port 8080. Caches responses to protect backing services.
Authentication via DASHBOARD_API_KEY header.
"""

import os
import json
import time
import ssl
import urllib.request
import urllib.parse
import sys
from datetime import datetime, date
from pathlib import Path
from typing import Optional, Dict, Any

sys.path.insert(0, '/opt/lumina-fleet')
try: from naming import display_name as _dn, constellation_name as _cn
except: _dn = lambda x: x; _cn = lambda: 'Lumina'

# FastAPI
from fastapi import FastAPI, HTTPException, Header
from fastapi.responses import JSONResponse

app = FastAPI(title='Lumina Gateway', version='1.0')

# Load env
def _load_env():
    env_file = Path('/opt/lumina-fleet/axon/.env')
    if env_file.exists():
        for line in env_file.read_text().splitlines():
            if '=' in line and not line.startswith('#'):
                k, v = line.split('=', 1)
                v = v.strip()
                # Strip surrounding quotes (single or double)
                if len(v) >= 2 and v[0] == v[-1] and v[0] in ("'", '"'):
                    v = v[1:-1]
                os.environ.setdefault(k.strip(), v)
_load_env()

DASHBOARD_API_KEY = os.environ.get('DASHBOARD_API_KEY', '')
LITELLM_KEY = os.environ.get('LITELLM_MASTER_KEY', '')
PLANE_TOKEN = os.environ.get('PLANE_TOKEN_LUMINA', '')
GITEA_TOKEN = os.environ.get('GITEA_TOKEN', '')
INBOX_DB_PASS = os.environ.get('INBOX_DB_PASS', '')

# In-memory cache: {endpoint: (data, expires_at)}
_cache: Dict[str, tuple] = {}
CACHE_TTL = {
    'health': 60, 'inbox': 60, 'tasks': 300,
    'briefing': 3600, 'commute': 300, 'weather': 1800,
    'news': 3600, 'crypto': 900, 'reports': 3600,
    'insights': 86400, 'budget': 86400, 'learning': 86400,
    'calendar': 900,
    'vehicle': 86400, 'sports': 1800,
}

def _cached(key: str, fetch_fn):
    """Return cached value or fetch fresh."""
    if key in _cache:
        data, expires = _cache[key]
        if time.time() < expires:
            return data
    data = fetch_fn()
    ttl = CACHE_TTL.get(key, 300)
    _cache[key] = (data, time.time() + ttl)
    return data

def _check_auth(x_api_key: str = ''):
    if DASHBOARD_API_KEY and x_api_key != DASHBOARD_API_KEY:
        raise HTTPException(status_code=401, detail='Unauthorized')

SSL_CTX = ssl.create_default_context()
SSL_CTX.check_hostname = False
SSL_CTX.verify_mode = ssl.CERT_NONE

def _http(url: str, headers: dict = None, timeout=8) -> dict:
    req = urllib.request.Request(url, headers=headers or {'User-Agent': 'LuminaGateway/1.0'})
    ctx = SSL_CTX if url.startswith('https') else None
    try:
        with urllib.request.urlopen(req, timeout=timeout, context=ctx) as r:
            return json.load(r)
    except Exception as e:
        return {'error': str(e)}


# ── ENDPOINTS ────────────────────────────────────────────────────────────────

@app.get('/api/health')
def get_health(x_api_key: str = Header(default='')):
    _check_auth(x_api_key)
    def fetch():
        services = []
        # Check Lumina via IronClaw gateway tunnel (CT305:3001 via socat)
        try:
            import socket as _sock
            with _sock.create_connection(('YOUR_IRONCLAW_IP', 3001), timeout=3):
                lumina_up = True
        except Exception:
            lumina_up = False
        services.append({'name': 'Lumina', 'status': 'healthy' if lumina_up else 'down', 'checked_at': datetime.now().strftime('%H:%M')})
        
        # LiteLLM needs auth header
        try:
            _req = urllib.request.Request('http://YOUR_LITELLM_IP:4000/v1/models',
                headers={'Authorization': f'Bearer {LITELLM_KEY}', 'User-Agent': 'LuminaGateway/1.0'})
            with urllib.request.urlopen(_req, timeout=5) as _r:
                services.append({'name': 'LiteLLM', 'status': 'healthy' if _r.status == 200 else 'degraded', 'checked_at': datetime.now().strftime('%H:%M')})
        except Exception:
            services.append({'name': 'LiteLLM', 'status': 'down', 'checked_at': datetime.now().strftime('%H:%M')})
        
        checks = [
            ('Plane', 'http://YOUR_PLANE_IP/'),
            ('Gitea', 'http://YOUR_GITEA_IP:3000/api/v1/version'),
            ('Matrix', 'http://YOUR_MATRIX_IP:6167/_matrix/client/versions'),
            ('VM901 Ollama', 'http://YOUR_GPU_HOST_IP:11434/api/version'),
            ('SearXNG', 'http://YOUR_SEARXNG_IP:8088/search?q=test&format=json'),
        ]
        for name, url in checks:
            try:
                req = urllib.request.Request(url, headers={'User-Agent': 'LuminaGateway/1.0'})
                with urllib.request.urlopen(req, timeout=3) as r:
                    status = 'healthy' if r.status in (200, 201) else 'degraded'
            except Exception:
                status = 'down'
            services.append({'name': name, 'status': status, 'checked_at': datetime.now().strftime('%H:%M')})
        # Check Axon service
        import subprocess
        try:
            result = subprocess.run(
                ['systemctl', 'is-active', 'axon'],
                capture_output=True, text=True, timeout=3
            )
            axon_status = 'healthy' if result.stdout.strip() == 'active' else 'down'
        except Exception:
            axon_status = 'unknown'
        services.append({'name': 'Axon', 'status': axon_status, 'checked_at': datetime.now().strftime('%H:%M')})
        healthy = sum(1 for s in services if s['status'] == 'healthy')
        return {
            'services': services,
            'summary': f'{healthy}/{len(services)} healthy',
            'timestamp': datetime.utcnow().isoformat() + 'Z'
        }
    return _cached('health', fetch)


@app.get('/api/inbox')
def get_inbox(x_api_key: str = Header(default='')):
    _check_auth(x_api_key)
    def fetch():
        try:
            import psycopg2
            conn = psycopg2.connect(
                host='YOUR_POSTGRES_IP', dbname='lumina_inbox',
                user='lumina_inbox_user', password=INBOX_DB_PASS,
                connect_timeout=3
            )
            cur = conn.cursor()
            cur.execute(
                "SELECT COUNT(*) FROM inbox_messages WHERE to_agent='lumina' AND status='pending'"
            )
            total = cur.fetchone()[0]
            cur.execute(
                "SELECT priority, COUNT(*) FROM inbox_messages "
                "WHERE to_agent='lumina' AND status='pending' GROUP BY priority"
            )
            by_p = dict(cur.fetchall())
            cur.execute(
                "SELECT created_at FROM inbox_messages "
                "WHERE to_agent='lumina' AND status='pending' "
                "ORDER BY created_at ASC LIMIT 1"
            )
            oldest = cur.fetchone()
            conn.close()
            return {
                'pending': total,
                'by_priority': by_p,
                'oldest': oldest[0].isoformat() + 'Z' if oldest else None
            }
        except Exception as e:
            return {'pending': 0, 'error': str(e)}
    return _cached('inbox', fetch)


@app.get('/api/tasks')
def get_tasks(x_api_key: str = Header(default='')):
    _check_auth(x_api_key)
    def fetch():
        try:
            headers = {'X-API-Key': PLANE_TOKEN}
            # LM project urgent/high In Progress or Todo
            data = _http(
                'http://YOUR_PLANE_IP/api/v1/workspaces/moosenet/projects/'
                '4ef3f3ec-e7ef-4af3-b258-881565e629f9/issues/?per_page=50',
                headers
            )
            issues = data.get('results', [])
            # Get state names
            states_data = _http(
                'http://YOUR_PLANE_IP/api/v1/workspaces/moosenet/projects/'
                '4ef3f3ec-e7ef-4af3-b258-881565e629f9/states/',
                headers
            )
            SM = {s['id']: s['name'] for s in states_data.get('results', [])}
            urgent = [
                {
                    'name': i['name'][:60],
                    'priority': i['priority'],
                    'state': SM.get(i['state'], '')
                }
                for i in issues
                if SM.get(i['state'], '') in ('In Progress', 'Todo')
                and i['priority'] in ('urgent', 'high')
            ][:5]
            return {'count': len(urgent), 'items': urgent}
        except Exception as e:
            return {'count': 0, 'items': [], 'error': str(e)}
    return _cached('tasks', fetch)


@app.get('/api/briefing')
def get_briefing(x_api_key: str = Header(default='')):
    _check_auth(x_api_key)
    def fetch():
        md_path = Path('/opt/lumina-fleet/vigil/output/markdown')
        summary = {}
        for fname in ['latest-morning.md', 'latest-afternoon.md']:
            fpath = md_path / fname
            if fpath.exists():
                content = fpath.read_text()[:2000]
                lines = [l.strip() for l in content.split('\n') if l.strip() and not l.startswith('#')]
                summary[fname.replace('latest-', '').replace('.md', '')] = lines[:3]
        return {
            'summary': summary,
            'dashboard_url': 'http://YOUR_FLEET_SERVER_IP/briefing/',
            'updated': datetime.now().isoformat()
        }
    return _cached('briefing', fetch)


@app.get('/api/commute')
def get_commute(x_api_key: str = Header(default='')):
    _check_auth(x_api_key)
    def fetch():
        try:
            sys.path.insert(0, '/opt/lumina-fleet/engram')
            import engram
            engram.LLM_KEY = LITELLM_KEY
            recent = engram.get_recent(hours_back=12, agent_filter='sentinel')
            commute_entries = [e for e in recent if 'commute' in e.get('action', '').lower()]
            if commute_entries:
                latest = commute_entries[0]
                return {
                    'status': 'data_available',
                    'latest': latest['outcome'],
                    'timestamp': latest['created_at']
                }
            return {
                'status': 'no_recent_data',
                'note': 'Commute monitor fires at 7:00, 7:30, 16:00, 16:30 weekdays'
            }
        except Exception as e:
            return {'status': 'error', 'error': str(e)}
    return _cached('commute', fetch)


@app.get('/api/reports')
def get_reports(x_api_key: str = Header(default='')):
    _check_auth(x_api_key)
    def fetch():
        md_dir = Path('/opt/lumina-fleet/seer/output/markdown')
        reports = []
        if md_dir.exists():
            for f in sorted(md_dir.glob('*.md'), key=lambda x: x.stat().st_mtime, reverse=True)[:5]:
                reports.append({
                    'filename': f.name,
                    'title': f.name.replace('.md', '').replace('-', ' ').title()[:60],
                    'url': f'http://YOUR_FLEET_SERVER_IP/research/{f.stem}.html',
                    'date': f.name[:10] if len(f.name) > 10 else 'unknown'
                })
        return {'count': len(reports), 'reports': reports}
    return _cached('reports', fetch)


@app.get('/api/insights')
def get_insights(x_api_key: str = Header(default='')):
    _check_auth(x_api_key)
    def fetch():
        try:
            sys.path.insert(0, '/opt/lumina-fleet/engram')
            import engram
            engram.LLM_KEY = LITELLM_KEY
            recent = engram.get_recent(hours_back=48)
            if recent:
                entry = recent[0]
                return {
                    'insight': f'{entry["agent"].title()}: {entry["action"]} -> {entry["outcome"][:80]}',
                    'type': 'recent_activity',
                    'timestamp': entry['created_at'],
                    'constellation': _cn(),
                    'lead_agent': _dn('lumina'),
                }
            return {'insight': 'Lumina system nominal. All agents active.', 'type': 'status',
                    'constellation': _cn(), 'lead_agent': _dn('lumina')}
        except Exception as e:
            return {'insight': 'Lumina Dashboard v1.0', 'type': 'welcome', 'error': str(e),
                    'constellation': _cn(), 'lead_agent': _dn('lumina')}
    return _cached('insights', fetch)


@app.get('/api/learning')
def get_learning(x_api_key: str = Header(default='')):
    _check_auth(x_api_key)
    def fetch():
        import subprocess
        try:
            result = subprocess.run(
                ['python3', '/opt/lumina-fleet/crucible/crucible.py', 'streak'],
                capture_output=True, text=True, timeout=10,
                env={**os.environ, 'LITELLM_MASTER_KEY': LITELLM_KEY}
            )
            if result.returncode == 0:
                return json.loads(result.stdout)
            return {'current_streak': 0, 'note': 'No learning data yet'}
        except Exception as e:
            return {'current_streak': 0, 'error': str(e)}
    return _cached('learning', fetch)



@app.get('/api/calendar')
def get_calendar(x_api_key: str = Header(default='')):
    _check_auth(x_api_key)
    def fetch():
        try:
            import caldav
            from datetime import date, datetime as dt
            email = os.environ.get('GOOGLE_LUMINA_EMAIL', '')
            password = os.environ.get('GOOGLE_APP_PASSWORD', '')
            if not email or not password:
                return {'events': [], 'note': 'Google credentials not configured'}
            client = caldav.DAVClient(
                url='https://www.google.com/calendar/dav/' + email + '/events/',
                username=email,
                password=password
            )
            principal = client.principal()
            calendars = principal.calendars()
            today_start = dt.combine(date.today(), dt.min.time())
            today_end = dt.combine(date.today(), dt.max.time())
            events = []
            for cal in calendars[:5]:
                try:
                    for evt in cal.date_search(start=today_start, end=today_end, expand=True):
                        comp = evt.icalendar_component
                        dtstart = comp.get('DTSTART')
                        summary = str(comp.get('SUMMARY', ''))
                        if summary:
                            events.append({
                                'title': summary[:80],
                                'start': str(dtstart.dt) if dtstart else '',
                                'calendar': cal.name or ''
                            })
                except Exception:
                    pass
            events.sort(key=lambda x: x.get('start', ''))
            return {'date': date.today().isoformat(), 'count': len(events), 'events': events[:10]}
        except Exception as e:
            return {'events': [], 'error': str(e)[:100]}
    return _cached('calendar', fetch)


@app.get('/api/weather')
def get_weather(x_api_key: str = Header(default='')):
    """Current weather for your two locations."""
    _check_auth(x_api_key)
    def fetch():
        try:
            results = {}
            for city, query in [('sf', 'San+Francisco'), ('fc', 'Foster+City,CA')]:
                url = f'https://wttr.in/{query}?format=j1'
                data = _http(url, timeout=8)
                if 'current_condition' in data:
                    cc = data['current_condition'][0]
                    results[city] = {
                        'temp_f': cc.get('temp_F', ''),
                        'feels_like_f': cc.get('FeelsLikeF', ''),
                        'description': cc.get('weatherDesc', [{}])[0].get('value', ''),
                        'humidity': cc.get('humidity', ''),
                        'wind_mph': cc.get('windspeedMiles', ''),
                    }
                else:
                    results[city] = {'error': 'unavailable'}
            return results
        except Exception as e:
            return {'error': str(e)}
    return _cached('weather', fetch)


@app.get('/api/nexus_badge')
def get_nexus_badge(x_api_key: str = Header(default='')):
    """Quick Nexus inbox count badge — unread message count for lumina agent."""
    _check_auth(x_api_key)
    def fetch():
        try:
            import psycopg2
            host = os.environ.get('INBOX_DB_HOST', '')
            user = os.environ.get('INBOX_DB_USER', 'lumina_inbox')
            pw = os.environ.get('INBOX_DB_PASS', '')
            if not host:
                return {'count': 0, 'error': 'DB not configured'}
            conn = psycopg2.connect(host=host, dbname='lumina_inbox', user=user, password=pw, connect_timeout=5)
            cur = conn.cursor()
            cur.execute("SELECT COUNT(*) FROM inbox_messages WHERE to_agent='lumina' AND status='pending'")
            count = cur.fetchone()[0]
            cur.execute("SELECT COUNT(*) FROM inbox_messages WHERE to_agent='lumina' AND status='pending' AND priority='critical'")
            critical = cur.fetchone()[0]
            conn.close()
            return {'count': count, 'critical': critical, 'badge': str(count) if count > 0 else ''}
        except Exception as e:
            return {'count': 0, 'error': str(e)[:80]}
    return _cached('nexus_badge', fetch)


@app.get('/api/plane_urgent')
def get_plane_urgent(x_api_key: str = Header(default='')):
    """Urgent/high-priority Plane tasks across key projects."""
    _check_auth(x_api_key)
    def fetch():
        try:
            if not PLANE_TOKEN:
                return {'tasks': [], 'error': 'Plane token not configured'}
            headers = {'X-API-Key': PLANE_TOKEN}
            # Get in-progress items across main projects
            projects = {
                'LM': '4ef3f3ec-e7ef-4af3-b258-881565e629f9',
                'PX': '507ff56c-772d-462e-a79c-2d93783968ff',
            }
            urgent = []
            for proj_id, proj_uuid in projects.items():
                url = f'http://YOUR_PLANE_IP/api/v1/workspaces/moosenet/projects/{proj_uuid}/issues/?state_group=started&priority=urgent'
                data = _http(url, headers=headers)
                for issue in data.get('results', [])[:5]:
                    urgent.append({
                        'project': proj_id,
                        'id': f'{proj_id}-{issue["sequence_id"]}',
                        'title': issue['name'][:60],
                        'priority': issue.get('priority', 'normal'),
                    })
            return {'count': len(urgent), 'tasks': urgent[:10]}
        except Exception as e:
            return {'tasks': [], 'error': str(e)[:80]}
    return _cached('plane_urgent', fetch)


@app.get('/api/code')
def get_code_health(x_api_key: str = Header(default='')):
    """Cortex code health — graph stats for lumina repos."""
    _check_auth(x_api_key)
    def fetch():
        try:
            import subprocess, json
            results = {}
            for repo in ('lumina-fleet', 'lumina-terminus'):
                cmd = f'python3 /opt/lumina-fleet/cortex/cortex.py stats {repo}'
                r = subprocess.run(cmd, shell=True, capture_output=True, text=True, timeout=15)
                if r.returncode == 0:
                    results[repo] = json.loads(r.stdout.strip())
                else:
                    results[repo] = {'error': 'unavailable'}
            return results
        except Exception as e:
            return {'error': str(e)[:80]}
    return _cached('code_health', fetch)


@app.post('/webhook')
async def gitea_webhook(request):
    """Gitea webhook receiver — triggers incremental Cortex graph rebuild on push."""
    import subprocess
    try:
        body = await request.json()
        repo_name = body.get('repository', {}).get('name', '')
        if repo_name in ('lumina-fleet', 'lumina-terminus'):
            subprocess.Popen(
                ['python3', '/opt/lumina-fleet/cortex/cortex.py', 'build', repo_name],
                stdout=open('/opt/lumina-fleet/cortex/logs/webhook.log', 'a'),
                stderr=subprocess.STDOUT
            )
            return {'status': 'rebuilding', 'repo': repo_name}
    except Exception as e:
        pass
    return {'status': 'ignored'}


@app.get('/health')
def healthcheck():
    return {'status': 'ok', 'service': 'lumina-gateway', 'version': '1.2'}


@app.get('/api/cost')
def get_cost(x_api_key: str = Header(default='')):
    """Myelin cost summary — today's inference spend and agent breakdown."""
    _check_auth(x_api_key)
    def fetch():
        import json
        from pathlib import Path
        usage_file = Path('/opt/lumina-fleet/myelin/output/usage.json')
        if usage_file.exists():
            try:
                return json.loads(usage_file.read_text())
            except Exception as e:
                return {'error': str(e)[:80]}
        return {'note': 'Myelin not yet collecting. Run myelin_collect.py first.', 'today': {'cost_usd': 0}}
    return _cached('cost', fetch)


@app.get('/')
def root():
    return {
        'service': 'Lumina API Gateway',
        'version': '1.3',
        'endpoints': [
            '/api/health', '/api/inbox', '/api/tasks', '/api/briefing',
            '/api/commute', '/api/reports', '/api/insights', '/api/learning',
            '/api/calendar', '/api/weather', '/api/nexus_badge', '/api/plane_urgent',
            '/api/code', '/api/cost', '/webhook'
        ]
    }


# ── CHAT ENDPOINT ─────────────────────────────────────────────────────────────

from pydantic import BaseModel

class ChatRequest(BaseModel):
    message: str
    session_id: str = ''

@app.post('/api/chat')
def post_chat(req: ChatRequest, x_api_key: str = Header(default='')):
    """
    Chat endpoint for the Lumina chat widget.
    Accepts a message and session_id; forwards to Lumina via Nexus when available.
    For now returns an immediate acknowledgement — Lumina responds via Matrix.
    """
    _check_auth(x_api_key)
    msg = req.message.strip()
    if not msg:
        raise HTTPException(status_code=400, detail='message is required')
    sid = req.session_id or ('chat-' + str(int(time.time())))
    # Attempt to route through Nexus if psycopg2 + DB are available
    routed = False
    try:
        import psycopg2
        host = os.environ.get('INBOX_DB_HOST', '')
        user = os.environ.get('INBOX_DB_USER', 'lumina_inbox')
        pw   = os.environ.get('INBOX_DB_PASS', '')
        if host and pw:
            conn = psycopg2.connect(
                host=host, dbname='lumina_inbox', user=user,
                password=pw, connect_timeout=3
            )
            cur = conn.cursor()
            import uuid as _uuid
            cur.execute(
                "INSERT INTO inbox_messages "
                "(id, from_agent, to_agent, message_type, priority, payload, status, created_at) "
                "VALUES (%s, %s, %s, %s, %s, %s, 'pending', NOW())",
                (
                    str(_uuid.uuid4()), 'dashboard-chat', 'lumina',
                    'chat', 'normal',
                    json.dumps({'message': msg, 'session_id': sid, 'source': 'chat_widget'})
                )
            )
            conn.commit()
            conn.close()
            routed = True
    except Exception:
        pass
    return {
        'response': f'Received: {msg}. Lumina will respond via Matrix.',
        'session_id': sid,
        'routed_to_nexus': routed,
        'timestamp': datetime.utcnow().isoformat() + 'Z'
    }


if __name__ == '__main__':
    import uvicorn
    port = int(os.environ.get('GATEWAY_PORT', 8080))
    print(f'[gateway] Starting on port {port}')
    uvicorn.run(app, host='0.0.0.0', port=port, log_level='info')
