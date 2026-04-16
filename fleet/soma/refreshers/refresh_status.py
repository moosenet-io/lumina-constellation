"""
refresh_status.py — Status page data refresher
fleet/soma/refreshers/refresh_status.py

Probes all Constellation services and returns structured status.
Called by the cache background task. Plain Python, no LLM.
"""

import json
import os
import subprocess
import time
import urllib.request
import urllib.error
from datetime import datetime, timezone
from pathlib import Path


def _probe_http(url: str, timeout: int = 5, headers: dict = None) -> tuple:
    """Returns (ok: bool, data: dict, latency_ms: int)."""
    import time
    start = time.time()
    try:
        req = urllib.request.Request(url, headers=headers or {})
        with urllib.request.urlopen(req, timeout=timeout) as r:
            ms = int((time.time() - start) * 1000)
            try:
                data = json.loads(r.read())
            except Exception:
                data = {}
            return True, data, ms
    except Exception as e:
        ms = int((time.time() - start) * 1000)
        return False, {'error': str(e)[:100]}, ms


def _probe_ssh(host: str, command: str, timeout: int = 10) -> tuple:
    """SSH probe. Returns (ok, output, error)."""
    try:
        result = subprocess.run(
            ['ssh', '-o', 'ConnectTimeout=5', '-o', 'BatchMode=yes', host, command],
            capture_output=True, text=True, timeout=timeout
        )
        return result.returncode == 0, result.stdout.strip(), result.stderr.strip()
    except Exception as e:
        return False, '', str(e)[:100]


def refresh_status() -> dict:
    """
    Probe all Constellation services. Returns structured status dict.
    Pure Python — $0 inference cost.
    """
    ironclaw_url  = os.environ.get('IRONCLAW_URL', '')
    ironclaw_token = os.environ.get('IRONCLAW_GATEWAY_TOKEN', '')
    litellm_url   = os.environ.get('LITELLM_URL', '')
    litellm_key   = os.environ.get('LITELLM_MASTER_KEY', '')
    nexus_db_url  = os.environ.get('NEXUS_API_URL', 'http://localhost:8083')
    matrix_url    = os.environ.get('MATRIX_URL', '')

    services = {}
    overall_ok = True

    # IronClaw — /api/health returns {"status":"healthy","channel":"gateway"}
    # Also probe /v1/models for model count (LiteLLM-compatible endpoint on the gateway)
    ic_ok, ic_data, ic_ms = _probe_http(
        f'{ironclaw_url}/api/health',
        headers={'Authorization': f'Bearer {ironclaw_token}'} if ironclaw_token else {}
    )
    ic_gateway_status = ic_data.get('status', '') if ic_ok else ''
    ic_channel = ic_data.get('channel', '') if ic_ok else ''
    # Probe models endpoint for model count (degraded = gateway up but no models)
    ic_models_ok, ic_models_data, _ = _probe_http(
        f'{ironclaw_url}/v1/models',
        headers={'Authorization': f'Bearer {ironclaw_token}'} if ironclaw_token else {}
    ) if ic_ok else (False, {}, 0)
    ic_model_count = len(ic_models_data.get('data', [])) if ic_models_ok else None
    ic_display = (
        f"{ic_model_count} models · {ic_ms}ms" if ic_model_count
        else ("LLM proxy unreachable" if ic_ok else "IronClaw offline")
    )
    services['ironclaw'] = {
        'name': 'IronClaw', 'ok': ic_ok,
        'latency_ms': ic_ms,
        'channel': ic_channel,
        'gateway_status': ic_gateway_status,
        'model_count': ic_model_count,
        'display': ic_display,
        'error': ic_data.get('error') if not ic_ok else (None if ic_models_ok else 'LLM proxy unreachable'),
    }
    if not ic_ok:
        overall_ok = False

    # LiteLLM
    ll_ok, ll_data, ll_ms = _probe_http(
        f'{litellm_url}/v1/models',
        headers={'Authorization': f'Bearer {litellm_key}'} if litellm_key else {}
    )
    model_count = len(ll_data.get('data', [])) if ll_ok else 0
    services['litellm'] = {
        'name': 'LiteLLM', 'ok': ll_ok,
        'latency_ms': ll_ms,
        'model_count': model_count,
        'error': ll_data.get('error') if not ll_ok else None,
    }

    # Matrix
    mx_ok, mx_data, mx_ms = _probe_http(f'{matrix_url}/_matrix/client/versions')
    services['matrix'] = {
        'name': 'Matrix', 'ok': mx_ok,
        'latency_ms': mx_ms,
        'error': mx_data.get('error') if not mx_ok else None,
    }

    # Nexus (SQLite-backed inbox — check DB file, then count messages)
    import sqlite3 as _sqlite3
    nexus_db = '/opt/lumina-fleet/nexus/nexus.db'
    _nexus_pg_host = os.environ.get('INBOX_DB_HOST', '')
    nx_ok = False
    nx_message_count = None
    nx_unacked = None
    nx_err = None
    if _nexus_pg_host:
        # Postgres-backed Nexus — probe the DB directly
        try:
            import psycopg2 as _pg2
            _pg_conn = _pg2.connect(
                host=_nexus_pg_host,
                dbname='lumina_inbox',
                user=os.environ.get('INBOX_DB_USER', 'lumina_inbox_user'),
                password=os.environ.get('INBOX_DB_PASS', ''),
                connect_timeout=3,
            )
            _cur = _pg_conn.cursor()
            _cur.execute("SELECT COUNT(*) FROM inbox_messages")
            nx_message_count = _cur.fetchone()[0]
            _cur.execute("SELECT COUNT(*) FROM inbox_messages WHERE status='pending'")
            nx_unacked = _cur.fetchone()[0]
            _pg_conn.close()
            nx_ok = True
        except Exception as _e:
            nx_err = f'Nexus DB: {str(_e)[:60]}'
    elif os.path.exists(nexus_db):
        try:
            conn = _sqlite3.connect(nexus_db, timeout=2)
            nx_message_count = conn.execute("SELECT COUNT(*) FROM inbox_messages").fetchone()[0]
            nx_unacked = conn.execute("SELECT COUNT(*) FROM inbox_messages WHERE status='pending'").fetchone()[0]
            conn.close()
            nx_ok = True
        except Exception as e:
            nx_err = str(e)[:80]
    else:
        nx_err = 'Nexus DB not found (inbox not initialised)'
        nx_ok = False
    services['nexus'] = {
        'name': 'Nexus', 'ok': nx_ok,
        'latency_ms': 0,
        'message_count': nx_message_count,
        'unacked': nx_unacked,
        'error': nx_err if not nx_ok else None,
    }

    # Engram (local check)
    engram_db = '/opt/lumina-fleet/engram/engram.db'
    engram_ok = os.path.exists(engram_db)
    services['engram'] = {
        'name': 'Engram', 'ok': engram_ok,
        'fact_count': None,  # Filled by engram refresher if available
    }

    # Plane (via plane-helper — rate-limited, retrying)
    import sys as _s
    _s.path.insert(0, '/opt/plane-helper')
    try:
        from plane_helper import PlaneClient
        _t0 = time.time()
        plane = PlaneClient()
        pl_data = plane.get('/workspaces/moosenet/projects/')
        pl_ms = int((time.time() - _t0) * 1000)
        projects = pl_data.get('results', pl_data) if isinstance(pl_data, dict) else pl_data
        project_count = len(projects)
        pl_ok = True
        pl_err = None
    except Exception as _pe:
        pl_ok = False; pl_ms = 0; project_count = 0; pl_err = str(_pe)[:80]
    services['plane'] = {
        'name': 'Plane', 'ok': pl_ok,
        'latency_ms': pl_ms,
        'project_count': project_count,
        'error': pl_err,
    }

    # Synapse (config + daily count from gate_log)
    try:
        import yaml as _yaml
        _cy = _yaml.safe_load(Path('/opt/lumina-fleet/constellation.yaml').read_text()) if Path('/opt/lumina-fleet/constellation.yaml').exists() else {}
        _sy_cfg = _cy.get('synapse', {})
        _sy_enabled = _sy_cfg.get('enabled', False)
        _sy_strength = _sy_cfg.get('strength', 'moderate')
        _sy_max = _sy_cfg.get('max_messages_per_day', 3)
        # Count today's sent messages from gate_log.json
        _sy_sent = 0
        _gate_log = Path('/opt/lumina-fleet/synapse/gate_log.json')
        if _gate_log.exists():
            import json as _j, datetime as _dt
            _today = _dt.datetime.now(_dt.timezone.utc).date().isoformat()
            _log = _j.loads(_gate_log.read_text())
            _sy_sent = sum(
                1 for e in _log
                if e.get('sent', False) and str(e.get('ts', '')).startswith(_today[:10])
                or (e.get('ts') and _dt.datetime.utcfromtimestamp(e['ts']).strftime('%Y-%m-%d') == _today)
            )
        # Check mute marker
        _markers = {}
        _pm = Path('/opt/lumina-fleet/pulse/markers.json')
        if _pm.exists():
            _markers = json.loads(_pm.read_text())
        _sy_muted = time.time() < _markers.get('synapse_muted_until', 0)
        services['synapse'] = {
            'name': 'Synapse',
            'ok': True,        # Synapse is always "ok" when configured; use 'enabled' for state
            'enabled': _sy_enabled,
            'strength': _sy_strength,
            'sent_today': _sy_sent,
            'max_per_day': _sy_max,
            'muted': _sy_muted,
        }
    except Exception:
        services['synapse'] = {'name': 'Synapse', 'ok': False, 'enabled': False, 'sent_today': 0, 'max_per_day': 3, 'muted': False}

    # Overall status
    core_services = ['ironclaw']
    core_ok = all(services[s]['ok'] for s in core_services if s in services)
    any_down = any(not v['ok'] for v in services.values())

    if core_ok and not any_down:
        status_label = 'ALL SYSTEMS OK'
        status_level = 'ok'
    elif core_ok and any_down:
        down_names = [v['name'] for v in services.values() if not v['ok']]
        status_label = f"DEGRADED — {', '.join(down_names)} unavailable"
        status_level = 'degraded'
    else:
        status_label = 'CRITICAL — core services down'
        status_level = 'critical'

    # Cost summary (from Myelin output file)
    cost = None
    try:
        usage_file = '/opt/lumina-fleet/myelin/output/usage.json'
        import json as _json
        with open(usage_file) as f:
            usage = _json.load(f)
        cost = {
            'today_usd': round(usage.get('today_usd', 0), 4),
            'python_usd': round(usage.get('python_usd', 0), 4),
            'local_usd': round(usage.get('local_usd', 0), 4),
            'cloud_usd': round(usage.get('cloud_usd', 0), 4),
            'budget_warn': 2.0,
            'budget_hard': 10.0,
            'status': 'ok' if usage.get('today_usd', 0) < 2.0 else
                      ('warn' if usage.get('today_usd', 0) < 10.0 else 'critical'),
        }
    except Exception:
        pass

    return {
        'ok': True,
        'status': status_label,
        'status_level': status_level,
        'services': services,
        'cost': cost,
        'checked_at': datetime.now(timezone.utc).isoformat(),
    }
