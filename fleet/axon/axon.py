#!/usr/bin/env python3
"""
Axon — Work Queue Agent
Polls Nexus inbox (direct Postgres), executes work orders, reports results.
Runs on CT310 as axon.service (systemd).

Work order payload schema:
  {"op": "plane_op"|"gitea_op"|"maintenance",
   "action": str,
   "params": dict,
   "description": str}

Nexus backend: lumina_inbox Postgres on CT300 (YOUR_POSTGRES_IP).
Direct psycopg2 connection — no MCP SSH bridge needed.
"""

import os
import sys
import json
import time
import uuid
import signal
import logging
import traceback
import urllib.request
import urllib.error
from datetime import datetime, timezone, timedelta

import psycopg2
import psycopg2.extras

# Engram integration — load memory module from same fleet dir
sys.path.insert(0, '/opt/lumina-fleet/engram')
try:
    import engram as _engram
    _ENGRAM_OK = True
except ImportError:
    _ENGRAM_OK = False

# Pulse — temporal awareness (SP.C4)
sys.path.insert(0, '/opt/lumina-fleet/shared')
try:
    import pulse as _pulse
    _PULSE_OK = True
except ImportError:
    _PULSE_OK = False

# ── Config ────────────────────────────────────────────────────────────────────

POLL_INTERVAL = int(os.environ.get('POLL_INTERVAL', '60'))  # seconds
MAX_RETRIES = 3
RETRY_BACKOFF = [5, 15, 30]  # seconds

PLANE_BASE = os.environ.get('PLANE_BASE_URL', 'http://YOUR_PLANE_IP')
PLANE_TOKEN = os.environ.get('PLANE_TOKEN_AXON', os.environ.get('PLANE_TOKEN_LUMINA', ''))
PLANE_WS = os.environ.get('PLANE_WORKSPACE_SLUG', 'moosenet')
PX_PROJECT_ID = os.environ.get('PX_PROJECT_ID', '')  # The Plexus project UUID

GITEA_BASE = os.environ.get('GITEA_URL', 'http://YOUR_GITEA_IP:3000')
GITEA_TOKEN = os.environ.get('GITEA_TOKEN', '')

INBOX_DB_HOST = os.environ.get('INBOX_DB_HOST', 'YOUR_POSTGRES_IP')
INBOX_DB_USER = os.environ.get('INBOX_DB_USER', 'lumina_inbox_user')
INBOX_DB_PASS = os.environ.get('INBOX_DB_PASS', 'nexus_inbox_changeme')
INBOX_DB_NAME = os.environ.get('INBOX_DB_NAME', 'lumina_inbox')

PRIORITY_TTL = {'critical': 72, 'urgent': 48, 'normal': 24, 'low': 12}

logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s [axon] %(levelname)s %(message)s',
    handlers=[logging.StreamHandler(sys.stdout)]
)
log = logging.getLogger('axon')

# Graceful shutdown
_running = True


def _handle_signal(sig, frame):
    global _running
    log.info('Signal %s received — shutting down after current work', sig)
    _running = False


signal.signal(signal.SIGTERM, _handle_signal)
signal.signal(signal.SIGINT, _handle_signal)


# ── Nexus direct Postgres ─────────────────────────────────────────────────────

def _db_conn():
    """Open a psycopg2 connection to the Nexus inbox database."""
    return psycopg2.connect(
        host=INBOX_DB_HOST,
        dbname=INBOX_DB_NAME,
        user=INBOX_DB_USER,
        password=INBOX_DB_PASS,
        connect_timeout=5,
        cursor_factory=psycopg2.extras.RealDictCursor,
    )


def nexus_check():
    """Return pending message counts by priority for axon."""
    try:
        with _db_conn() as conn:
            with conn.cursor() as cur:
                cur.execute("""
                    SELECT priority, COUNT(*) AS cnt
                    FROM inbox_messages
                    WHERE to_agent = 'axon'
                      AND status = 'pending'
                      AND expires_at > NOW()
                    GROUP BY priority
                """)
                rows = cur.fetchall()
        by_priority = {r['priority']: r['cnt'] for r in rows}
        total = sum(by_priority.values())
        return {'pending': total, 'by_priority': by_priority}
    except Exception as e:
        return {'error': str(e)}


def nexus_read(limit=5):
    """Read up to limit pending messages for axon, ordered by priority+age."""
    PRIORITY_ORDER = {'critical': 0, 'urgent': 1, 'normal': 2, 'low': 3}
    try:
        with _db_conn() as conn:
            with conn.cursor() as cur:
                cur.execute("""
                    SELECT id, from_agent, to_agent, message_type, priority,
                           payload, correlation_id, created_at, expires_at, status
                    FROM inbox_messages
                    WHERE to_agent = 'axon'
                      AND status = 'pending'
                      AND expires_at > NOW()
                    ORDER BY
                        CASE priority
                            WHEN 'critical' THEN 0
                            WHEN 'urgent'   THEN 1
                            WHEN 'normal'   THEN 2
                            WHEN 'low'      THEN 3
                            ELSE 4
                        END,
                        created_at ASC
                    LIMIT %s
                """, (limit,))
                rows = cur.fetchall()
                # Mark as read
                ids = [str(r['id']) for r in rows]
                if ids:
                    cur.execute("""
                        UPDATE inbox_messages
                        SET status = 'read', read_at = NOW()
                        WHERE id = ANY(%s::uuid[])
                    """, (ids,))
                conn.commit()
        messages = []
        for r in rows:
            msg = dict(r)
            msg['id'] = str(msg['id'])
            msg['created_at'] = msg['created_at'].isoformat() if msg['created_at'] else None
            msg['expires_at'] = msg['expires_at'].isoformat() if msg['expires_at'] else None
            # Ensure payload is dict
            if isinstance(msg['payload'], str):
                msg['payload'] = json.loads(msg['payload'])
            messages.append(msg)
        return {'messages': messages, 'count': len(messages)}
    except Exception as e:
        return {'error': str(e)}


def nexus_ack(message_ids: list):
    """Mark messages as processed."""
    try:
        with _db_conn() as conn:
            with conn.cursor() as cur:
                cur.execute("""
                    UPDATE inbox_messages
                    SET status = 'processed', processed_at = NOW()
                    WHERE id = ANY(%s::uuid[])
                """, (message_ids,))
                acked = cur.rowcount
                conn.commit()
        return {'acked': acked, 'message_ids': message_ids}
    except Exception as e:
        return {'error': str(e)}


def nexus_send(msg_type: str, payload: dict, priority='normal', correlation_id='',
               to_agent='lumina'):
    """Send a message from axon to another agent (default: lumina)."""
    try:
        ttl_hours = PRIORITY_TTL.get(priority, 24)
        msg_id = str(uuid.uuid4())
        with _db_conn() as conn:
            with conn.cursor() as cur:
                cur.execute("""
                    INSERT INTO inbox_messages
                        (id, from_agent, to_agent, message_type, priority,
                         payload, correlation_id, expires_at)
                    VALUES (%s, 'axon', %s, %s, %s, %s, %s, NOW() + %s * INTERVAL '1 hour')
                """, (
                    msg_id, to_agent, msg_type, priority,
                    json.dumps(payload),
                    correlation_id or None,
                    ttl_hours,
                ))
                conn.commit()
        return {'message_id': msg_id, 'status': 'sent'}
    except Exception as e:
        return {'error': str(e)}


# ── Plane API ─────────────────────────────────────────────────────────────────

def _plane(method, path, data=None):
    url = f'{PLANE_BASE}{path}'
    req = urllib.request.Request(
        url,
        data=json.dumps(data).encode() if data else None,
        headers={'X-API-Key': PLANE_TOKEN, 'Content-Type': 'application/json'},
        method=method
    )
    with urllib.request.urlopen(req, timeout=15) as r:
        return json.load(r)


def plane_get_states(project_id):
    d = _plane('GET', f'/api/v1/workspaces/{PLANE_WS}/projects/{project_id}/states/')
    return {s['name']: s['id'] for s in d.get('results', d if isinstance(d, list) else [])}


def plane_create_issue(project_id, name, description='', state_id=None, priority='medium'):
    data = {'name': name, 'description_html': f'<p>{description}</p>', 'priority': priority}
    if state_id:
        data['state'] = state_id
    return _plane('POST', f'/api/v1/workspaces/{PLANE_WS}/projects/{project_id}/issues/', data)


def plane_update_issue(project_id, issue_id, **kwargs):
    return _plane('PATCH',
                  f'/api/v1/workspaces/{PLANE_WS}/projects/{project_id}/issues/{issue_id}/',
                  kwargs)


def plane_create_label(project_id, name, color='#6366F1'):
    return _plane('POST', f'/api/v1/workspaces/{PLANE_WS}/projects/{project_id}/labels/',
                  {'name': name, 'color': color})


# ── Gitea API ─────────────────────────────────────────────────────────────────

def _gitea(method, path, data=None):
    req = urllib.request.Request(
        f'{GITEA_BASE}/api/v1{path}',
        data=json.dumps(data).encode() if data else None,
        headers={'Authorization': f'token {GITEA_TOKEN}', 'Content-Type': 'application/json'},
        method=method
    )
    with urllib.request.urlopen(req, timeout=15) as r:
        return json.load(r)


def gitea_create_repo(name, description='', private=True):
    return _gitea('POST', '/orgs/moosenet/repos', {
        'name': name, 'description': description,
        'private': private, 'auto_init': True, 'default_branch': 'main'
    })


# ── Engram helpers ────────────────────────────────────────────────────────────

def engram_query_context(description: str) -> list:
    """Query Engram for relevant context before executing a work order."""
    if not _ENGRAM_OK:
        return []
    try:
        results = _engram.query(description, top_k=3)
        if results:
            log.info('[engram] context for "%s": %d results', description[:60], len(results))
        return results
    except Exception as e:
        log.warning('[engram] query failed: %s', e)
        return []


def engram_log(action: str, outcome: str, context: str = ''):
    """Log work completion to Engram activity journal."""
    if not _ENGRAM_OK:
        return
    try:
        _engram.journal('axon', action, outcome, context)
    except Exception as e:
        log.warning('[engram] journal failed: %s', e)


# ── Work order handlers ───────────────────────────────────────────────────────

def handle_plane_op(params: dict, plexus_issue_id: str) -> dict:
    action = params.get('action', '')
    results = []

    if action == 'bulk_create_issues':
        project_id = params['project_id']
        issues = params.get('issues', [])
        states = plane_get_states(project_id)
        for item in issues:
            state_id = states.get(item.get('state', 'Todo'))
            r = plane_create_issue(project_id, item['name'],
                                   item.get('description', ''),
                                   state_id, item.get('priority', 'medium'))
            results.append({'seq': r.get('sequence_id'), 'name': item['name']})
            time.sleep(0.25)
        return {'created': len(results), 'issues': results}

    elif action == 'bulk_create_labels':
        project_id = params['project_id']
        labels = params.get('labels', [])
        for lbl in labels:
            r = plane_create_label(project_id, lbl['name'], lbl.get('color', '#6366F1'))
            results.append(r.get('name'))
            time.sleep(0.2)
        return {'created': len(results), 'labels': results}

    elif action == 'update_issue_state':
        project_id = params['project_id']
        issue_id = params['issue_id']
        new_state = params['state']
        states = plane_get_states(project_id)
        state_id = states.get(new_state)
        if not state_id:
            return {'error': f'Unknown state: {new_state}'}
        r = plane_update_issue(project_id, issue_id, state=state_id)
        return {'updated': r.get('sequence_id'), 'state': new_state}

    else:
        return {'error': f'Unknown plane_op action: {action}'}


def handle_gitea_op(params: dict) -> dict:
    action = params.get('action', '')
    results = []

    if action == 'create_repos':
        for repo in params.get('repos', []):
            r = gitea_create_repo(repo['name'], repo.get('description', ''))
            results.append(r.get('full_name', repo['name']))
            time.sleep(0.3)
        return {'created': len(results), 'repos': results}

    else:
        return {'error': f'Unknown gitea_op action: {action}'}


def handle_work_order(message: dict) -> dict:
    payload = message.get('payload', {})
    if isinstance(payload, str):
        payload = json.loads(payload)

    op = payload.get('op')
    params = payload.get('params', {})
    # Merge top-level action into params so handlers can find it
    if 'action' not in params and 'action' in payload:
        params = dict(params, action=payload['action'])
    description = payload.get('description', 'Work order')

    # Query Engram for relevant context before executing
    context_results = engram_query_context(description)
    if context_results:
        params['_engram_context'] = context_results

    # Create a tracking issue in The Plexus
    plexus_issue_id = None
    if PX_PROJECT_ID and PLANE_TOKEN:
        try:
            states = plane_get_states(PX_PROJECT_ID)
            r = plane_create_issue(PX_PROJECT_ID, description,
                                   f'Work order from Lumina. op={op}',
                                   states.get('In Progress'), 'medium')
            plexus_issue_id = r.get('id')
        except Exception as e:
            log.warning('Could not create PX tracking issue: %s', e)

    # Execute
    try:
        if op == 'plane_op':
            result = handle_plane_op(params, plexus_issue_id)
        elif op == 'gitea_op':
            result = handle_gitea_op(params)
        else:
            result = {'error': f'Unknown op: {op}. Valid: plane_op, gitea_op, maintenance'}

        # Mark PX issue Done/Failed
        if plexus_issue_id and PX_PROJECT_ID and PLANE_TOKEN:
            try:
                states = plane_get_states(PX_PROJECT_ID)
                new_state = 'Cancelled' if 'error' in result else 'Done'
                plane_update_issue(PX_PROJECT_ID, plexus_issue_id, state=states.get(new_state))
            except Exception:
                pass

        result['op'] = op
        result['description'] = description

        # Log to Engram journal
        outcome_str = 'success' if 'error' not in result else f"error: {result['error'][:100]}"
        engram_log(f'{op}: {description[:100]}', outcome_str, f'msg_id={message.get("id","?")}')

        return result

    except Exception as e:
        log.error('Work order execution failed: %s\n%s', e, traceback.format_exc())
        if plexus_issue_id and PX_PROJECT_ID and PLANE_TOKEN:
            try:
                states = plane_get_states(PX_PROJECT_ID)
                plane_update_issue(PX_PROJECT_ID, plexus_issue_id, state=states.get('Cancelled'))
            except Exception:
                pass
        engram_log(f'{op}: {description[:100]}', f'exception: {str(e)[:100]}', 'execution failed')
        return {'error': str(e), 'traceback': traceback.format_exc()[:500]}


# ── Main polling loop ─────────────────────────────────────────────────────────

def main():
    log.info('Axon starting — polling Nexus inbox every %ds', POLL_INTERVAL)
    log.info('DB: %s@%s/%s', INBOX_DB_USER, INBOX_DB_HOST, INBOX_DB_NAME)

    # Test DB connectivity at startup
    try:
        check = nexus_check()
        if 'error' in check:
            log.error('DB connectivity check failed: %s', check['error'])
            sys.exit(1)
        log.info('DB connected. Pending for axon: %d', check.get('pending', 0))
    except Exception as e:
        log.error('DB startup check failed: %s', e)
        sys.exit(1)

    # Send startup heartbeat
    r = nexus_send('heartbeat', {
        'status': 'online',
        'agent': 'axon',
        'started_at': datetime.now(timezone.utc).isoformat(),
        'poll_interval': POLL_INTERVAL,
    }, priority='low')
    log.info('Startup heartbeat: %s', r.get('message_id', r.get('error', 'unknown')))

    while _running:
        try:
            # Lightweight check — mark poll time for Sentinel duration tracking
            if _PULSE_OK:
                _pulse.mark('axon_last_poll')
            check = nexus_check()
            if 'error' in check:
                log.warning('nexus_check failed: %s', check['error'])
                time.sleep(POLL_INTERVAL)
                continue

            pending = check.get('pending', 0)
            if pending == 0:
                log.debug('Inbox empty, sleeping %ds', POLL_INTERVAL)
                time.sleep(POLL_INTERVAL)
                continue

            log.info('Inbox: %d pending (critical=%d urgent=%d normal=%d low=%d)',
                     pending,
                     check['by_priority'].get('critical', 0),
                     check['by_priority'].get('urgent', 0),
                     check['by_priority'].get('normal', 0),
                     check['by_priority'].get('low', 0))

            # Read messages
            read_result = nexus_read(limit=5)
            if 'error' in read_result:
                log.error('nexus_read failed: %s', read_result['error'])
                time.sleep(POLL_INTERVAL)
                continue

            messages = read_result.get('messages', [])
            ack_ids = []

            for msg in messages:
                msg_id = msg['id']
                msg_type = msg.get('message_type', '')
                correlation_id = msg.get('correlation_id', '') or ''
                log.info('Processing message %s type=%s priority=%s',
                         msg_id[:8], msg_type, msg.get('priority'))

                result = None
                for attempt in range(MAX_RETRIES):
                    try:
                        if msg_type == 'work_order':
                            result = handle_work_order(msg)
                        elif msg_type == 'heartbeat':
                            result = {'status': 'ack', 'message': 'heartbeat received'}
                        elif msg_type == 'notification':
                            log.info('Notification: %s', msg.get('payload', {}))
                            result = {'status': 'noted'}
                        else:
                            result = {'error': f'Unknown message_type: {msg_type}'}
                        break  # success
                    except Exception as e:
                        if attempt < MAX_RETRIES - 1:
                            wait = RETRY_BACKOFF[attempt]
                            log.warning('Attempt %d failed, retrying in %ds: %s',
                                        attempt + 1, wait, e)
                            time.sleep(wait)
                        else:
                            result = {'error': str(e), 'attempts': attempt + 1}

                # Report result back to Lumina
                if msg_type == 'work_order':
                    if result and 'error' in result:
                        nexus_send('escalation', {
                            'original_message_id': msg_id,
                            'error': result['error'],
                            'op': result.get('op', 'unknown'),
                        }, priority='urgent', correlation_id=correlation_id)
                        log.warning('Escalated work order %s: %s', msg_id[:8], result['error'])
                    else:
                        nexus_send('result', {
                            'original_message_id': msg_id,
                            'result': result,
                        }, priority='normal', correlation_id=correlation_id)
                        log.info('Completed work order %s: %s', msg_id[:8],
                                 json.dumps(result)[:100])

                ack_ids.append(msg_id)

            # Ack all processed messages
            if ack_ids:
                ack_result = nexus_ack(ack_ids)
                log.info('Acked %d messages', ack_result.get('acked', 0))

        except KeyboardInterrupt:
            break
        except Exception as e:
            log.error('Main loop error: %s', e)
            time.sleep(POLL_INTERVAL)

    log.info('Axon shutdown complete')


if __name__ == '__main__':
    main()
