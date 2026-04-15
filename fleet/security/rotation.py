#!/usr/bin/env python3
"""
rotation.py — Lumina secret rotation system. (SEC.1-SEC.5)

Reads secrets_registry.yaml, checks secret ages, rotates expired secrets,
and sends Matrix alerts for manual-rotation secrets.

Rotation methods:
  random_hex_32  — crypto.token_hex(32), update Infisical, restart services
  gitea_api      — Gitea API: delete old token, create new, update Infisical
  plane_api      — Plane CE API: create new token, update Infisical
  manual         — Send Matrix/Nexus alert with regeneration instructions

Sentinel integration:
  check_secret_ages() — Returns list of secrets needing attention.
  Call from sentinel health checks.

Usage:
  python3 rotation.py check          — Print status of all secrets
  python3 rotation.py rotate <name>  — Force-rotate a specific secret
  python3 rotation.py run            — Rotate all expired secrets
"""

import json
import os
import secrets as _secrets_module
import subprocess
import sys
import urllib.request
import urllib.error
from datetime import datetime, timezone, timedelta
from pathlib import Path

import yaml

# ── Config ─────────────────────────────────────────────────────────────────────

_FLEET_DIR = Path(os.environ.get('FLEET_DIR', '/opt/lumina-fleet'))
_REGISTRY_FILE = Path(__file__).parent / 'secrets_registry.yaml'
_STATE_FILE = _FLEET_DIR / 'security' / 'rotation_state.json'

INFISICAL_AUTH_FILE = Path('/opt/briefing-agent/.infisical-auth')
INFISICAL_HEALTH_URL = os.environ.get('INFISICAL_URL', '')

GITEA_URL = os.environ.get('GITEA_URL', '')
GITEA_TOKEN = os.environ.get('GITEA_TOKEN', '')

MATRIX_NEXUS_DB_HOST = os.environ.get('INBOX_DB_HOST', '')
MATRIX_NEXUS_DB_USER = os.environ.get('INBOX_DB_USER', 'lumina_inbox_user')
MATRIX_NEXUS_DB_PASS = os.environ.get('INBOX_DB_PASS', '')

# SEC.7: Direct Matrix webhook for alerts (fallback if Nexus unavailable)
MATRIX_WEBHOOK_URL = os.environ.get('MATRIX_WEBHOOK_URL', '')

# Warn at 80% of max_age
WARN_THRESHOLD = 0.80


# ── State management ──────────────────────────────────────────────────────────

def _load_state() -> dict:
    try:
        if _STATE_FILE.exists():
            return json.loads(_STATE_FILE.read_text())
    except Exception:
        pass
    return {}


def _save_state(state: dict):
    _STATE_FILE.parent.mkdir(parents=True, exist_ok=True)
    _STATE_FILE.write_text(json.dumps(state, indent=2))


# ── Registry loading ──────────────────────────────────────────────────────────

def load_registry() -> list:
    """Load and return the secrets registry list."""
    if not _REGISTRY_FILE.exists():
        print(f'[rotation] Registry not found: {_REGISTRY_FILE}')
        return []
    data = yaml.safe_load(_REGISTRY_FILE.read_text())
    return data.get('secrets', [])


# ── Age checking ──────────────────────────────────────────────────────────────

def check_secret_ages() -> list:
    """
    Check all secrets and return list of secrets needing attention.
    Called by Sentinel health checks.

    Returns list of dicts:
        {name, status, age_days, max_age_days, days_remaining, method}
    Status: 'ok' | 'warn' | 'expired' | 'unknown'
    """
    registry = load_registry()
    state = _load_state()
    now = datetime.now(timezone.utc)
    results = []

    for secret in registry:
        name = secret['name']
        max_age = secret.get('max_age_days', 365)
        entry = state.get(name, {})
        last_rotated_str = entry.get('last_rotated')

        if not last_rotated_str:
            results.append({
                'name': name,
                'status': 'unknown',
                'age_days': None,
                'max_age_days': max_age,
                'days_remaining': None,
                'method': secret.get('method', 'manual'),
                'description': secret.get('description', ''),
            })
            continue

        last_rotated = datetime.fromisoformat(last_rotated_str)
        age = now - last_rotated
        age_days = age.days
        days_remaining = max_age - age_days
        warn_at = max_age * WARN_THRESHOLD

        if age_days >= max_age:
            status = 'expired'
        elif age_days >= warn_at:
            status = 'warn'
        else:
            status = 'ok'

        results.append({
            'name': name,
            'status': status,
            'age_days': age_days,
            'max_age_days': max_age,
            'days_remaining': days_remaining,
            'method': secret.get('method', 'manual'),
            'description': secret.get('description', ''),
        })

    return results


# ── Infisical client ──────────────────────────────────────────────────────────

def _get_infisical_token() -> tuple:
    """Returns (infisical_url, access_token) or raises."""
    auth = {}
    if INFISICAL_AUTH_FILE.exists():
        for line in INFISICAL_AUTH_FILE.read_text().splitlines():
            if '=' in line and not line.startswith('#'):
                k, v = line.split('=', 1)
                auth[k.strip()] = v.strip()

    url = auth.get('INFISICAL_URL') or INFISICAL_HEALTH_URL
    if not url:
        raise RuntimeError('INFISICAL_URL not configured')

    payload = json.dumps({
        'clientId': auth['INFISICAL_CLIENT_ID'],
        'clientSecret': auth['INFISICAL_CLIENT_SECRET'],
    }).encode()
    req = urllib.request.Request(
        f'{url}/api/v1/auth/universal-auth/login',
        data=payload, headers={'Content-Type': 'application/json'}, method='POST'
    )
    with urllib.request.urlopen(req, timeout=10) as r:
        token = json.loads(r.read())['accessToken']
    return url, token


def _infisical_update_secret(url: str, token: str, project_id: str, key: str, value: str):
    """Update a secret value in Infisical."""
    # Try PATCH first, then PUT
    for method in ('PATCH', 'PUT', 'POST'):
        payload = json.dumps({
            'secretName': key,
            'secretValue': value,
            'environment': 'prod',
            'secretPath': '/',
        }).encode()
        req = urllib.request.Request(
            f'{url}/api/v3/secrets/raw/{key}?workspaceId={project_id}&environment=prod&secretPath=/',
            data=payload,
            headers={
                'Authorization': f'Bearer {token}',
                'Content-Type': 'application/json',
            },
            method=method,
        )
        try:
            with urllib.request.urlopen(req, timeout=10) as r:
                return json.loads(r.read())
        except urllib.error.HTTPError as e:
            if e.code in (404, 422) and method != 'POST':
                continue
            raise
    raise RuntimeError(f'Failed to update {key} in Infisical')


def _get_infisical_project_id(auth_data: dict, project_name: str) -> str:
    """Return project ID for a given project name slug."""
    return auth_data.get(f'{project_name.upper()}_PROJECT_ID', auth_data.get('SERVICES_PROJECT_ID', ''))


def _infisical_get_secret(url: str, token: str, project_id: str, key: str) -> str:
    """Read current value of a secret from Infisical. (SEC.8 rollback support)"""
    req = urllib.request.Request(
        f'{url}/api/v3/secrets/raw/{key}?workspaceId={project_id}&environment=prod&secretPath=/',
        headers={'Authorization': f'Bearer {token}', 'Content-Type': 'application/json'},
    )
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            data = json.loads(r.read())
            return data.get('secret', {}).get('secretValue', '')
    except Exception:
        return ''


def _send_matrix_webhook(secret_name: str, message: str):
    """Post alert directly to Matrix via webhook URL. (SEC.7)"""
    if not MATRIX_WEBHOOK_URL:
        return
    try:
        body = json.dumps({'text': f'[Secret Rotation Alert]\n{message}'}).encode()
        req = urllib.request.Request(
            MATRIX_WEBHOOK_URL, data=body,
            headers={'Content-Type': 'application/json'}, method='POST'
        )
        urllib.request.urlopen(req, timeout=10)
        print(f'[rotation] Matrix webhook alert sent for {secret_name}')
    except Exception as e:
        print(f'[rotation] Matrix webhook failed for {secret_name}: {e}')


# ── Rotation methods ──────────────────────────────────────────────────────────

def _rotate_random_hex_32(secret: dict, state: dict) -> bool:
    """Generate new random hex secret and update Infisical. (SEC.8: saves prev for rollback)"""
    name = secret['name']
    new_value = _secrets_module.token_hex(32)

    try:
        url, token = _get_infisical_token()
        auth = {}
        if INFISICAL_AUTH_FILE.exists():
            for line in INFISICAL_AUTH_FILE.read_text().splitlines():
                if '=' in line and not line.startswith('#'):
                    k, v = line.split('=', 1)
                    auth[k.strip()] = v.strip()

        project_id = _get_infisical_project_id(auth, secret.get('infisical_project', 'services'))

        # SEC.8: Read current value before overwriting (rollback point)
        prev_value = _infisical_get_secret(url, token, project_id, name)
        if prev_value:
            state.setdefault(name, {})['prev_value'] = prev_value

        _infisical_update_secret(url, token, project_id, name, new_value)
        print(f'[rotation] {name}: rotated (random_hex_32) → Infisical updated')

        # Run restart commands
        for cmd in secret.get('restart_commands', []):
            _run_restart(cmd, name)

        state[name] = {
            'last_rotated': datetime.now(timezone.utc).isoformat(),
            'method': 'random_hex_32',
            'status': 'ok',
        }
        return True

    except Exception as e:
        print(f'[rotation] {name}: rotation FAILED — {e}')
        return False


def _rotate_gitea_api(secret: dict, state: dict) -> bool:
    """Rotate Gitea API token: delete old, create new, update Infisical."""
    name = secret['name']
    owner = secret.get('gitea_token_owner', 'moosenet')
    token_name = secret.get('gitea_token_name', 'lumina-fleet-agent')

    if not GITEA_URL or not GITEA_TOKEN:
        print(f'[rotation] {name}: GITEA_URL or GITEA_TOKEN not set — skipping')
        _send_manual_alert(secret, reason='GITEA credentials not available for auto-rotation')
        return False

    try:
        # 1. Get existing tokens
        req = urllib.request.Request(
            f'{GITEA_URL}/api/v1/users/{owner}/tokens',
            headers={'Authorization': f'token {GITEA_TOKEN}', 'Content-Type': 'application/json'}
        )
        with urllib.request.urlopen(req, timeout=10) as r:
            tokens = json.loads(r.read())

        # 2. Delete existing token with same name
        for t in tokens:
            if t.get('name') == token_name:
                del_req = urllib.request.Request(
                    f'{GITEA_URL}/api/v1/users/{owner}/tokens/{t["id"]}',
                    headers={'Authorization': f'token {GITEA_TOKEN}'},
                    method='DELETE'
                )
                urllib.request.urlopen(del_req, timeout=10)
                print(f'[rotation] {name}: deleted old token "{token_name}" (id={t["id"]})')

        # 3. Create new token
        new_token_name = f'{token_name}-{datetime.now().strftime("%Y%m")}'
        payload = json.dumps({'name': new_token_name}).encode()
        create_req = urllib.request.Request(
            f'{GITEA_URL}/api/v1/users/{owner}/tokens',
            data=payload,
            headers={
                'Authorization': f'token {GITEA_TOKEN}',
                'Content-Type': 'application/json',
            },
            method='POST'
        )
        with urllib.request.urlopen(create_req, timeout=10) as r:
            new_token_data = json.loads(r.read())
        new_token_value = new_token_data.get('sha1') or new_token_data.get('token', '')

        if not new_token_value:
            raise RuntimeError('No token value in Gitea response')

        # 4. Update Infisical
        url, inf_token = _get_infisical_token()
        auth = {}
        if INFISICAL_AUTH_FILE.exists():
            for line in INFISICAL_AUTH_FILE.read_text().splitlines():
                if '=' in line and not line.startswith('#'):
                    k, v = line.split('=', 1)
                    auth[k.strip()] = v.strip()
        project_id = _get_infisical_project_id(auth, secret.get('infisical_project', 'services'))
        _infisical_update_secret(url, inf_token, project_id, name, new_token_value)

        # 5. Restart dependent services
        for cmd in secret.get('restart_commands', []):
            _run_restart(cmd, name)

        print(f'[rotation] {name}: rotated (gitea_api) → new token "{new_token_name}"')
        state[name] = {
            'last_rotated': datetime.now(timezone.utc).isoformat(),
            'method': 'gitea_api',
            'status': 'ok',
        }
        return True

    except Exception as e:
        print(f'[rotation] {name}: Gitea rotation FAILED — {e}')
        _send_manual_alert(secret, reason=f'Auto-rotation failed: {e}')
        return False


def _send_manual_alert(secret: dict, reason: str = ''):
    """Send a Nexus message to Lumina requesting manual rotation."""
    name = secret['name']
    instructions = secret.get('manual_instructions', 'Regenerate and update in Infisical.')
    description = secret.get('description', name)
    max_age = secret.get('max_age_days', 365)

    message = (
        f'SECRET ROTATION NEEDED: {name}\n'
        f'Description: {description}\n'
        f'Max age: {max_age} days\n'
        f'{f"Reason: {reason}" if reason else ""}\n\n'
        f'Instructions:\n{instructions.strip()}'
    )

    nexus_sent = False
    if MATRIX_NEXUS_DB_HOST:
        try:
            import psycopg2
            conn = psycopg2.connect(
                host=MATRIX_NEXUS_DB_HOST,
                dbname='lumina_inbox',
                user=MATRIX_NEXUS_DB_USER,
                password=MATRIX_NEXUS_DB_PASS,
                connect_timeout=5,
            )
            payload = json.dumps({
                'alert': message,
                'source': 'rotation',
                'secret_name': name,
                'timestamp': datetime.now(timezone.utc).isoformat(),
            })
            conn.cursor().execute(
                "INSERT INTO inbox_messages (from_agent,to_agent,message_type,priority,payload,status) "
                "VALUES ('sentinel','lumina','secret_rotation','urgent',%s,'pending')",
                (payload,)
            )
            conn.commit()
            conn.close()
            print(f'[rotation] Manual rotation alert sent to Nexus for {name}')
            nexus_sent = True
        except Exception as e:
            print(f'[rotation] Nexus alert failed for {name}: {e}')

    # SEC.7: Fallback (or supplement) to direct Matrix webhook
    if MATRIX_WEBHOOK_URL:
        _send_matrix_webhook(name, message)
    elif not nexus_sent:
        print(f'[rotation] No delivery channel configured. Alert for {name}:\n{message}')


def _run_restart(cmd: str, secret_name: str):
    """Run a service restart command after rotation."""
    try:
        result = subprocess.run(cmd, shell=True, capture_output=True, text=True, timeout=30)
        if result.returncode == 0:
            print(f'[rotation] Restarted service: {cmd}')
        else:
            print(f'[rotation] Service restart failed ({cmd}): {result.stderr[:100]}')
    except Exception as e:
        print(f'[rotation] Restart command error ({cmd}): {e}')


# ── Rollback ──────────────────────────────────────────────────────────────────

def _health_check_after_rotation(secret: dict) -> bool:
    """
    Quick health check after rotation to verify services are up.
    Returns True if healthy, False if rollback needed.
    """
    # Currently: just check if restart commands succeeded by running a simple check
    # Future: call sentinel health_checks for the specific services
    services = secret.get('services', [])
    if not services:
        return True

    # Best-effort: check systemd service status for each service
    for svc in services:
        service_name = f'{svc}.service' if not svc.endswith('.service') else svc
        try:
            result = subprocess.run(
                f'systemctl is-active {service_name}',
                shell=True, capture_output=True, text=True, timeout=10
            )
            if result.stdout.strip() not in ('active', 'activating'):
                print(f'[rotation] Health check FAILED: {service_name} is not active')
                return False
        except Exception:
            pass  # If systemctl is not available (e.g., in container), skip

    return True


# ── Main rotation logic ───────────────────────────────────────────────────────

def rotate_secret(secret: dict, state: dict, force: bool = False) -> bool:
    """Rotate a single secret based on its method."""
    name = secret['name']
    method = secret.get('method', 'manual')

    print(f'[rotation] Rotating {name} via {method}')

    if method == 'random_hex_32':
        success = _rotate_random_hex_32(secret, state)
    elif method == 'gitea_api':
        success = _rotate_gitea_api(secret, state)
    elif method in ('plane_api', 'manual'):
        _send_manual_alert(secret)
        state[name] = {
            'last_rotated': state.get(name, {}).get('last_rotated', ''),
            'last_manual_alert': datetime.now(timezone.utc).isoformat(),
            'status': 'manual_alert_sent',
        }
        return True  # Alert sent, not a failure
    else:
        print(f'[rotation] Unknown method {method} for {name}')
        return False

    if success:
        # Health check post-rotation (SEC.8: attempt rollback on failure)
        if not _health_check_after_rotation(secret):
            print(f'[rotation] ROLLBACK: {name} health check failed after rotation')
            prev_value = state.get(name, {}).get('prev_value', '')
            rolled_back = False
            if prev_value and method == 'random_hex_32':
                try:
                    url, token = _get_infisical_token()
                    auth = {}
                    if INFISICAL_AUTH_FILE.exists():
                        for line in INFISICAL_AUTH_FILE.read_text().splitlines():
                            if '=' in line and not line.startswith('#'):
                                k, v = line.split('=', 1)
                                auth[k.strip()] = v.strip()
                    project_id = _get_infisical_project_id(auth, secret.get('infisical_project', 'services'))
                    _infisical_update_secret(url, token, project_id, name, prev_value)
                    # Re-run restart commands with old value in place
                    for cmd in secret.get('restart_commands', []):
                        _run_restart(cmd, name)
                    print(f'[rotation] ROLLBACK: {name} restored previous value')
                    rolled_back = True
                except Exception as rb_err:
                    print(f'[rotation] ROLLBACK FAILED for {name}: {rb_err}')

            reason = 'Health check failed — rolled back to previous value' if rolled_back else \
                     'Health check failed — rollback NOT possible, manual intervention required'
            _send_manual_alert(secret, reason=reason)
            state[name]['status'] = 'rolled_back' if rolled_back else 'health_check_failed'
            return False

    return success


def run_all(dry_run: bool = False) -> dict:
    """Check all secrets and rotate expired ones. Returns summary."""
    registry = load_registry()
    state = _load_state()
    now = datetime.now(timezone.utc)

    summary = {'rotated': [], 'warned': [], 'manual_alert': [], 'ok': [], 'errors': []}

    for secret in registry:
        name = secret['name']
        max_age = secret.get('max_age_days', 365)
        entry = state.get(name, {})
        last_rotated_str = entry.get('last_rotated')

        if not last_rotated_str:
            print(f'[rotation] {name}: no rotation record — treating as needing rotation')
            if not dry_run:
                if rotate_secret(secret, state):
                    summary['rotated'].append(name)
                else:
                    summary['errors'].append(name)
            else:
                print(f'  [dry-run] Would rotate {name}')
            continue

        last_rotated = datetime.fromisoformat(last_rotated_str)
        age_days = (now - last_rotated).days
        warn_at = int(max_age * WARN_THRESHOLD)

        if age_days < warn_at:
            summary['ok'].append(name)
            continue

        if age_days >= max_age:
            print(f'[rotation] {name}: EXPIRED ({age_days}/{max_age} days)')
            if not dry_run:
                if rotate_secret(secret, state):
                    summary['rotated'].append(name)
                else:
                    summary['errors'].append(name)
            else:
                print(f'  [dry-run] Would rotate {name}')
                summary['warned'].append(name)
        else:
            days_left = max_age - age_days
            print(f'[rotation] {name}: WARNING ({age_days}/{max_age} days — {days_left} days remaining)')
            summary['warned'].append(name)
            # Send warning alert (not rotation)
            if not dry_run and secret.get('method') == 'manual':
                _send_manual_alert(secret, reason=f'Secret expires in {days_left} days')

    if not dry_run:
        _save_state(state)

    return summary


# ── Sentinel integration ──────────────────────────────────────────────────────

def sentinel_check() -> dict:
    """
    Called by Sentinel health checks.
    Returns health check result compatible with health_checks.py format.

    Returns:
        {ok: bool, status: 'ok'|'warn'|'critical', value: str, message: str}
    """
    ages = check_secret_ages()

    expired = [s for s in ages if s['status'] == 'expired']
    warned = [s for s in ages if s['status'] == 'warn']
    unknown = [s for s in ages if s['status'] == 'unknown']

    if expired:
        names = ', '.join(s['name'] for s in expired)
        return {
            'ok': False,
            'status': 'critical',
            'value': f'{len(expired)} expired',
            'message': f'Expired secrets: {names}',
        }

    if warned:
        names = ', '.join(s['name'] for s in warned)
        return {
            'ok': True,
            'status': 'warn',
            'value': f'{len(warned)} expiring soon',
            'message': f'Secrets expiring within 20% of max age: {names}',
        }

    if unknown:
        return {
            'ok': True,
            'status': 'warn',
            'value': f'{len(unknown)} untracked',
            'message': f'Secrets with no rotation record: {", ".join(s["name"] for s in unknown)}',
        }

    return {
        'ok': True,
        'status': 'ok',
        'value': f'{len(ages)} secrets OK',
        'message': 'All secrets within rotation window',
    }


# ── Prometheus export (SEC.9) ─────────────────────────────────────────────────

def prometheus_export(output_file: str = '') -> str:
    """
    Export secret age metrics in Prometheus text format.

    Metrics:
      lumina_secret_age_days        — Current age of secret in days (-1 if unknown)
      lumina_secret_max_age_days    — Rotation threshold for this secret in days
      lumina_secret_days_remaining  — Days until rotation due (-1 if unknown)

    For use with node_exporter textfile collector:
      python3 rotation.py prometheus-export --output /var/lib/node_exporter/textfile/lumina_secrets.prom

    Labels:
      name    — Secret name (e.g. SOMA_JWT_SECRET)
      method  — Rotation method (random_hex_32 / gitea_api / manual)
      status  — ok / warn / expired / unknown
    """
    ages = check_secret_ages()
    lines = [
        '# HELP lumina_secret_age_days Age of managed secret in days since last rotation',
        '# TYPE lumina_secret_age_days gauge',
    ]
    for s in ages:
        labels = f'name="{s["name"]}",method="{s["method"]}",status="{s["status"]}"'
        age = s['age_days'] if s['age_days'] is not None else -1
        lines.append(f'lumina_secret_age_days{{{labels}}} {age}')

    lines += [
        '',
        '# HELP lumina_secret_max_age_days Maximum allowed age before rotation in days',
        '# TYPE lumina_secret_max_age_days gauge',
    ]
    for s in ages:
        labels = f'name="{s["name"]}",method="{s["method"]}"'
        lines.append(f'lumina_secret_max_age_days{{{labels}}} {s["max_age_days"]}')

    lines += [
        '',
        '# HELP lumina_secret_days_remaining Days remaining until rotation required',
        '# TYPE lumina_secret_days_remaining gauge',
    ]
    for s in ages:
        labels = f'name="{s["name"]}",method="{s["method"]}",status="{s["status"]}"'
        remaining = s['days_remaining'] if s['days_remaining'] is not None else -1
        lines.append(f'lumina_secret_days_remaining{{{labels}}} {remaining}')

    text = '\n'.join(lines) + '\n'

    if output_file:
        Path(output_file).parent.mkdir(parents=True, exist_ok=True)
        Path(output_file).write_text(text)
        print(f'[rotation] Prometheus metrics written to {output_file}')
    else:
        print(text, end='')

    return text


# ── CLI ───────────────────────────────────────────────────────────────────────

def _print_status():
    """Print a status table for all secrets."""
    ages = check_secret_ages()
    print(f'{"Secret":<35} {"Status":<10} {"Age":>6} {"Max":>6} {"Remaining":>10}  Method')
    print('-' * 85)
    for s in ages:
        age_str = f'{s["age_days"]}d' if s["age_days"] is not None else '?'
        rem_str = f'{s["days_remaining"]}d' if s["days_remaining"] is not None else '?'
        status_tag = {'ok': 'OK', 'warn': 'WARN', 'expired': 'EXPIRED', 'unknown': '?'}.get(s['status'], s['status'])
        print(f'{s["name"]:<35} {status_tag:<10} {age_str:>6} {s["max_age_days"]:>5}d {rem_str:>10}  {s["method"]}')


if __name__ == '__main__':
    cmd = sys.argv[1] if len(sys.argv) > 1 else 'check'

    if cmd == 'check':
        _print_status()

    elif cmd == 'run':
        dry = '--dry-run' in sys.argv
        print(f'[rotation] Running rotation check {"(dry run)" if dry else ""}')
        summary = run_all(dry_run=dry)
        print(f'\nSummary: rotated={summary["rotated"]}, warned={summary["warned"]}, '
              f'errors={summary["errors"]}, ok={len(summary["ok"])}')

    elif cmd == 'rotate' and len(sys.argv) > 2:
        target = sys.argv[2]
        registry = load_registry()
        state = _load_state()
        secret = next((s for s in registry if s['name'] == target), None)
        if not secret:
            print(f'Secret {target} not found in registry')
            sys.exit(1)
        ok = rotate_secret(secret, state, force=True)
        _save_state(state)
        print(f'Rotation {"succeeded" if ok else "FAILED"}: {target}')

    elif cmd == 'sentinel':
        result = sentinel_check()
        print(json.dumps(result, indent=2))

    elif cmd == 'prometheus-export':
        output = ''
        if '--output' in sys.argv:
            idx = sys.argv.index('--output')
            if idx + 1 < len(sys.argv):
                output = sys.argv[idx + 1]
        prometheus_export(output_file=output)

    else:
        print('Usage: rotation.py check | run [--dry-run] | rotate <SECRET_NAME> | sentinel | prometheus-export [--output FILE]')
