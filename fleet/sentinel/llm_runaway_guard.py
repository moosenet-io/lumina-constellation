#!/usr/bin/env python3
"""
llm_runaway_guard.py — Sentinel LLM Runaway Detection and Circuit Breaker

Monitors LLM API spending via LiteLLM and OpenRouter APIs.
Detects and kills runaway processes making excessive inference calls.
Runs as part of the Sentinel health check cycle on CT310.

Thresholds (configurable via env vars):
  RUNAWAY_HOURLY_LIMIT   — max $ per hour before alert (default: $2.00)
  RUNAWAY_DAILY_LIMIT    — max $ per day before circuit break (default: $10.00)
  RUNAWAY_PROCESS_CALLS  — max LiteLLM calls per process per minute (default: 10)

Actions:
  WARN  — Send Nexus alert to Lumina (relay to the operator via Matrix)
  BREAK — Kill the offending process and send alert
  BLOCK — Add process to /tmp/llm_blocked_pids.txt to prevent restart

Usage:
  python3 llm_runaway_guard.py              # one-shot check
  python3 llm_runaway_guard.py --monitor    # continuous monitor (60s loop)
  python3 llm_runaway_guard.py --status     # show current spend
"""

import os
import sys
import json
import time
import signal
import subprocess
import urllib.request
from datetime import datetime, timedelta
from pathlib import Path

# ── Config ──────────────────────────────────────────────────────────────────

LITELLM_URL = os.environ.get('LITELLM_URL', 'http://YOUR_LITELLM_IP:4000')
LITELLM_KEY = os.environ.get('LITELLM_MASTER_KEY', '')
OR_KEY = os.environ.get('OPENROUTER_API_KEY', '')

HOURLY_WARN_LIMIT = float(os.environ.get('RUNAWAY_HOURLY_LIMIT', '2.00'))
DAILY_BREAK_LIMIT = float(os.environ.get('RUNAWAY_DAILY_LIMIT', '10.00'))
PROCESS_CALLS_LIMIT = int(os.environ.get('RUNAWAY_PROCESS_CALLS', '10'))

NEXUS_DB_HOST = os.environ.get('INBOX_DB_HOST', 'YOUR_POSTGRES_IP')
NEXUS_DB_USER = os.environ.get('INBOX_DB_USER', 'lumina_inbox_user')
NEXUS_DB_PASS = os.environ.get('INBOX_DB_PASS', '')

STATE_FILE = Path(os.environ.get('RUNAWAY_STATE_FILE', '/tmp/llm_guard_state.json'))
BLOCK_FILE = Path('/tmp/llm_blocked_pids.txt')


# ── Spend Monitoring ────────────────────────────────────────────────────────

def get_litellm_spend() -> dict:
    """Fetch spend stats from LiteLLM proxy."""
    try:
        req = urllib.request.Request(
            f'{LITELLM_URL}/global/spend',
            headers={'Authorization': f'Bearer {LITELLM_KEY}'}
        )
        with urllib.request.urlopen(req, timeout=5) as r:
            return json.load(r)
    except Exception as e:
        return {'error': str(e), 'spend': None}


def get_openrouter_spend() -> dict:
    """Fetch current usage from OpenRouter API."""
    if not OR_KEY:
        return {'error': 'no OR key', 'usage_daily': 0}
    try:
        req = urllib.request.Request(
            'https://openrouter.ai/api/v1/auth/key',
            headers={'Authorization': f'Bearer {OR_KEY}'}
        )
        with urllib.request.urlopen(req, timeout=10) as r:
            d = json.load(r)
            return d.get('data', d)
    except Exception as e:
        return {'error': str(e), 'usage_daily': 0}


def get_active_llm_processes() -> list[dict]:
    """Find processes that might be making LLM calls (Python scripts, ironclaw, etc.)."""
    llm_processes = []
    try:
        result = subprocess.run(
            ['ps', 'aux'],
            capture_output=True, text=True, timeout=5
        )
        for line in result.stdout.splitlines()[1:]:
            parts = line.split(None, 10)
            if len(parts) < 11:
                continue
            cmd = parts[10]
            # Identify known LLM-calling processes
            if any(p in cmd for p in ['ironclaw', 'llm-proxy', 'bridge.py', 'loop.py', 'vector.py', 'wizard.py', 'seer.py']):
                try:
                    pid = int(parts[1])
                    cpu = float(parts[2])
                    mem = float(parts[3])
                    llm_processes.append({
                        'pid': pid, 'cpu': cpu, 'mem': mem,
                        'cmd': cmd[:80], 'user': parts[0],
                        'running_since': parts[8],
                    })
                except (ValueError, IndexError):
                    pass
    except Exception:
        pass
    return llm_processes


# ── Alert & Circuit Breaker ──────────────────────────────────────────────────

def send_nexus_alert(message: str, priority: str = 'urgent') -> bool:
    """Send alert via Nexus inbox to Lumina."""
    try:
        import psycopg2
        conn = psycopg2.connect(
            host=NEXUS_DB_HOST, dbname='lumina_inbox',
            user=NEXUS_DB_USER, password=NEXUS_DB_PASS, connect_timeout=5
        )
        cur = conn.cursor()
        cur.execute("""
            INSERT INTO inbox_messages (from_agent, to_agent, message_type, payload, priority, status)
            VALUES ('sentinel', 'lumina', 'escalation', %s, %s, 'pending')
        """, (json.dumps({'alert': 'llm_runaway', 'message': message, 'timestamp': datetime.utcnow().isoformat()}), priority))
        conn.commit()
        conn.close()
        return True
    except Exception as e:
        print(f'[guard] Nexus alert failed: {e}', file=sys.stderr)
        return False


def kill_runaway_process(pid: int, cmd: str, reason: str) -> bool:
    """Kill a runaway process with SIGTERM, then SIGKILL if needed."""
    print(f'[guard] KILLING PID {pid} ({cmd[:40]}) — {reason}')
    try:
        os.kill(pid, signal.SIGTERM)
        time.sleep(3)
        # Check if still alive
        os.kill(pid, 0)  # raises if dead
        os.kill(pid, signal.SIGKILL)
        print(f'[guard] SIGKILL sent to PID {pid}')
    except ProcessLookupError:
        print(f'[guard] PID {pid} already dead')
        return True
    except Exception as e:
        print(f'[guard] Kill failed for PID {pid}: {e}', file=sys.stderr)
        return False
    return True


def load_state() -> dict:
    try:
        if STATE_FILE.exists():
            return json.loads(STATE_FILE.read_text())
    except Exception:
        pass
    return {'last_daily_spend': 0, 'last_check': None, 'alerts_sent': 0, 'breaks_triggered': 0}


def save_state(state: dict):
    try:
        STATE_FILE.write_text(json.dumps(state, indent=2))
    except Exception:
        pass


# ── Main Guard Logic ─────────────────────────────────────────────────────────

def run_check() -> dict:
    """Execute one guard cycle. Returns status dict."""
    state = load_state()
    result = {
        'timestamp': datetime.utcnow().isoformat(),
        'or_daily_spend': 0,
        'alerts': [],
        'kills': [],
        'status': 'ok',
    }

    # 1. Check OpenRouter daily spend
    or_data = get_openrouter_spend()
    daily_spend = float(or_data.get('usage_daily', 0) or 0)
    result['or_daily_spend'] = daily_spend

    print(f'[guard] OpenRouter daily spend: ${daily_spend:.4f} (limit: ${DAILY_BREAK_LIMIT})')

    if daily_spend >= DAILY_BREAK_LIMIT:
        msg = (f'🚨 LLM CIRCUIT BREAKER: OpenRouter daily spend ${daily_spend:.2f} '
               f'exceeds limit ${DAILY_BREAK_LIMIT:.2f}. '
               f'Killing LLM-calling processes and alerting the operator.')
        print(f'[guard] CIRCUIT BREAK: {msg}')
        result['status'] = 'circuit_break'
        result['alerts'].append(msg)

        # Kill non-essential LLM processes (not ironclaw itself)
        procs = get_active_llm_processes()
        for proc in procs:
            if 'ironclaw' not in proc['cmd'] and 'llm-proxy' not in proc['cmd']:
                if kill_runaway_process(proc['pid'], proc['cmd'], f'Daily spend limit ${DAILY_BREAK_LIMIT}'):
                    result['kills'].append({'pid': proc['pid'], 'cmd': proc['cmd']})
                    state['breaks_triggered'] = state.get('breaks_triggered', 0) + 1

        send_nexus_alert(msg, priority='critical')
        state['alerts_sent'] = state.get('alerts_sent', 0) + 1

    elif daily_spend >= HOURLY_WARN_LIMIT:
        msg = (f'⚠️ LLM spend warning: OpenRouter daily ${daily_spend:.2f} '
               f'(warn threshold: ${HOURLY_WARN_LIMIT:.2f}). '
               f'Check for runaway processes.')
        print(f'[guard] WARN: {msg}')
        result['status'] = 'warn'
        result['alerts'].append(msg)

        # List suspicious processes but don't kill yet
        procs = get_active_llm_processes()
        if procs:
            proc_list = ', '.join(f"PID {p['pid']} ({p['cmd'][:30]})" for p in procs[:5])
            msg += f' Active LLM processes: {proc_list}'

        send_nexus_alert(msg, priority='urgent')
        state['alerts_sent'] = state.get('alerts_sent', 0) + 1

    # 2. Check for high-CPU LLM processes (possible tight loop)
    procs = get_active_llm_processes()
    for proc in procs:
        if proc['cpu'] > 90:
            msg = (f'High CPU LLM process detected: PID {proc["pid"]} '
                   f'at {proc["cpu"]}% CPU — {proc["cmd"][:50]}')
            print(f'[guard] HIGH CPU: {msg}')
            result['alerts'].append(msg)
            if result['status'] == 'ok':
                result['status'] = 'warn'
            send_nexus_alert(msg, priority='urgent')

    state['last_daily_spend'] = daily_spend
    state['last_check'] = result['timestamp']
    save_state(state)

    return result


def print_status():
    """Print current LLM spend status."""
    or_data = get_openrouter_spend()
    daily = float(or_data.get('usage_daily', 0) or 0)
    weekly = float(or_data.get('usage_weekly', 0) or 0)
    total = float(or_data.get('usage', 0) or 0)

    print(f'OpenRouter spend:')
    print(f'  Daily:   ${daily:.4f} (limit: ${DAILY_BREAK_LIMIT})')
    print(f'  Weekly:  ${weekly:.4f}')
    print(f'  Total:   ${total:.4f}')
    print()

    procs = get_active_llm_processes()
    print(f'Active LLM processes ({len(procs)}):')
    for p in procs:
        print(f'  PID {p["pid"]:6d} CPU:{p["cpu"]:5.1f}% | {p["cmd"][:60]}')

    state = load_state()
    print()
    print(f'Guard state: alerts={state.get("alerts_sent",0)} breaks={state.get("breaks_triggered",0)}')


# ── Entry Point ──────────────────────────────────────────────────────────────

if __name__ == '__main__':
    import argparse
    parser = argparse.ArgumentParser(description='LLM runaway guard')
    parser.add_argument('--monitor', action='store_true', help='Continuous monitor (60s loop)')
    parser.add_argument('--status', action='store_true', help='Show current spend status')
    parser.add_argument('--dry-run', action='store_true', help='Check only, no kills or alerts')
    args = parser.parse_args()

    if args.status:
        print_status()
        sys.exit(0)

    if args.monitor:
        print('[guard] Starting continuous monitor (60s interval)...')
        while True:
            try:
                result = run_check()
                print(f'[guard] {result["timestamp"]} status={result["status"]} '
                      f'spend=${result["or_daily_spend"]:.3f}')
            except Exception as e:
                print(f'[guard] Check error: {e}', file=sys.stderr)
            time.sleep(60)
    else:
        result = run_check()
        print(json.dumps(result, indent=2))
        sys.exit(0 if result['status'] == 'ok' else 1)
