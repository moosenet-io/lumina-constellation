#!/usr/bin/env python3
"""
alert_rules.py — Sentinel alert threshold evaluation.
Reads health check results and sends Matrix notifications via Nexus.
Debounces: only re-alerts after 30 min silence per check.
"""
import json
import os
import time
from pathlib import Path
from datetime import datetime, timezone

HEALTH_JSON = Path('/opt/lumina-fleet/sentinel/output/health.json')
ALERT_STATE = Path('/opt/lumina-fleet/sentinel/output/alert_state.json')
DEBOUNCE_MINUTES = 30

# Alert thresholds
RULES = [
    ('axon_db',    'critical', 'CRITICAL: Axon DB connection failed — work queue processing stopped'),
    ('ollama_gpu', 'critical', 'CRITICAL: local GPU offline — inference fallback active (higher cost)'),
    ('llm_cost',   'critical', 'CRITICAL: LLM daily cost circuit-break threshold hit — killing non-essential processes'),
    ('llm_cost',   'warn',     'WARN: LLM daily cost approaching $2/day threshold'),
    ('ironclaw',   'critical', 'CRITICAL: IronClaw (Lumina) is down — no agent responses'),
    ('postgres',   'critical', 'CRITICAL: Postgres/Nexus DB unreachable — all inbox operations failing'),
    ('matrix',     'critical', 'CRITICAL: Matrix bridge down — no Matrix communication'),
    ('terminus',   'critical', 'CRITICAL: Terminus MCP hub down — IronClaw has no tools'),
    ('docker',     'critical', 'CRITICAL: Critical Docker containers down on fleet-host'),
    ('axon_db',    'warn',     'WARN: Axon running but no recent DB poll activity'),
    ('nexus_age',  'warn',     'WARN: Nexus has many unacked messages or very old pending messages'),
    ('soma',       'critical', 'WARN: Soma admin panel not responding'),
]


def load_state() -> dict:
    try:
        if ALERT_STATE.exists():
            return json.loads(ALERT_STATE.read_text())
    except Exception:
        pass
    return {}


def save_state(state: dict):
    try:
        ALERT_STATE.parent.mkdir(parents=True, exist_ok=True)
        ALERT_STATE.write_text(json.dumps(state, indent=2))
    except Exception as e:
        print(f'[alert] State save failed: {e}')


def should_alert(check_name: str, status: str, state: dict) -> bool:
    """Return True if alert should fire (new condition or debounce expired)."""
    key = f'{check_name}:{status}'
    last_alert = state.get(key, 0)
    now = time.time()
    elapsed_min = (now - last_alert) / 60
    return elapsed_min >= DEBOUNCE_MINUTES


def send_nexus_alert(message: str, priority: str = 'urgent'):
    import os, psycopg2
    host = os.environ.get('INBOX_DB_HOST', 'YOUR_POSTGRES_IP')
    user = os.environ.get('INBOX_DB_USER', 'lumina_inbox_user')
    password = os.environ.get('INBOX_DB_PASS', '')
    try:
        conn = psycopg2.connect(host=host, dbname='lumina_inbox', user=user,
                                password=password, connect_timeout=3)
        payload = json.dumps({'alert': message, 'source': 'sentinel-alerts',
                              'timestamp': datetime.now(timezone.utc).isoformat()})
        conn.cursor().execute(
            "INSERT INTO inbox_messages (from_agent,to_agent,message_type,priority,payload,status) "
            "VALUES ('sentinel','lumina','escalation',%s,%s,'pending')",
            (priority, payload)
        )
        conn.commit()
        conn.close()
        print(f'[alert] Sent: {message[:80]}')
    except Exception as e:
        print(f'[alert] Nexus send failed: {e}')


def evaluate_rules(health: dict) -> list[str]:
    """Evaluate all alert rules against health data. Returns list of fired alert messages."""
    checks = health.get('checks', {})
    state = load_state()
    fired = []
    now = time.time()

    for check_name, trigger_status, message in RULES:
        check = checks.get(check_name, {})
        current_status = check.get('status', 'ok')

        if current_status == trigger_status:
            if should_alert(check_name, trigger_status, state):
                # Add check value to message
                value = check.get('value', '')
                detail = check.get('message', '')
                full_msg = f'{message} | {detail}' if detail else message

                send_nexus_alert(full_msg,
                                 priority='critical' if trigger_status == 'critical' else 'urgent')
                state[f'{check_name}:{trigger_status}'] = now
                fired.append(full_msg)
        else:
            # Clear the debounce if condition resolved
            key = f'{check_name}:{trigger_status}'
            if key in state and current_status == 'ok':
                del state[key]
                # Send recovery notification
                send_nexus_alert(f'RESOLVED: {check_name} is now OK (was {trigger_status})', 'normal')

    save_state(state)
    return fired


def main():
    import os
    env_file = Path('/opt/lumina-fleet/axon/.env')
    if env_file.exists():
        for line in env_file.read_text().splitlines():
            if '=' in line and not line.startswith('#'):
                k, v = line.split('=', 1)
                os.environ.setdefault(k.strip(), v.strip().strip('"').strip("'"))

    if not HEALTH_JSON.exists():
        print('[alert] No health.json found — run health_checks.py first')
        return

    health = json.loads(HEALTH_JSON.read_text())
    fired = evaluate_rules(health)
    print(f'[alert] Evaluated {len(RULES)} rules, {len(fired)} alerts fired')


if __name__ == '__main__':
    main()
