#!/usr/bin/env python3
"""
health_checks.py — Comprehensive Lumina Constellation health monitoring.
Overhaul after two production incidents (Apr 13, 2026):
1. Axon DB password reset → undetected 24h outage
2. VM901 GPU offline → LiteLLM → openrouter-auto → $30/day runaway
"""
import os, sys, json, subprocess, urllib.request, urllib.error, time
from datetime import datetime, timezone
from pathlib import Path

def _load_env():
    env_file = Path('/opt/lumina-fleet/axon/.env')
    if env_file.exists():
        for line in env_file.read_text().splitlines():
            if '=' in line and not line.startswith('#'):
                k, v = line.split('=', 1)
                os.environ.setdefault(k.strip(), v.strip().strip('"').strip("'"))

_load_env()

LITELLM_URL   = os.environ.get('LITELLM_URL', 'http://YOUR_LITELLM_IP:4000')
LITELLM_KEY   = os.environ.get('LITELLM_MASTER_KEY', '')
INBOX_DB_HOST = os.environ.get('INBOX_DB_HOST', 'YOUR_POSTGRES_IP')
INBOX_DB_USER = os.environ.get('INBOX_DB_USER', 'lumina_inbox_user')
INBOX_DB_PASS = os.environ.get('INBOX_DB_PASS', '')
OR_KEY        = os.environ.get('OPENROUTER_API_KEY', '')
GITEA_TOKEN   = os.environ.get('GITEA_TOKEN', '')
PLANE_TOKEN   = os.environ.get('PLANE_API_TOKEN', '')
TOMTOM_KEY    = os.environ.get('TOMTOM_API_KEY', '')
NEWS_API_KEY  = os.environ.get('NEWS_API_KEY', '')
PVS_HOST      = os.environ.get('PVS_HOST', 'root@YOUR_PVS_HOST_IP')

OUTPUT_DIR = Path('/opt/lumina-fleet/sentinel/output')
HEALTH_JSON = OUTPUT_DIR / 'health.json'
CHECK_TIMEOUT = 5

def _http(url, headers=None, timeout=CHECK_TIMEOUT):
    try:
        req = urllib.request.Request(url, headers=headers or {'User-Agent': 'Sentinel/2.0'})
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return True, f'HTTP {r.status}'
    except urllib.error.HTTPError as e:
        return e.code < 500, f'HTTP {e.code}'
    except Exception as e:
        return False, str(e)[:60]

def _pct(ct, cmd, timeout=CHECK_TIMEOUT):
    try:
        result = subprocess.run(
            ['ssh', '-o', f'ConnectTimeout={timeout}', '-o', 'StrictHostKeyChecking=no',
             PVS_HOST, f'pct exec {ct} -- {cmd}'],
            capture_output=True, text=True, timeout=timeout + 3)
        return result.returncode, (result.stdout + result.stderr).strip()[:200]
    except Exception as e:
        return -1, str(e)[:60]

def _nexus_alert(service, status, message, priority='urgent'):
    try:
        import psycopg2
        conn = psycopg2.connect(host=INBOX_DB_HOST, dbname='lumina_inbox',
            user=INBOX_DB_USER, password=INBOX_DB_PASS, connect_timeout=3)
        payload = json.dumps({'service': service, 'status': status, 'message': message,
            'source': 'sentinel', 'checked_at': datetime.utcnow().isoformat()+'Z'})
        conn.cursor().execute(
            "INSERT INTO inbox_messages (from_agent,to_agent,message_type,priority,payload,status) "
            "VALUES ('sentinel','lumina','notification',%s,%s,'pending')", (priority, payload))
        conn.commit(); conn.close()
    except Exception as e:
        print(f'[sentinel] Nexus alert failed: {e}', file=sys.stderr)

# ── INCIDENT CHECKS ────────────────────────────────────────────────────────────

def check_axon_db():
    """PRIORITY: Would have caught the Apr 12 24h Axon outage."""
    rc, out = _pct('310', 'systemctl is-active axon.service')
    if rc != 0 or 'active' not in out:
        return {'status': 'critical', 'value': 0, 'message': f'Axon not running: {out[:40]}'}
    rc2, out2 = _pct('310', "journalctl -u axon.service --since=-10min --no-pager 2>&1 | grep -c 'poll\\|DB' || echo 0")
    recent = int(out2.strip()) if out2.strip().isdigit() else 0
    db_cmd = (f'python3 -c "import psycopg2; psycopg2.connect(host=\\"{INBOX_DB_HOST}\\",'
              f'dbname=\\"lumina_inbox\\",user=\\"lumina_inbox_user\\",'
              f'password=\\"{INBOX_DB_PASS}\\",connect_timeout=3).close(); print(\\"ok\\")"')
    rc3, out3 = _pct('310', db_cmd, timeout=8)
    if rc3 != 0 or 'ok' not in out3:
        return {'status': 'critical', 'value': 0, 'message': f'Axon DB FAILED: {out3[:80]}'}
    if recent == 0:
        return {'status': 'warn', 'value': 1, 'message': 'DB ok but no poll in 10min'}
    return {'status': 'ok', 'value': recent, 'message': f'DB ok, {recent} polls/10min'}

def check_ollama_gpu():
    """PRIORITY: Would have prevented the Apr 13 $30 LLM runaway."""
    ok, msg = _http('http://YOUR_GPU_HOST_IP:11434/api/version', timeout=5)
    if ok:
        return {'status': 'ok', 'value': 1, 'message': f'VM901 GPU online'}
    return {'status': 'critical', 'value': 0, 'message': f'VM901 OFFLINE ({msg}). Fallback to claude-sonnet now active.'}

def check_llm_cost():
    """OpenRouter daily spend. WARN $2, CRITICAL $10."""
    if not OR_KEY:
        return {'status': 'ok', 'value': 0, 'message': 'OR key not configured'}
    try:
        req = urllib.request.Request('https://openrouter.ai/api/v1/auth/key',
            headers={'Authorization': f'Bearer {OR_KEY}'})
        with urllib.request.urlopen(req, timeout=10) as r:
            daily = float(json.load(r).get('data', {}).get('usage_daily', 0) or 0)
        if daily >= 10:
            return {'status': 'critical', 'value': daily, 'message': f'${daily:.2f}/day CIRCUIT BREAK!'}
        if daily >= 2:
            return {'status': 'warn', 'value': daily, 'message': f'${daily:.2f}/day warn'}
        return {'status': 'ok', 'value': daily, 'message': f'${daily:.2f}/day'}
    except Exception as e:
        return {'status': 'warn', 'value': -1, 'message': str(e)[:60]}

# ── Infrastructure ─────────────────────────────────────────────────────────────

def check_ironclaw():
    rc, out = _pct('305', '/usr/local/bin/ironclaw --version', timeout=8)
    if rc == 0:
        return {'status': 'ok', 'value': 1, 'message': out.strip()[:40]}
    rc2, _ = _pct('305', 'pgrep -f ironclaw')
    return ({'status': 'ok', 'value': 1, 'message': 'process running'} if rc2 == 0
            else {'status': 'critical', 'value': 0, 'message': 'IronClaw down'})

def check_terminus():
    rc, out = _pct('214', 'systemctl is-active ai-mcp.service')
    return ({'status': 'ok', 'value': 1, 'message': 'ai-mcp active'} if rc == 0 and 'active' in out
            else {'status': 'critical', 'value': 0, 'message': f'Terminus down: {out[:60]}'})

def check_litellm():
    start = time.monotonic()
    ok, msg = _http(f'{LITELLM_URL}/health', headers={'Authorization': f'Bearer {LITELLM_KEY}'})
    ms = int((time.monotonic() - start) * 1000)
    if ok:
        return {'status': 'warn' if ms > 2000 else 'ok', 'value': ms, 'message': f'{ms}ms'}
    return {'status': 'critical', 'value': 0, 'message': f'LiteLLM down: {msg}'}

def check_postgres():
    try:
        import psycopg2
        start = time.monotonic()
        conn = psycopg2.connect(host=INBOX_DB_HOST, dbname='lumina_inbox',
            user=INBOX_DB_USER, password=INBOX_DB_PASS, connect_timeout=CHECK_TIMEOUT)
        pending = conn.cursor()
        pending.execute("SELECT COUNT(*) FROM inbox_messages WHERE status='pending'")
        count = pending.fetchone()[0]
        conn.close()
        ms = int((time.monotonic() - start) * 1000)
        return {'status': 'ok', 'value': count, 'message': f'{count} pending, {ms}ms'}
    except Exception as e:
        return {'status': 'critical', 'value': -1, 'message': f'Postgres down: {str(e)[:60]}'}

def check_matrix():
    rc, out = _pct('306', 'systemctl is-active matrix-bridge.service')
    bridge = rc == 0 and 'active' in out
    rc2, out2 = _pct('306', 'docker ps --filter name=tuwunel --format "{{.Status}}"')
    tuwunel = rc2 == 0 and 'Up' in out2
    if bridge and tuwunel:
        return {'status': 'ok', 'value': 1, 'message': 'bridge+tuwunel running'}
    issues = (['bridge down'] if not bridge else []) + (['tuwunel down'] if not tuwunel else [])
    return {'status': 'critical', 'value': 0, 'message': ', '.join(issues)}

def check_docker():
    rc, out = _pct('310', 'docker ps --format "{{.Names}}:{{.Status}}"')
    if rc != 0:
        return {'status': 'critical', 'value': 0, 'message': 'Docker inaccessible'}
    expected = ['caddy', 'actual-budget', 'grocy', 'lubelogger']
    running = {l.split(':')[0] for l in out.splitlines() if 'Up' in l}
    down = [c for c in expected if c not in running]
    if not down:
        return {'status': 'ok', 'value': len(running), 'message': f'{len(running)} containers up'}
    return {'status': 'warn' if len(down) == 1 else 'critical',
            'value': len(running), 'message': f'Down: {", ".join(down)}'}

def check_plane():
    ok, msg = _http('http://YOUR_PLANE_IP/api/v1/workspaces/moosenet/projects/',
                    headers={'X-API-Key': PLANE_TOKEN} if PLANE_TOKEN else {})
    return ({'status': 'ok', 'value': 1, 'message': 'Plane ok'} if ok
            else {'status': 'warn', 'value': 0, 'message': f'Plane: {msg}'})

def check_ollama_cpu():
    ok, msg = _http('http://YOUR_CPU_OLLAMA_IP:11434/api/version', timeout=5)
    return ({'status': 'ok', 'value': 1, 'message': 'CT110 CPU online'} if ok
            else {'status': 'warn', 'value': 0, 'message': f'CT110 CPU offline: {msg}'})

# ── Agents ─────────────────────────────────────────────────────────────────────

def check_sentinel():
    rc, _ = _pct('310', 'systemctl is-active sentinel-health.timer')
    return ({'status': 'ok', 'value': 1, 'message': 'timer active'} if rc == 0
            else {'status': 'critical', 'value': 0, 'message': 'Sentinel timer NOT running!'})

def check_soma():
    ok, _ = _http('http://localhost:8082/health')
    if not ok:
        return {'status': 'critical', 'value': 0, 'message': 'Soma down'}
    pages = sum(1 for p in ['/status', '/wizard'] if _http(f'http://localhost:8082{p}')[0])
    return {'status': 'ok' if pages == 2 else 'warn', 'value': pages, 'message': f'{pages}/2 pages ok'}

# ── Data ───────────────────────────────────────────────────────────────────────

def check_engram():
    rc, out = _pct('310',
        "python3 -c \"import sqlite3; print(sqlite3.connect('/opt/lumina-fleet/engram/knowledge_base.db').execute('SELECT count(*) FROM knowledge_base').fetchone()[0])\"",
        timeout=8)
    if rc == 0 and out.strip().isdigit():
        return {'status': 'ok', 'value': int(out.strip()), 'message': f'{out.strip()} facts'}
    return {'status': 'warn', 'value': 0, 'message': f'Engram check failed: {out[:60]}'}

def check_nexus_age():
    try:
        import psycopg2
        conn = psycopg2.connect(host=INBOX_DB_HOST, dbname='lumina_inbox',
            user=INBOX_DB_USER, password=INBOX_DB_PASS, connect_timeout=CHECK_TIMEOUT)
        cur = conn.cursor()
        cur.execute("SELECT COUNT(*), COALESCE(EXTRACT(EPOCH FROM (NOW()-MIN(created_at)))/60,0) FROM inbox_messages WHERE status='pending'")
        count, oldest = cur.fetchone()
        conn.close()
        oldest = float(oldest)
        if count > 20 or oldest > 60:
            return {'status': 'warn', 'value': int(count), 'message': f'{count} unacked, oldest {oldest:.0f}min'}
        return {'status': 'ok', 'value': int(count), 'message': f'{count} pending, oldest {oldest:.0f}min'}
    except Exception as e:
        return {'status': 'warn', 'value': -1, 'message': str(e)[:60]}

def check_gitea():
    ok, msg = _http('http://YOUR_GITEA_IP:3000/api/v1/version',
                    headers={'Authorization': f'token {GITEA_TOKEN}'} if GITEA_TOKEN else {})
    return ({'status': 'ok', 'value': 1, 'message': 'Gitea ok'} if ok
            else {'status': 'warn', 'value': 0, 'message': f'Gitea: {msg}'})

# ── External ───────────────────────────────────────────────────────────────────

def check_tomtom():
    if not TOMTOM_KEY:
        return {'status': 'ok', 'value': -1, 'message': 'key not configured'}
    ok, msg = _http(f'https://api.tomtom.com/routing/1/calculateRoute/0,0:1,1/json?key={TOMTOM_KEY}', timeout=8)
    return ({'status': 'ok', 'value': 1, 'message': 'TomTom ok'} if ok
            else {'status': 'warn', 'value': 0, 'message': f'TomTom: {msg}'})

def check_newsapi():
    if not NEWS_API_KEY:
        return {'status': 'ok', 'value': -1, 'message': 'key not configured'}
    ok, msg = _http(f'https://newsapi.org/v2/top-headlines?country=us&pageSize=1&apiKey={NEWS_API_KEY}', timeout=8)
    return ({'status': 'ok', 'value': 1, 'message': 'NewsAPI ok'} if ok
            else {'status': 'warn', 'value': 0, 'message': f'NewsAPI: {msg}'})

# ── Registry & Runner ──────────────────────────────────────────────────────────

CHECKS = {
    # INCIDENT checks — must run every cycle
    'axon_db':    (check_axon_db,    'critical', 'Axon DB (incident check)'),
    'ollama_gpu': (check_ollama_gpu, 'critical', 'VM901 GPU (runaway check)'),
    'llm_cost':   (check_llm_cost,   'critical', 'LLM daily cost'),
    # Infrastructure
    'ironclaw':   (check_ironclaw,   'critical', 'IronClaw CT305'),
    'terminus':   (check_terminus,   'critical', 'Terminus CT214'),
    'litellm':    (check_litellm,    'high',     'LiteLLM CT215'),
    'postgres':   (check_postgres,   'critical', 'Postgres CT300'),
    'matrix':     (check_matrix,     'high',     'Matrix CT306'),
    'docker':     (check_docker,     'high',     'Docker CT310'),
    'plane':      (check_plane,      'medium',   'Plane CE'),
    'ollama_cpu': (check_ollama_cpu, 'medium',   'Ollama CPU CT110'),
    # Agents
    'sentinel':   (check_sentinel,   'critical', 'Sentinel self-check'),
    'soma':       (check_soma,       'high',     'Soma admin panel'),
    # Data
    'engram':     (check_engram,     'medium',   'Engram facts'),
    'nexus_age':  (check_nexus_age,  'medium',   'Nexus message age'),
    'gitea':      (check_gitea,      'medium',   'Gitea'),
    # External
    'tomtom':     (check_tomtom,     'low',      'TomTom API'),
    'newsapi':    (check_newsapi,    'low',      'NewsAPI'),
}

def run_all_checks(check_names=None, alert_on_change=True):
    OUTPUT_DIR.mkdir(parents=True, exist_ok=True)
    to_run = {k: v for k, v in CHECKS.items() if not check_names or k in check_names}
    results = {}
    critical_list, warn_list = [], []

    prev = {}
    try:
        prev = json.loads(HEALTH_JSON.read_text()).get('checks', {})
    except Exception:
        pass

    for name, (fn, tier, label) in to_run.items():
        try:
            result = fn()
        except Exception as e:
            result = {'status': 'warn', 'value': -1, 'message': f'Error: {str(e)[:60]}'}
        results[name] = {**result, 'label': label, 'tier': tier}

        prev_status = prev.get(name, {}).get('status', 'ok')
        is_new = result['status'] in ('critical', 'warn') and prev_status == 'ok'

        if result['status'] == 'critical':
            critical_list.append(name)
            if alert_on_change and is_new:
                _nexus_alert(name, 'critical', result['message'], 'critical')
                print(f'[sentinel] CRITICAL: {label}: {result["message"]}')
        elif result['status'] == 'warn':
            warn_list.append(name)
            if alert_on_change and is_new and tier in ('critical', 'high'):
                _nexus_alert(name, 'warn', result['message'], 'urgent')

    overall = 'critical' if critical_list else ('warn' if warn_list else 'ok')
    output = {
        'timestamp': datetime.now(timezone.utc).isoformat(),
        'overall': overall,
        'critical': len(critical_list),
        'warn': len(warn_list),
        'ok': len([r for r in results.values() if r['status'] == 'ok']),
        'total': len(results),
        'checks': results,
    }
    HEALTH_JSON.write_text(json.dumps(output, indent=2))
    print(f'[sentinel] {output["ok"]}/{output["total"]} ok | {len(critical_list)} critical | {len(warn_list)} warn')
    return output

if __name__ == '__main__':
    result = run_all_checks(sys.argv[1:] or None)
    print(json.dumps(result, indent=2, default=str))
    sys.exit(0 if result['overall'] == 'ok' else 1)
