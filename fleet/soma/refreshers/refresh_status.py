"""
refresh_status.py — Status page data refresher
fleet/soma/refreshers/refresh_status.py

Probes all Constellation services and returns structured status.
Called by the cache background task. Plain Python, no LLM.
"""

import json
import os
import subprocess
import urllib.request
import urllib.error
from datetime import datetime, timezone


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
    ironclaw_url  = os.environ.get('IRONCLAW_URL', 'http://192.168.0.217:3001')
    ironclaw_token = os.environ.get('IRONCLAW_GATEWAY_TOKEN', '')
    litellm_url   = os.environ.get('LITELLM_URL', 'http://192.168.0.215:4000')
    litellm_key   = os.environ.get('LITELLM_MASTER_KEY', '')
    nexus_db_url  = os.environ.get('NEXUS_API_URL', 'http://localhost:8083')
    matrix_url    = os.environ.get('MATRIX_URL', 'http://192.168.0.208:8008')

    services = {}
    overall_ok = True

    # IronClaw
    ic_ok, ic_data, ic_ms = _probe_http(
        f'{ironclaw_url}/api/health',
        headers={'Authorization': f'Bearer {ironclaw_token}'} if ironclaw_token else {}
    )
    services['ironclaw'] = {
        'name': 'IronClaw', 'ok': ic_ok,
        'latency_ms': ic_ms,
        'version': ic_data.get('version', '?') if ic_ok else None,
        'tool_count': ic_data.get('tool_count', ic_data.get('tools', '?')) if ic_ok else None,
        'error': ic_data.get('error') if not ic_ok else None,
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

    # Nexus (local DB-backed service on CT310)
    nx_ok, nx_data, nx_ms = _probe_http(f'{nexus_db_url}/health')
    services['nexus'] = {
        'name': 'Nexus', 'ok': nx_ok,
        'latency_ms': nx_ms,
        'message_count': nx_data.get('message_count') if nx_ok else None,
        'unacked': nx_data.get('unacked') if nx_ok else None,
        'error': nx_data.get('error') if not nx_ok else None,
    }

    # Engram (local check)
    engram_db = '/opt/lumina-fleet/engram/engram.db'
    engram_ok = os.path.exists(engram_db)
    services['engram'] = {
        'name': 'Engram', 'ok': engram_ok,
        'fact_count': None,  # Filled by engram refresher if available
    }

    # Plane
    plane_url = os.environ.get('PLANE_API_URL', 'http://192.168.0.232')
    plane_token = os.environ.get('PLANE_API_TOKEN', '')
    pl_ok, pl_data, pl_ms = _probe_http(
        f'{plane_url}/api/v1/workspaces/moosenet/projects/',
        timeout=8,
        headers={'X-API-Key': plane_token} if plane_token else {}
    )
    project_count = pl_data.get('count', len(pl_data.get('results', []))) if pl_ok else 0
    services['plane'] = {
        'name': 'Plane', 'ok': pl_ok,
        'latency_ms': pl_ms,
        'project_count': project_count,
        'error': pl_data.get('error') if not pl_ok else None,
    }

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
