"""Status page generator for Sentinel. Called after each health check cycle."""

import sys as _sys; _sys.path.insert(0, '/opt/lumina-fleet')
try: from naming import display_name as _dn, constellation_name as _cn
except: _dn = lambda x: x; _cn = lambda: 'Lumina'

import os
import json
import subprocess
import urllib.request
import ssl
from datetime import datetime
from pathlib import Path

OUTPUT_DIR = Path('/opt/lumina-fleet/sentinel/output/html')
PLANE_TOKEN = os.environ.get('PLANE_TOKEN_LUMINA', '')
INBOX_DB_PASS = os.environ.get('INBOX_DB_PASS', '')

SSL_CTX = ssl.create_default_context()
SSL_CTX.check_hostname = False
SSL_CTX.verify_mode = ssl.CERT_NONE

def check_service(name, check_fn):
    """Run a health check and return {name, status, message, checked_at}."""
    try:
        status, message = check_fn()
    except Exception as e:
        status, message = 'down', str(e)[:100]
    return {'name': name, 'status': status, 'message': message,
            'checked_at': datetime.now().strftime('%H:%M:%S')}

def _http_check(url, timeout=5, expected_status=200):
    try:
        ctx = SSL_CTX if url.startswith('https') else None
        req = urllib.request.Request(url, headers={'User-Agent': 'Sentinel/1.0'})
        with urllib.request.urlopen(req, timeout=timeout, context=ctx) as r:
            if r.status == expected_status:
                return 'healthy', f'HTTP {r.status}'
            return 'degraded', f'HTTP {r.status}'
    except urllib.error.URLError as e:
        if 'Connection refused' in str(e) or 'timed out' in str(e).lower():
            return 'down', str(e.reason)[:60]
        return 'degraded', str(e)[:60]
    except Exception as e:
        return 'down', str(e)[:60]

def collect_statuses():
    services = []

    # Lumina (CT305)
    def check_lumina():
        r, m = _http_check('http://YOUR_IRONCLAW_IP:3001/health', timeout=3)
        return r, m
    services.append(check_service('Lumina', check_lumina))

    # Nexus/Postgres (CT300)
    def check_nexus():
        try:
            import psycopg2
            conn = psycopg2.connect(host='YOUR_POSTGRES_IP', dbname='lumina_inbox',
                user='lumina_inbox_user', password=INBOX_DB_PASS, connect_timeout=3)
            cur = conn.cursor()
            cur.execute('SELECT COUNT(*) FROM inbox_messages WHERE status=\'pending\'')
            pending = cur.fetchone()[0]
            conn.close()
            return 'healthy', f'Connected, {pending} pending messages'
        except Exception as e:
            return 'down', str(e)[:60]
    services.append(check_service('Nexus (CT300)', check_nexus))

    # Terminus/MCP (CT214) — check via HTTP if available
    def check_terminus():
        r, m = _http_check('http://YOUR_TERMINUS_IP:8000/health', timeout=3)
        return r, m
    services.append(check_service('Terminus (CT214)', check_terminus))

    # LiteLLM (CT215)
    def check_litellm():
        r, m = _http_check('http://YOUR_LITELLM_IP:4000/health', timeout=3)
        return r, m
    services.append(check_service('LiteLLM (CT215)', check_litellm))

    # Tuwunel/Matrix (CT306)
    def check_matrix():
        r, m = _http_check('http://YOUR_MATRIX_IP:6167/_matrix/client/versions', timeout=3)
        return r, m
    services.append(check_service('Matrix (CT306)', check_matrix))

    # Plane (CT315)
    def check_plane():
        r, m = _http_check('http://YOUR_PLANE_IP/', timeout=3)
        return r, m
    services.append(check_service('Plane (CT315)', check_plane))

    # Gitea (CT223)
    def check_gitea():
        r, m = _http_check('http://YOUR_GITEA_IP:3000/api/v1/version', timeout=3)
        return r, m
    services.append(check_service('Gitea (CT223)', check_gitea))

    # VM901 GPU (Ollama)
    def check_vm901():
        r, m = _http_check('http://YOUR_GPU_HOST_IP:11434/api/version', timeout=3)
        return r, m
    services.append(check_service('VM901 GPU (Ollama)', check_vm901))

    # Axon (CT310 self-check)
    def check_axon():
        result = subprocess.run(['systemctl', 'is-active', 'axon'],
            capture_output=True, text=True, timeout=3)
        active = result.stdout.strip() == 'active'
        return ('healthy' if active else 'down'), ('running' if active else 'stopped')
    services.append(check_service('Axon (CT310)', check_axon))

    # Vigil - check last briefing file age
    def check_vigil():
        briefing_dir = Path('/opt/lumina-fleet/vigil/output/markdown')
        if not briefing_dir.exists():
            return 'degraded', 'No output directory'
        files = sorted(briefing_dir.glob('*.md'), key=lambda f: f.stat().st_mtime, reverse=True)
        if not files:
            return 'degraded', 'No briefings generated yet'
        import time
        age_hours = (time.time() - files[0].stat().st_mtime) / 3600
        if age_hours < 24:
            return 'healthy', f'Last briefing {age_hours:.1f}h ago'
        elif age_hours < 48:
            return 'degraded', f'Stale — {age_hours:.1f}h ago'
        return 'down', f'Very stale — {age_hours:.1f}h ago'
    services.append(check_service('Vigil (CT310)', check_vigil))

    # Sentinel self
    services.append({'name': f'{_dn("sentinel")} (CT310)', 'status': 'healthy',
                     'message': 'Running (self-report)',
                     'checked_at': datetime.now().strftime('%H:%M:%S')})

    # SearXNG
    def check_searxng():
        r, m = _http_check('http://YOUR_SEARXNG_IP:8088/search?q=test&format=json', timeout=3)
        return r, m
    services.append(check_service('SearXNG', check_searxng))

    return services

def generate_status_html(services=None):
    if services is None:
        services = collect_statuses()

    OUTPUT_DIR.mkdir(parents=True, exist_ok=True)

    # Map status to constellation CSS classes
    dot_class = {'healthy': 'up', 'degraded': 'degraded', 'down': 'down', 'unknown': 'unknown'}
    badge_class = {'healthy': 'badge-success', 'degraded': 'badge-warning', 'down': 'badge-danger', 'unknown': 'badge-neutral'}

    cards = ''
    for s in services:
        dc = dot_class.get(s['status'], 'unknown')
        bc = badge_class.get(s['status'], 'badge-neutral')
        cards += f'''
        <div class="card" style="border-left:3px solid var(--{'success' if s['status']=='healthy' else 'warning' if s['status']=='degraded' else 'danger' if s['status']=='down' else 'text-tertiary'})">
            <div style="display:flex;align-items:center;gap:var(--space-2);margin-bottom:var(--space-2)">
                <span class="health-dot {dc}"></span>
                <strong style="flex:1;font-size:var(--text-sm)">{s["name"]}</strong>
                <span class="badge {bc}">{s["status"]}</span>
            </div>
            <div style="font-size:var(--text-sm);color:var(--text-secondary);margin-bottom:var(--space-1)">{s["message"]}</div>
            <div style="font-size:var(--text-xs);color:var(--text-tertiary)">checked {s["checked_at"]}</div>
        </div>'''

    healthy = sum(1 for s in services if s['status'] == 'healthy')
    degraded = sum(1 for s in services if s['status'] == 'degraded')
    down = sum(1 for s in services if s['status'] == 'down')

    if down > 0:
        overall_badge = 'badge-danger'
        overall_text = 'INCIDENT'
        overall_dot = 'down'
    elif degraded > 0:
        overall_badge = 'badge-warning'
        overall_text = 'DEGRADED'
        overall_dot = 'degraded'
    else:
        overall_badge = 'badge-success'
        overall_text = 'ALL SYSTEMS GO'
        overall_dot = 'up'

    html_out = f'''<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta http-equiv="refresh" content="300">
<title>{_cn()} Status</title>
<link rel="stylesheet" href="/shared/constellation.css">
</head>
<body>
<div class="lumina-header">
    <div class="lumina-logo">{_cn()} Status</div>
    <div style="margin-left:auto;display:flex;align-items:center;gap:var(--space-2)">
        <span class="health-dot {overall_dot}"></span>
        <span class="badge {overall_badge}" style="font-size:var(--text-sm);padding:4px 14px">{overall_text}</span>
    </div>
</div>
<div class="page">
    <div style="text-align:center;margin-bottom:var(--space-5)">
        <div class="text-secondary" style="font-size:var(--text-sm)">{datetime.now().strftime("%Y-%m-%d %H:%M")} · {healthy} healthy · {degraded} degraded · {down} down · auto-refresh 5min</div>
    </div>
    <div class="grid-3">
{cards}
    </div>
</div>
<div class="lumina-footer">{_dn('sentinel')} v1.0 · CT310 · moosenet.online</div>
</body>
</html>'''

    (OUTPUT_DIR / 'index.html').write_text(html_out)
    return str(OUTPUT_DIR / 'index.html')


if __name__ == '__main__':
    services = collect_statuses()
    path = generate_status_html(services)
    print(f'Status page written to: {path}')
    for s in services:
        icon = 'OK' if s['status'] == 'healthy' else ('WARN' if s['status'] == 'degraded' else 'DOWN')
        print(f'  [{icon}] {s["name"]}: {s["message"]}')
