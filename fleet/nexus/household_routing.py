#!/usr/bin/env python3
"""
household_routing.py — Nexus household message routing layer.

Provides send/receive helpers for household-type messages routed through
the Nexus inbox (PostgreSQL primary, SQLite fallback).

Event types: grocery_update, calendar_event, chore_reminder,
             finance_alert, shopping_list_update

Usage:
    from nexus.household_routing import send_household, get_household_inbox, household_sync
"""

import os
import sys
import json
import uuid
import logging
from datetime import datetime, timezone
from typing import Optional

log = logging.getLogger('household_routing')

# ── Allowed event types ───────────────────────────────────────────────────────

HOUSEHOLD_EVENT_TYPES = {
    'grocery_update',
    'calendar_event',
    'chore_reminder',
    'finance_alert',
    'shopping_list_update',
}

# ── DB config (from axon .env) ────────────────────────────────────────────────

INBOX_DB_HOST = os.environ.get('INBOX_DB_HOST', 'YOUR_POSTGRES_IP')
INBOX_DB_USER = os.environ.get('INBOX_DB_USER', 'lumina_inbox_user')
INBOX_DB_PASS = os.environ.get('INBOX_DB_PASS', '')
INBOX_DB_NAME = os.environ.get('INBOX_DB_NAME', 'lumina_inbox')

# ── Backend selection ─────────────────────────────────────────────────────────

_PG_AVAILABLE = False
_psycopg2 = None

try:
    import psycopg2
    import psycopg2.extras
    _psycopg2 = psycopg2
    _PG_AVAILABLE = True
except ImportError:
    log.warning('psycopg2 not available — falling back to SQLite Nexus')

# SQLite fallback — import functions directly
_sqlite_nexus = None

def _load_sqlite_fallback():
    global _sqlite_nexus
    if _sqlite_nexus is not None:
        return _sqlite_nexus
    # Attempt import from sibling module in the nexus directory
    nexus_dir = os.path.dirname(os.path.abspath(__file__))
    if nexus_dir not in sys.path:
        sys.path.insert(0, nexus_dir)
    try:
        import nexus_sqlite as _mod
        _sqlite_nexus = _mod
        log.info('SQLite Nexus fallback loaded from %s', nexus_dir)
    except ImportError:
        # Try the known fleet path
        fleet_nexus = '/opt/lumina-fleet/nexus'
        if fleet_nexus not in sys.path:
            sys.path.insert(0, fleet_nexus)
        try:
            import nexus_sqlite as _mod
            _sqlite_nexus = _mod
            log.info('SQLite Nexus fallback loaded from %s', fleet_nexus)
        except ImportError:
            log.error('nexus_sqlite.py not found — no fallback available')
            _sqlite_nexus = None
    return _sqlite_nexus


def _get_pg_conn():
    """Open a PostgreSQL connection using env-sourced credentials."""
    if not _PG_AVAILABLE:
        return None
    try:
        conn = _psycopg2.connect(
            host=INBOX_DB_HOST,
            user=INBOX_DB_USER,
            password=INBOX_DB_PASS,
            dbname=INBOX_DB_NAME,
            connect_timeout=5,
        )
        return conn
    except Exception as exc:
        log.warning('PostgreSQL unavailable (%s) — using SQLite fallback', exc)
        return None


# ── Core helpers ──────────────────────────────────────────────────────────────

def send_household(
    from_agent: str,
    to_agent: str,
    event_type: str,
    payload: dict,
    priority: str = 'normal',
    correlation_id: Optional[str] = None,
) -> dict:
    """
    Send a household-typed Nexus message from one agent to another.

    Sets message_type='household' and attaches event_type into the payload
    envelope so receivers can filter by event.

    Args:
        from_agent:     Sender agent name (e.g. 'lumina', 'partner').
        to_agent:       Recipient agent name.
        event_type:     One of HOUSEHOLD_EVENT_TYPES.
        payload:        Arbitrary dict of event data.
        priority:       'critical'|'urgent'|'normal'|'low'  (default 'normal').
        correlation_id: Optional thread ID for reply tracking.

    Returns:
        dict with 'id' and 'status'.
    """
    if event_type not in HOUSEHOLD_EVENT_TYPES:
        raise ValueError(
            f"Unknown event_type '{event_type}'. "
            f"Valid types: {sorted(HOUSEHOLD_EVENT_TYPES)}"
        )

    msg_id = str(uuid.uuid4())
    envelope = {
        'event_type': event_type,
        'data': payload,
        'sent_at': datetime.now(timezone.utc).isoformat(),
    }
    payload_json = json.dumps(envelope)

    conn = _get_pg_conn()
    if conn is not None:
        try:
            with conn:
                with conn.cursor() as cur:
                    cur.execute(
                        """
                        INSERT INTO inbox_messages
                            (id, from_agent, to_agent, message_type, payload,
                             priority, status, correlation_id)
                        VALUES (%s, %s, %s, 'household', %s, %s, 'pending', %s)
                        """,
                        (msg_id, from_agent, to_agent, payload_json,
                         priority, correlation_id),
                    )
            log.info('household send [pg] %s → %s [%s] id=%s', from_agent, to_agent, event_type, msg_id)
            return {'id': msg_id, 'status': 'sent', 'backend': 'postgres'}
        except Exception as exc:
            log.warning('PostgreSQL insert failed (%s) — falling back to SQLite', exc)
        finally:
            conn.close()

    # SQLite fallback
    sqlite = _load_sqlite_fallback()
    if sqlite is None:
        return {'id': msg_id, 'status': 'error', 'error': 'No Nexus backend available'}

    result = sqlite.nexus_send(
        from_agent=from_agent,
        to_agent=to_agent,
        message_type='household',
        payload=envelope,
        priority=priority,
        correlation_id=correlation_id,
    )
    result['backend'] = 'sqlite'
    log.info('household send [sqlite] %s → %s [%s] id=%s', from_agent, to_agent, event_type, result.get('id', msg_id))
    return result


def get_household_inbox(agent_id: str, limit: int = 20) -> list:
    """
    Read pending household messages for an agent, ordered by priority then age.

    Marks returned messages as 'read' (standard Nexus behaviour).

    Args:
        agent_id: The agent whose inbox to query (e.g. 'lumina', 'partner').
        limit:    Maximum number of messages to return (default 20).

    Returns:
        List of message dicts. Each includes parsed 'payload' with
        'event_type' and 'data' keys.
    """
    conn = _get_pg_conn()
    if conn is not None:
        try:
            with conn:
                with conn.cursor(_psycopg2.extras.RealDictCursor) as cur:
                    cur.execute(
                        """
                        SELECT id, from_agent, to_agent, message_type,
                               payload, priority, status, correlation_id,
                               created_at, updated_at
                        FROM inbox_messages
                        WHERE to_agent = %s
                          AND status   = 'pending'
                          AND message_type = 'household'
                        ORDER BY
                            CASE priority
                                WHEN 'critical' THEN 1
                                WHEN 'urgent'   THEN 2
                                WHEN 'normal'   THEN 3
                                ELSE 4
                            END,
                            created_at ASC
                        LIMIT %s
                        """,
                        (agent_id, limit),
                    )
                    rows = cur.fetchall()

                if rows:
                    ids = [r['id'] for r in rows]
                    with conn.cursor() as upd:
                        upd.execute(
                            "UPDATE inbox_messages SET status='read', updated_at=NOW() "
                            f"WHERE id = ANY(%s::uuid[])",
                            (ids,),
                        )

            messages = []
            for row in rows:
                msg = dict(row)
                if isinstance(msg.get('payload'), str):
                    try:
                        msg['payload'] = json.loads(msg['payload'])
                    except Exception:
                        pass
                if isinstance(msg.get('created_at'), datetime):
                    msg['created_at'] = msg['created_at'].isoformat()
                if isinstance(msg.get('updated_at'), datetime):
                    msg['updated_at'] = msg['updated_at'].isoformat()
                messages.append(msg)

            log.info('household inbox [pg] agent=%s returned=%d', agent_id, len(messages))
            return messages
        except Exception as exc:
            log.warning('PostgreSQL read failed (%s) — falling back to SQLite', exc)
        finally:
            conn.close()

    # SQLite fallback
    sqlite = _load_sqlite_fallback()
    if sqlite is None:
        log.error('No Nexus backend available for get_household_inbox')
        return []

    messages = sqlite.nexus_read(agent_id=agent_id, type_filter='household', limit=limit)
    log.info('household inbox [sqlite] agent=%s returned=%d', agent_id, len(messages))
    return messages


def household_sync(event_type: str, payload: dict, priority: str = 'normal') -> dict:
    """
    Broadcast a household event to both 'lumina' and 'partner' agents.

    Sends the same event to each agent in the household constellation.
    Uses a shared correlation_id so replies can be threaded.

    Args:
        event_type: One of HOUSEHOLD_EVENT_TYPES.
        payload:    Event data dict.
        priority:   Message priority (default 'normal').

    Returns:
        dict with 'correlation_id', 'results' list, and 'recipients' count.
    """
    from household_config import HOUSEHOLD_AGENTS  # local import to avoid circular

    correlation_id = str(uuid.uuid4())
    results = []

    for agent in HOUSEHOLD_AGENTS:
        result = send_household(
            from_agent='system',
            to_agent=agent,
            event_type=event_type,
            payload=payload,
            priority=priority,
            correlation_id=correlation_id,
        )
        results.append({'agent': agent, **result})

    log.info(
        'household_sync [%s] → %d agents (correlation_id=%s)',
        event_type, len(results), correlation_id,
    )
    return {
        'correlation_id': correlation_id,
        'event_type': event_type,
        'recipients': len(results),
        'results': results,
    }


# ── CLI for quick testing ─────────────────────────────────────────────────────

if __name__ == '__main__':
    import argparse

    logging.basicConfig(level=logging.INFO, format='%(levelname)s %(message)s')

    parser = argparse.ArgumentParser(description='Household Nexus routing CLI')
    sub = parser.add_subparsers(dest='cmd')

    p = sub.add_parser('send', help='Send a household event')
    p.add_argument('from_agent')
    p.add_argument('to_agent')
    p.add_argument('event_type', choices=sorted(HOUSEHOLD_EVENT_TYPES))
    p.add_argument('--payload', default='{}')
    p.add_argument('--priority', default='normal')

    p = sub.add_parser('inbox', help='Read household inbox for an agent')
    p.add_argument('agent_id')
    p.add_argument('--limit', type=int, default=10)

    p = sub.add_parser('sync', help='Broadcast event to all household agents')
    p.add_argument('event_type', choices=sorted(HOUSEHOLD_EVENT_TYPES))
    p.add_argument('--payload', default='{}')
    p.add_argument('--priority', default='normal')

    args = parser.parse_args()

    if args.cmd == 'send':
        print(json.dumps(
            send_household(args.from_agent, args.to_agent, args.event_type,
                           json.loads(args.payload), args.priority),
            indent=2
        ))
    elif args.cmd == 'inbox':
        print(json.dumps(get_household_inbox(args.agent_id, args.limit), indent=2, default=str))
    elif args.cmd == 'sync':
        print(json.dumps(
            household_sync(args.event_type, json.loads(args.payload), args.priority),
            indent=2
        ))
    else:
        parser.print_help()
