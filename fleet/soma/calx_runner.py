#!/usr/bin/env python3
"""
calx_runner.py — Validates system state against Calx rules from Soma insights
fleet/soma/calx_runner.py (SOM P5-17)

Reads engram/system/calx/rules.json (populated by accept_insight endpoint)
and runs each rule against current system state. Outputs pass/fail/skip per rule.

Usage:
    python3 calx_runner.py [--json] [--verbose]
"""
import os
import json
import sqlite3
import subprocess
from datetime import datetime, timezone
from pathlib import Path

FLEET_DIR = Path(os.environ.get('FLEET_DIR', '/opt/lumina-fleet'))
RULES_FILE = FLEET_DIR / 'engram' / 'system' / 'calx' / 'rules.json'
CALX_LOG = FLEET_DIR / 'soma' / 'logs' / 'calx_runner.log'


def load_rules() -> list[dict]:
    if not RULES_FILE.exists():
        return []
    try:
        return json.loads(RULES_FILE.read_text())
    except Exception:
        return []


def check_rule(rule: dict) -> dict:
    """Evaluate a single Calx rule. Returns {rule_id, title, status, detail}."""
    rule_id = rule.get('id', '?')
    title = rule.get('title', rule_id)
    action = rule.get('action', '')
    rule_type = rule.get('type', 'suggestion')

    # Skip disabled rules
    if not rule.get('enabled', True):
        return {'rule_id': rule_id, 'title': title, 'status': 'skip', 'detail': 'disabled'}

    # Dispatch based on action keywords
    result = _evaluate_action(action, rule)
    return {'rule_id': rule_id, 'title': title, **result}


def _evaluate_action(action: str, rule: dict) -> dict:
    """Try to evaluate an action string against system state."""
    action_lower = action.lower()

    # Check service is running
    if 'systemctl' in action_lower or any(svc in action_lower for svc in ('soma', 'axon', 'vigil', 'sentinel', 'synapse', 'myelin')):
        svc_name = _extract_service_name(action_lower)
        if svc_name:
            result = subprocess.run(
                ['systemctl', 'is-active', svc_name],
                capture_output=True, text=True
            )
            active = result.stdout.strip() == 'active'
            return {
                'status': 'pass' if active else 'fail',
                'detail': f"systemctl is-active {svc_name}: {result.stdout.strip()}"
            }

    # Check file exists
    if 'file' in action_lower or 'path' in action_lower or '/' in action:
        for part in action.split():
            if part.startswith('/opt/') or part.startswith('/var/') or part.startswith('/etc/'):
                exists = Path(part).exists()
                return {
                    'status': 'pass' if exists else 'fail',
                    'detail': f"{'exists' if exists else 'MISSING'}: {part}"
                }

    # Check Python import works
    if 'import' in action_lower:
        module = action.split('import')[-1].strip().split()[0].strip('"\'')
        if module:
            result = subprocess.run(
                ['python3', '-c', f'import {module}; print("ok")'],
                capture_output=True, text=True
            )
            ok = result.returncode == 0
            return {'status': 'pass' if ok else 'fail', 'detail': f"import {module}: {'ok' if ok else result.stderr[:80]}"}

    # Default: mark as informational (can't auto-check)
    return {'status': 'info', 'detail': 'action requires manual verification'}


def _extract_service_name(text: str) -> str:
    """Try to find a systemd service name in text."""
    import re
    # Look for patterns like "soma.service", "axon", etc.
    match = re.search(r'(soma|axon|vigil|sentinel|synapse|myelin|dura|vector|seer|wizard)(?:\.service)?', text)
    return match.group(1) if match else ''


def log_results(results: list[dict]):
    CALX_LOG.parent.mkdir(parents=True, exist_ok=True)
    with open(CALX_LOG, 'a') as f:
        f.write(json.dumps({
            'ts': datetime.now(timezone.utc).isoformat(),
            'results': results
        }) + '\n')


def main():
    import argparse
    parser = argparse.ArgumentParser(description='Calx system validation runner')
    parser.add_argument('--json', action='store_true', help='JSON output')
    parser.add_argument('--verbose', action='store_true', help='Show all rules including pass/skip')
    args = parser.parse_args()

    rules = load_rules()
    if not rules:
        if args.json:
            print(json.dumps({'ok': True, 'rules': 0, 'results': [], 'note': 'No Calx rules found'}))
        else:
            print('[calx-runner] No rules found. Accept Soma insights to populate rules.')
        return

    results = [check_rule(r) for r in rules]
    log_results(results)

    if args.json:
        fails = [r for r in results if r['status'] == 'fail']
        print(json.dumps({
            'ok': len(fails) == 0,
            'rules': len(rules),
            'pass': len([r for r in results if r['status'] == 'pass']),
            'fail': len(fails),
            'skip': len([r for r in results if r['status'] in ('skip', 'info')]),
            'results': results,
        }, indent=2))
        return

    print(f'[calx-runner] {len(rules)} rules checked — {datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M UTC")}')
    for r in results:
        if r['status'] == 'fail' or args.verbose:
            icon = {'pass': '✓', 'fail': '✗', 'skip': '~', 'info': 'ℹ'}.get(r['status'], '?')
            print(f"  {icon} [{r['rule_id']}] {r['title']}")
            if r['status'] == 'fail' or args.verbose:
                print(f"      {r['detail']}")

    fails = [r for r in results if r['status'] == 'fail']
    if fails:
        print(f'\n  ✗ {len(fails)} rule(s) failing')
    else:
        print(f'\n  ✓ All {len(rules)} rules pass (or are informational)')


if __name__ == '__main__':
    main()
