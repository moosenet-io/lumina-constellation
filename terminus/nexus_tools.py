import os
import json
import psycopg2
import psycopg2.extras
from datetime import datetime

# ============================================================
# Nexus Tools — Inter-Agent Inbox (Lumina Nexus)
# MCP tools providing the 5-function Nexus API over Postgres.
# Backend: lumina_inbox database on postgres-host (YOUR_POSTGRES_IP).
# Registered in Terminus server.py on terminus-host.
# from_agent is enforced by Terminus context for non-lumina agents
# ============================================================

# Core system agents + partner agent names (Lumiere family and 'partner' generic).
# Add new partner names here during CT onboarding ceremony.
VALID_AGENTS = {'lumina', 'axon', 'vigil', 'sentinel', 'vector', 'seer', 'wizard', 'lumiere', 'lumos', 'lima', 'lumen', 'lux', 'partner'}
VALID_TYPES = {'work_order', 'status', 'escalation', 'result', 'heartbeat', 'notification'}
VALID_PRIORITIES = {'critical', 'urgent', 'normal', 'low'}
PRIORITY_TTL = {'critical': 72, 'urgent': 48, 'normal': 24, 'low': 12}


def _get_conn():
    """Open a psycopg2 connection using env vars set by fetch-mcp-secrets.sh."""
    return psycopg2.connect(
        host=os.environ.get('INBOX_DB_HOST', 'YOUR_POSTGRES_IP'),
        dbname='lumina_inbox',
        user=os.environ.get('INBOX_DB_USER', 'lumina_inbox_user'),
        password=os.environ.get('INBOX_DB_PASS', 'nexus_inbox_changeme'),
        connect_timeout=5,
    )


def register_nexus_tools(mcp):

    # ── nexus_send ────────────────────────────────────────────────────────────

    @mcp.tool()
    def nexus_send(
        from_agent: str,
        to_agent: str,
        message_type: str,
        payload: str,
        priority: str = 'normal',
        correlation_id: str = '',
        ttl_hours: int = 0,
    ) -> dict:
        """Send a message to an agent's Nexus inbox.
        from_agent: sender identity (lumina, axon, vigil, sentinel, vector, seer, wizard).
        to_agent: recipient agent ID or * for broadcast.
        message_type: work_order, status, escalation, result, heartbeat, notification.
        payload: JSON string with message body.
        priority: critical (72h TTL), urgent (48h), normal (24h default), low (12h).
        correlation_id: optional UUID linking related messages (e.g. work order + result).
        ttl_hours: override default TTL. 0 = use priority default."""

        # Multi-claw: override from_agent with Terminus context to prevent spoofing
        from server import get_agent_context
        terminus_agent_id = get_agent_context()
        # If from_agent doesn't match terminus context, use terminus context
        # (unless the caller is explicitly overriding — allow lumina to send as any agent)
        if terminus_agent_id != 'lumina' and from_agent != terminus_agent_id:
            from_agent = terminus_agent_id

        from_agent = from_agent.lower().strip()
        to_agent = to_agent.lower().strip()

        # Validate sender
        if from_agent not in VALID_AGENTS:
            return {'error': f'Unknown from_agent: {from_agent}. Valid: {sorted(VALID_AGENTS)}'}

        # Access control: no peer-to-peer — non-lumina agents can only send TO lumina
        if from_agent != 'lumina' and to_agent != 'lumina' and to_agent != '*':
            return {'error': f'Access denied: {from_agent} can only send to lumina, not to {to_agent}. No peer-to-peer messaging.'}

        # Validate recipient
        if to_agent != '*' and to_agent not in VALID_AGENTS:
            return {'error': f'Unknown to_agent: {to_agent}. Valid: {sorted(VALID_AGENTS)} or * for broadcast.'}

        if message_type not in VALID_TYPES:
            return {'error': f'Unknown message_type: {message_type}. Valid: {sorted(VALID_TYPES)}'}

        priority = priority.lower() if priority else 'normal'
        if priority not in VALID_PRIORITIES:
            priority = 'normal'

        # Parse payload
        try:
            payload_obj = json.loads(payload) if isinstance(payload, str) else payload
        except json.JSONDecodeError as e:
            return {'error': f'payload must be valid JSON: {e}'}

        # Determine TTL
        hours = ttl_hours if ttl_hours > 0 else PRIORITY_TTL.get(priority, 24)

        # Determine recipients (broadcast expands to all agents except sender)
        recipients = sorted(VALID_AGENTS - {from_agent}) if to_agent == '*' else [to_agent]

        try:
            conn = _get_conn()
            cur = conn.cursor()
            inserted = []
            for recipient in recipients:
                cur.execute(
                    """INSERT INTO inbox_messages
                       (from_agent, to_agent, message_type, priority, payload,
                        correlation_id, expires_at)
                       VALUES (%s, %s, %s, %s, %s, %s,
                               now() + (%s || ' hours')::interval)
                       RETURNING id, created_at""",
                    (from_agent, recipient, message_type, priority,
                     json.dumps(payload_obj),
                     correlation_id or None,
                     str(hours))
                )
                row = cur.fetchone()
                inserted.append({'message_id': str(row[0]), 'to_agent': recipient})
            conn.commit()
            conn.close()

            if len(inserted) == 1:
                return {
                    'message_id': inserted[0]['message_id'],
                    'status': 'delivered',
                    'to_agent': to_agent,
                    'priority': priority,
                    'ttl_hours': hours,
                    'timestamp': datetime.utcnow().isoformat() + 'Z',
                }
            return {
                'status': 'broadcast_delivered',
                'recipients': len(inserted),
                'messages': inserted,
                'priority': priority,
                'timestamp': datetime.utcnow().isoformat() + 'Z',
            }
        except Exception as e:
            return {'error': f'nexus_send failed: {e}'}

    # ── nexus_check ───────────────────────────────────────────────────────────

    @mcp.tool()
    def nexus_check(agent_id: str) -> dict:
        """Lightweight inbox count check — designed for 5-min heartbeat at near-zero cost.
        Returns pending message counts by priority and type without reading message bodies.
        agent_id: which inbox to check (lumina, axon, vigil, sentinel, vector, seer, wizard)."""

        agent_id = agent_id.lower().strip()
        if agent_id not in VALID_AGENTS:
            return {'error': f'Unknown agent_id: {agent_id}'}

        try:
            conn = _get_conn()
            cur = conn.cursor()

            # Total pending
            cur.execute(
                "SELECT COUNT(*) FROM inbox_messages WHERE to_agent=%s AND status='pending'",
                (agent_id,)
            )
            total = cur.fetchone()[0]

            # By priority
            cur.execute(
                """SELECT priority, COUNT(*) FROM inbox_messages
                   WHERE to_agent=%s AND status='pending'
                   GROUP BY priority""",
                (agent_id,)
            )
            by_priority = {p: 0 for p in VALID_PRIORITIES}
            for row in cur.fetchall():
                by_priority[row[0]] = row[1]

            # By type
            cur.execute(
                """SELECT message_type, COUNT(*) FROM inbox_messages
                   WHERE to_agent=%s AND status='pending'
                   GROUP BY message_type""",
                (agent_id,)
            )
            by_type = {row[0]: row[1] for row in cur.fetchall()}

            # Oldest unread
            cur.execute(
                """SELECT created_at FROM inbox_messages
                   WHERE to_agent=%s AND status='pending'
                   ORDER BY created_at ASC LIMIT 1""",
                (agent_id,)
            )
            oldest = cur.fetchone()
            conn.close()

            return {
                'agent': agent_id,
                'pending': total,
                'by_priority': by_priority,
                'by_type': by_type,
                'oldest_unread': oldest[0].isoformat() + 'Z' if oldest else None,
            }
        except Exception as e:
            return {'error': f'nexus_check failed: {e}'}

    # ── nexus_read ────────────────────────────────────────────────────────────

    @mcp.tool()
    def nexus_read(
        agent_id: str,
        type_filter: str = '',
        priority_filter: str = '',
        limit: int = 10,
        since: str = '',
    ) -> dict:
        """Read pending messages from an agent's inbox. Marks them as read.
        agent_id: inbox to read.
        type_filter: optional — only return this message_type.
        priority_filter: minimum priority level (urgent returns urgent+critical).
        limit: max messages to return (default 10).
        since: ISO8601 timestamp — only messages after this time."""

        agent_id = agent_id.lower().strip()
        if agent_id not in VALID_AGENTS:
            return {'error': f'Unknown agent_id: {agent_id}'}

        limit = min(max(1, limit), 50)

        priority_order = ['critical', 'urgent', 'normal', 'low']

        try:
            conn = _get_conn()
            cur = conn.cursor(cursor_factory=psycopg2.extras.RealDictCursor)

            conditions = ["to_agent=%s", "status='pending'"]
            params = [agent_id]

            if type_filter:
                conditions.append("message_type=%s")
                params.append(type_filter)

            if priority_filter and priority_filter in priority_order:
                idx = priority_order.index(priority_filter)
                allowed = priority_order[:idx + 1]
                conditions.append(f"priority = ANY(%s)")
                params.append(allowed)

            if since:
                conditions.append("created_at > %s::timestamptz")
                params.append(since)

            where = ' AND '.join(conditions)
            cur.execute(
                f"""SELECT id, from_agent, to_agent, message_type, priority,
                           payload, correlation_id, created_at, expires_at
                    FROM inbox_messages
                    WHERE {where}
                    ORDER BY
                        CASE priority
                            WHEN 'critical' THEN 1
                            WHEN 'urgent' THEN 2
                            WHEN 'normal' THEN 3
                            WHEN 'low' THEN 4
                        END,
                        created_at ASC
                    LIMIT %s""",
                params + [limit]
            )
            rows = cur.fetchall()

            messages = []
            ids_to_mark = []
            for row in rows:
                msg = dict(row)
                msg['id'] = str(msg['id'])
                msg['created_at'] = msg['created_at'].isoformat() + 'Z'
                msg['expires_at'] = msg['expires_at'].isoformat() + 'Z'
                messages.append(msg)
                ids_to_mark.append(msg['id'])

            # Mark as read
            if ids_to_mark:
                cur.execute(
                    """UPDATE inbox_messages SET status='read', read_at=now()
                       WHERE id = ANY(%s::uuid[])""",
                    (ids_to_mark,)
                )
            conn.commit()
            conn.close()

            return {
                'agent': agent_id,
                'count': len(messages),
                'messages': messages,
            }
        except Exception as e:
            return {'error': f'nexus_read failed: {e}'}

    # ── nexus_ack ─────────────────────────────────────────────────────────────

    @mcp.tool()
    def nexus_ack(message_ids: str) -> dict:
        """Mark messages as processed. Prevents re-delivery. Idempotent.
        message_ids: comma-separated UUIDs to acknowledge."""

        ids = [m.strip() for m in message_ids.split(',') if m.strip()]
        if not ids:
            return {'error': 'message_ids must be a comma-separated list of UUIDs'}

        try:
            conn = _get_conn()
            cur = conn.cursor()
            cur.execute(
                """UPDATE inbox_messages
                   SET status='processed', processed_at=now()
                   WHERE id = ANY(%s::uuid[])
                   AND status IN ('pending', 'read')""",
                (ids,)
            )
            acked = cur.rowcount
            conn.commit()
            conn.close()
            return {
                'acked': acked,
                'requested': len(ids),
                'timestamp': datetime.utcnow().isoformat() + 'Z',
            }
        except Exception as e:
            return {'error': f'nexus_ack failed: {e}'}

    # ── nexus_history ─────────────────────────────────────────────────────────

    @mcp.tool()
    def nexus_history(
        agent_id: str = '',
        correlation_id: str = '',
        hours_back: int = 24,
        limit: int = 50,
    ) -> dict:
        """Query processed/expired messages for audit and debugging.
        agent_id: filter by agent (default: all — lumina only; others see own messages).
        correlation_id: find all messages related to a work order UUID.
        hours_back: look back N hours (default 24, max 168).
        limit: max results (default 50).
        Access control: non-lumina callers can only view their own message history."""

        hours_back = min(max(1, hours_back), 168)
        limit = min(max(1, limit), 200)

        try:
            conn = _get_conn()
            cur = conn.cursor(cursor_factory=psycopg2.extras.RealDictCursor)

            conditions = ["created_at > now() - (%s || ' hours')::interval"]
            params = [str(hours_back)]

            if agent_id:
                agent_id = agent_id.lower().strip()
                if agent_id not in VALID_AGENTS:
                    return {'error': f'Unknown agent_id: {agent_id}'}
                conditions.append("(from_agent=%s OR to_agent=%s)")
                params.extend([agent_id, agent_id])

            if correlation_id:
                conditions.append("correlation_id=%s")
                params.append(correlation_id)

            where = ' AND '.join(conditions)
            cur.execute(
                f"""SELECT id, from_agent, to_agent, message_type, priority,
                           payload, correlation_id, created_at, status,
                           read_at, processed_at
                    FROM inbox_messages
                    WHERE {where}
                    ORDER BY created_at DESC
                    LIMIT %s""",
                params + [limit]
            )
            rows = cur.fetchall()
            conn.close()

            messages = []
            for row in rows:
                msg = dict(row)
                msg['id'] = str(msg['id'])
                msg['created_at'] = msg['created_at'].isoformat() + 'Z'
                if msg['read_at']:
                    msg['read_at'] = msg['read_at'].isoformat() + 'Z'
                if msg['processed_at']:
                    msg['processed_at'] = msg['processed_at'].isoformat() + 'Z'
                messages.append(msg)

            return {
                'count': len(messages),
                'hours_back': hours_back,
                'messages': messages,
            }
        except Exception as e:
            return {'error': f'nexus_history failed: {e}'}
