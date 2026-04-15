#!/usr/bin/env python3
"""
dura_secret_rotation.py — Weekly Infisical secret rotation checker (DUR P4)
fleet/dura/dura_secret_rotation.py

Queries Infisical for current secrets, checks last-rotation dates against
configured max-age policies, logs audit trail to SQLite, and outputs
rotation recommendations. Auto-rotates 'auto' method secrets.

Usage:
    python3 dura_secret_rotation.py [--check | --rotate | --audit | --json]

No LLM — pure Python policy enforcement.
"""
import os
import json
import sqlite3
import logging
import subprocess
from datetime import datetime, timedelta, timezone
from pathlib import Path

# ── Config ─────────────────────────────────────────────────────────────────────
INFISICAL_HOST = os.environ.get('INFISICAL_HOST', '')
INFISICAL_TOKEN = os.environ.get('INFISICAL_TOKEN', '')
INFISICAL_PROJECT_SLUG = os.environ.get('INFISICAL_PROJECT_SLUG', 'moosenet-services')
INFISICAL_ENV = os.environ.get('INFISICAL_ENV', 'prod')

DB_PATH = os.environ.get('DURA_DB_PATH', '/opt/lumina-fleet/dura/dura.db')
LOG_FILE = os.environ.get('DURA_LOG_FILE', '/opt/lumina-fleet/dura/logs/dura_secret_rotation.log')
ENV_FILE = '/opt/lumina-fleet/axon/.env'

# Secret rotation policy: name → {max_age_days, method}
# method: 'auto' = generate + push to Infisical; 'manual' = alert only
SECRET_POLICY = {
    'SOMA_JWT_SECRET':          {'max_age_days': 90,  'method': 'auto'},
    'IRONCLAW_GATEWAY_TOKEN':   {'max_age_days': 90,  'method': 'auto'},
    'LITELLM_MASTER_KEY':       {'max_age_days': 180, 'method': 'auto'},
    'GITEA_TOKEN':              {'max_age_days': 365, 'method': 'auto'},
    'PLANE_API_TOKEN':          {'max_age_days': 365, 'method': 'manual'},
    'OPENROUTER_API_KEY':       {'max_age_days': 365, 'method': 'manual'},
    'INBOX_DB_PASS':            {'max_age_days': 365, 'method': 'manual'},
    'TOMTOM_API_KEY':           {'max_age_days': 365, 'method': 'manual'},
    'NEWS_API_KEY':             {'max_age_days': 365, 'method': 'manual'},
}


def setup_logging():
    Path(LOG_FILE).parent.mkdir(parents=True, exist_ok=True)
    logging.basicConfig(
        level=logging.INFO,
        format='%(asctime)s [dura-rotation] %(levelname)s %(message)s',
        handlers=[
            logging.FileHandler(LOG_FILE),
            logging.StreamHandler(),
        ]
    )


def ensure_db(db_path: str):
    conn = sqlite3.connect(db_path)
    conn.executescript("""
        CREATE TABLE IF NOT EXISTS rotation_audit (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp TEXT NOT NULL DEFAULT (datetime('now')),
            secret_name TEXT NOT NULL,
            action TEXT NOT NULL,
            status TEXT NOT NULL,
            method TEXT DEFAULT 'manual',
            age_days INTEGER DEFAULT 0,
            max_age_days INTEGER DEFAULT 0,
            notes TEXT DEFAULT ''
        );
        CREATE INDEX IF NOT EXISTS idx_rotation_name ON rotation_audit(secret_name, timestamp);
    """)
    conn.commit()
    conn.close()


def log_audit(db_path: str, secret_name: str, action: str, status: str,
              method: str = 'manual', age_days: int = 0, max_age_days: int = 0,
              notes: str = ''):
    conn = sqlite3.connect(db_path)
    conn.execute("""
        INSERT INTO rotation_audit (secret_name, action, status, method, age_days, max_age_days, notes)
        VALUES (?, ?, ?, ?, ?, ?, ?)
    """, (secret_name, action, status, method, age_days, max_age_days, notes))
    conn.commit()
    conn.close()


def get_last_rotation(db_path: str, secret_name: str) -> datetime | None:
    """Return timestamp of last successful rotation from audit log."""
    conn = sqlite3.connect(db_path)
    cur = conn.cursor()
    cur.execute("""
        SELECT timestamp FROM rotation_audit
        WHERE secret_name = ? AND action = 'rotate' AND status = 'success'
        ORDER BY timestamp DESC LIMIT 1
    """, (secret_name,))
    row = cur.fetchone()
    conn.close()
    if row:
        try:
            return datetime.fromisoformat(row[0]).replace(tzinfo=timezone.utc)
        except Exception:
            pass
    return None


def fetch_infisical_secrets() -> dict[str, dict]:
    """Fetch secrets from Infisical API. Returns {name: {created_at, updated_at, version}}."""
    if not INFISICAL_HOST or not INFISICAL_TOKEN:
        logging.warning("Infisical not configured (INFISICAL_HOST or INFISICAL_TOKEN missing)")
        return {}
    try:
        import urllib.request
        url = f"{INFISICAL_HOST}/api/v3/secrets/raw?workspaceSlug={INFISICAL_PROJECT_SLUG}&environment={INFISICAL_ENV}"
        req = urllib.request.Request(url, headers={
            'Authorization': f'Bearer {INFISICAL_TOKEN}',
            'Content-Type': 'application/json',
        })
        resp = urllib.request.urlopen(req, timeout=10)
        data = json.loads(resp.read())
        secrets = {}
        for s in data.get('secrets', []):
            secrets[s['secretKey']] = {
                'created_at': s.get('createdAt', ''),
                'updated_at': s.get('updatedAt', ''),
                'version': s.get('version', 1),
            }
        return secrets
    except Exception as e:
        logging.warning(f"Infisical fetch failed: {e}")
        return {}


def generate_token(length: int = 32) -> str:
    """Generate a secure random hex token."""
    import secrets
    return secrets.token_hex(length)


def push_to_infisical(secret_name: str, new_value: str) -> bool:
    """Update a secret value in Infisical via API."""
    if not INFISICAL_HOST or not INFISICAL_TOKEN:
        logging.warning(f"Cannot push {secret_name} — Infisical not configured")
        return False
    try:
        import urllib.request
        payload = json.dumps({
            'workspaceSlug': INFISICAL_PROJECT_SLUG,
            'environment': INFISICAL_ENV,
            'secretKey': secret_name,
            'secretValue': new_value,
        }).encode()
        url = f"{INFISICAL_HOST}/api/v3/secrets/raw/{secret_name}"
        req = urllib.request.Request(url, data=payload, method='PATCH', headers={
            'Authorization': f'Bearer {INFISICAL_TOKEN}',
            'Content-Type': 'application/json',
        })
        urllib.request.urlopen(req, timeout=10)
        return True
    except Exception as e:
        logging.error(f"Infisical push failed for {secret_name}: {e}")
        return False


def check_rotation_status(db_path: str) -> list[dict]:
    """Check all secrets against policy and return status list."""
    now = datetime.now(timezone.utc)
    infisical_secrets = fetch_infisical_secrets()
    results = []

    for name, policy in SECRET_POLICY.items():
        max_age = policy['max_age_days']
        method = policy['method']

        # Try to get last rotation from audit log first (most reliable)
        last_rotated = get_last_rotation(db_path, name)

        # Fall back to Infisical updated_at
        if not last_rotated and name in infisical_secrets:
            updated_str = infisical_secrets[name].get('updated_at', '')
            if updated_str:
                try:
                    last_rotated = datetime.fromisoformat(updated_str.replace('Z', '+00:00'))
                except Exception:
                    pass

        if last_rotated:
            age_days = (now - last_rotated).days
        else:
            age_days = max_age + 1  # Unknown — treat as overdue

        pct = round(age_days / max_age * 100) if max_age else 0
        overdue = age_days >= max_age
        warn = pct >= 80

        status = 'overdue' if overdue else ('warn' if warn else 'ok')
        results.append({
            'name': name,
            'method': method,
            'max_age_days': max_age,
            'age_days': age_days,
            'pct': pct,
            'status': status,
            'last_rotated': last_rotated.isoformat() if last_rotated else None,
            'in_infisical': name in infisical_secrets,
        })

    return sorted(results, key=lambda x: (-x['pct'], x['name']))


def do_rotate(db_path: str, dry_run: bool = False) -> list[dict]:
    """Rotate all overdue auto-method secrets. Returns rotation log."""
    results = check_rotation_status(db_path)
    rotated = []

    for r in results:
        if r['status'] not in ('overdue', 'warn') or r['method'] != 'auto':
            continue

        name = r['name']
        logging.info(f"Rotating {name} (age={r['age_days']}d, {r['pct']}% of {r['max_age_days']}d limit)")

        if dry_run:
            logging.info(f"  [dry-run] Would generate + push {name}")
            log_audit(db_path, name, 'rotate_dry_run', 'skipped',
                      method=r['method'], age_days=r['age_days'],
                      max_age_days=r['max_age_days'], notes='dry-run')
            rotated.append({'name': name, 'status': 'dry-run'})
            continue

        new_val = generate_token(32)
        success = push_to_infisical(name, new_val)

        status = 'success' if success else 'failed'
        notes = 'pushed to Infisical' if success else 'Infisical push failed'
        log_audit(db_path, name, 'rotate', status,
                  method=r['method'], age_days=r['age_days'],
                  max_age_days=r['max_age_days'], notes=notes)

        if success:
            logging.info(f"  ✓ Rotated {name} — new value pushed to Infisical")
        else:
            logging.error(f"  ✗ Failed to rotate {name}")

        rotated.append({'name': name, 'status': status})

    return rotated


def get_audit_log(db_path: str, limit: int = 50) -> list[dict]:
    """Return recent audit log entries."""
    conn = sqlite3.connect(db_path)
    cur = conn.cursor()
    cur.execute("""
        SELECT timestamp, secret_name, action, status, method, age_days, max_age_days, notes
        FROM rotation_audit ORDER BY timestamp DESC LIMIT ?
    """, (limit,))
    rows = cur.fetchall()
    conn.close()
    keys = ['timestamp', 'secret_name', 'action', 'status', 'method', 'age_days', 'max_age_days', 'notes']
    return [dict(zip(keys, r)) for r in rows]


def main():
    import argparse
    parser = argparse.ArgumentParser(description='Dura secret rotation checker')
    parser.add_argument('--check', action='store_true', help='Check rotation status only')
    parser.add_argument('--rotate', action='store_true', help='Rotate overdue auto-method secrets')
    parser.add_argument('--dry-run', action='store_true', help='With --rotate: simulate without changing')
    parser.add_argument('--audit', action='store_true', help='Show recent audit log')
    parser.add_argument('--json', action='store_true', help='Output JSON')
    args = parser.parse_args()

    setup_logging()
    ensure_db(DB_PATH)

    if args.audit:
        entries = get_audit_log(DB_PATH)
        if args.json:
            print(json.dumps(entries, indent=2))
        else:
            for e in entries:
                icon = '✓' if e['status'] == 'success' else '✗'
                print(f"  {e['timestamp']} {icon} {e['secret_name']} [{e['action']}] {e['notes']}")
        return

    if args.rotate:
        rotated = do_rotate(DB_PATH, dry_run=args.dry_run)
        if args.json:
            print(json.dumps(rotated, indent=2))
        else:
            if not rotated:
                print("[dura-rotation] No secrets due for rotation.")
            for r in rotated:
                icon = '✓' if r['status'] == 'success' else ('~' if r['status'] == 'dry-run' else '✗')
                print(f"  {icon} {r['name']}: {r['status']}")
        return

    # Default: --check
    results = check_rotation_status(DB_PATH)

    # Log the check itself
    for r in results:
        log_audit(DB_PATH, r['name'], 'check', r['status'],
                  method=r['method'], age_days=r['age_days'],
                  max_age_days=r['max_age_days'])

    if args.json:
        print(json.dumps({
            'secrets': results,
            'checked_at': datetime.now(timezone.utc).strftime('%Y-%m-%d %H:%M UTC'),
        }, indent=2))
        return

    print(f"[dura-rotation] Secret rotation status — {datetime.now(timezone.utc).strftime('%Y-%m-%d %H:%M UTC')}")
    print()
    for r in results:
        icon = {'overdue': '🔴', 'warn': '🟡', 'ok': '🟢'}.get(r['status'], '⚪')
        age_str = f"{r['age_days']}d" if r['age_days'] <= r['max_age_days'] + 365 else 'unknown'
        method_label = '[auto]' if r['method'] == 'auto' else '[manual]'
        print(f"  {icon} {r['name']:<30} {age_str:>6} / {r['max_age_days']}d  {r['pct']:>3}%  {method_label}")

    overdue = [r for r in results if r['status'] == 'overdue']
    warn = [r for r in results if r['status'] == 'warn']
    if overdue:
        print(f"\n  ⚠ {len(overdue)} secret(s) overdue for rotation. Run --rotate to update auto-method secrets.")
    elif warn:
        print(f"\n  ⚠ {len(warn)} secret(s) approaching rotation age.")
    else:
        print(f"\n  ✓ All {len(results)} secrets within rotation policy.")


if __name__ == '__main__':
    main()
