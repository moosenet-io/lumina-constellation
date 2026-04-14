#!/usr/bin/env python3
"""
Inbox Monitor — replaces inbox-monitor IronClaw routine
Direct SQL on Postgres. Zero inference. Sends Matrix alert on critical messages.
CT310 /opt/lumina-fleet/nexus/inbox_monitor.py
"""
import os, sys, json, urllib.request, time
from datetime import datetime, timedelta
from pathlib import Path
import psycopg2

def _load_env():
    env_file = Path('/opt/lumina-fleet/axon/.env')
    if env_file.exists():
        for line in env_file.read_text().splitlines():
            if '=' in line and not line.startswith('#'):
                k, v = line.split('=', 1)
                os.environ.setdefault(k.strip(), v.strip())

MATRIX_HOMESERVER = os.environ.get('MATRIX_HOMESERVER', 'http://YOUR_MATRIX_IP:6167')
MATRIX_TOKEN = os.environ.get('MATRIX_BOT_TOKEN', '')
MATRIX_ROOM = os.environ.get('MATRIX_ROOM_ID', '')

def _send_matrix(message):
    """Send Matrix message via bridge bot HTTP API."""
    try:
        bridge_url = 'http://YOUR_MATRIX_IP:8080/send'
        data = json.dumps({'message': message, 'room': MATRIX_ROOM}).encode()
        req = urllib.request.Request(bridge_url, data=data,
            headers={'Content-Type': 'application/json'}, method='POST')
        with urllib.request.urlopen(req, timeout=5) as r:
            return r.status == 200
    except Exception as e:
        print(f'[inbox-monitor] Matrix send failed: {e}', file=sys.stderr)
        return False

def check_inbox():
    _load_env()

    db_host = os.environ.get('INBOX_DB_HOST', 'YOUR_POSTGRES_IP')
    db_user = os.environ.get('INBOX_DB_USER', 'lumina_inbox_user')
    db_pass = os.environ.get('INBOX_DB_PASS', '')

    try:
        conn = psycopg2.connect(host=db_host, dbname='lumina_inbox',
            user=db_user, password=db_pass, connect_timeout=3)
        cur = conn.cursor()

        # Count pending messages by priority
        cur.execute("""SELECT priority, COUNT(*), MIN(created_at)
            FROM inbox_messages
            WHERE to_agent='lumina' AND status='pending'
            GROUP BY priority""")
        rows = cur.fetchall()

        critical = next((r for r in rows if r[0]=='critical'), None)
        urgent = next((r for r in rows if r[0]=='urgent'), None)

        alerts_sent = 0

        if critical and critical[1] > 0:
            age = (datetime.utcnow() - critical[2].replace(tzinfo=None)).total_seconds() / 60
            msg = f'Lumina: {critical[1]} CRITICAL message(s) in inbox (oldest: {age:.0f}min ago). Check Matrix.'
            if _send_matrix(msg):
                alerts_sent += 1
            print(f'[inbox-monitor] CRITICAL: {critical[1]} messages, sent alert')

        elif urgent and urgent[1] > 0:
            age = (datetime.utcnow() - urgent[2].replace(tzinfo=None)).total_seconds() / 60
            if age > 10:  # only alert if stale > 10 min
                msg = f'Lumina: {urgent[1]} urgent message(s) in inbox (oldest: {age:.0f}min ago).'
                _send_matrix(msg)
                alerts_sent += 1
                print(f'[inbox-monitor] URGENT: {urgent[1]} messages, sent alert')

        total = sum(r[1] for r in rows)
        conn.close()
        print(f'[inbox-monitor] {total} pending messages | {alerts_sent} alerts sent')
        return total

    except Exception as e:
        print(f'[inbox-monitor] Error: {e}', file=sys.stderr)
        return -1

if __name__ == '__main__':
    check_inbox()
