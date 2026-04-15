import json
import os
import uuid

# ============================================================
# Axon Tools — Work Queue Agent
# MCP tools for Lumina to submit work orders to Axon via Nexus
# and monitor their execution status.
# Axon runs on fleet-host /opt/lumina-fleet/axon/axon.py.
# Communication via Nexus inbox (Postgres on postgres-host).
# ============================================================

# Axon work order payload schema:
# {
#   "op": "plane_op" | "gitea_op" | "maintenance",
#   "action": str,     # e.g. "bulk_create_issues", "create_repos"
#   "params": dict,    # operation-specific parameters
#   "description": str # used as PX work item title
# }


def register_axon_tools(mcp):

    @mcp.tool()
    def axon_submit(
        op: str,
        action: str,
        params: str,
        description: str,
        priority: str = 'normal',
    ) -> dict:
        """Submit a work order to Axon via Nexus inbox.
        Axon will pick it up on next poll (within 60s) and execute it.

        op: plane_op, gitea_op, or maintenance
        action: specific operation (e.g. bulk_create_issues, create_repos)
        params: JSON string with operation parameters
        description: human-readable description (becomes PX work item title)
        priority: normal (default), urgent, critical, low

        Returns: {message_id, correlation_id, status} — use message_id to track result.

        Example — create labels:
          axon_submit(op=plane_op, action=bulk_create_labels,
                      params={"project_id":"xxx","labels":[{"name":"foo","color":"#red"}]},
                      description="Create sprint labels in LM project")"""

        try:
            payload_obj = json.loads(params) if isinstance(params, str) else params
        except json.JSONDecodeError as e:
            return {'error': f'params must be valid JSON: {e}'}

        correlation_id = str(uuid.uuid4())

        work_order = {
            'op': op,
            'action': action,
            'params': payload_obj,
            'description': description,
        }

        try:
            import psycopg2
            import psycopg2.extras
            conn = psycopg2.connect(
                host=os.environ.get('INBOX_DB_HOST', 'YOUR_POSTGRES_IP'),
                dbname='lumina_inbox',
                user=os.environ.get('INBOX_DB_USER', 'lumina_inbox_user'),
                password=os.environ.get('INBOX_DB_PASS', ''),
                connect_timeout=5,
            )
            cur = conn.cursor()
            ttl = {'critical': 72, 'urgent': 48, 'normal': 24, 'low': 12}.get(priority, 24)
            cur.execute(
                """INSERT INTO inbox_messages
                   (from_agent, to_agent, message_type, priority, payload, correlation_id, expires_at)
                   VALUES ('lumina', 'axon', 'work_order', %s, %s, %s,
                           now() + (%s || ' hours')::interval)
                   RETURNING id, created_at""",
                (priority, json.dumps(work_order), correlation_id, str(ttl))
            )
            row = cur.fetchone()
            conn.commit()
            conn.close()
            return {
                'message_id': str(row[0]),
                'correlation_id': correlation_id,
                'status': 'queued',
                'op': op,
                'action': action,
                'priority': priority,
                'description': description,
                'note': 'Axon will pick this up within 60 seconds.',
            }
        except Exception as e:
            return {'error': f'axon_submit failed: {e}'}

    @mcp.tool()
    def axon_status(correlation_id: str) -> dict:
        """Check the status of a submitted work order by correlation_id.
        Queries Nexus message history to find the original work_order and any result/escalation.
        Returns the current state and result payload if available.

        correlation_id: the UUID returned by axon_submit."""

        try:
            import psycopg2
            import psycopg2.extras
            conn = psycopg2.connect(
                host=os.environ.get('INBOX_DB_HOST', 'YOUR_POSTGRES_IP'),
                dbname='lumina_inbox',
                user=os.environ.get('INBOX_DB_USER', 'lumina_inbox_user'),
                password=os.environ.get('INBOX_DB_PASS', ''),
                connect_timeout=5,
            )
            cur = conn.cursor(cursor_factory=psycopg2.extras.RealDictCursor)
            cur.execute(
                """SELECT id, from_agent, to_agent, message_type, priority,
                          payload, status, created_at, processed_at
                   FROM inbox_messages
                   WHERE correlation_id=%s
                   ORDER BY created_at ASC""",
                (correlation_id,)
            )
            rows = cur.fetchall()
            conn.close()

            if not rows:
                return {'status': 'not_found', 'correlation_id': correlation_id}

            messages = []
            for row in rows:
                m = dict(row)
                m['id'] = str(m['id'])
                m['created_at'] = m['created_at'].isoformat() + 'Z'
                if m['processed_at']:
                    m['processed_at'] = m['processed_at'].isoformat() + 'Z'
                messages.append(m)

            # Determine overall status
            types = {m['message_type'] for m in messages}

            if 'result' in types:
                result_msg = next(m for m in messages if m['message_type'] == 'result')
                return {
                    'status': 'completed',
                    'correlation_id': correlation_id,
                    'result': result_msg['payload'],
                    'messages': messages,
                }
            elif 'escalation' in types:
                esc_msg = next(m for m in messages if m['message_type'] == 'escalation')
                return {
                    'status': 'escalated',
                    'correlation_id': correlation_id,
                    'escalation': esc_msg['payload'],
                    'messages': messages,
                }
            elif any(m['status'] in ('pending', 'read') for m in messages):
                return {
                    'status': 'in_progress',
                    'correlation_id': correlation_id,
                    'messages': messages,
                }
            else:
                return {
                    'status': 'unknown',
                    'correlation_id': correlation_id,
                    'messages': messages,
                }
        except Exception as e:
            return {'error': f'axon_status failed: {e}'}

    @mcp.tool()
    def axon_list(hours_back: int = 24, limit: int = 20) -> dict:
        """List recent work orders submitted to Axon.
        Returns a summary table of recent work orders with their status.

        hours_back: look back N hours (default 24)
        limit: max results (default 20)"""

        try:
            import psycopg2
            import psycopg2.extras
            conn = psycopg2.connect(
                host=os.environ.get('INBOX_DB_HOST', 'YOUR_POSTGRES_IP'),
                dbname='lumina_inbox',
                user=os.environ.get('INBOX_DB_USER', 'lumina_inbox_user'),
                password=os.environ.get('INBOX_DB_PASS', ''),
                connect_timeout=5,
            )
            cur = conn.cursor(cursor_factory=psycopg2.extras.RealDictCursor)
            cur.execute(
                """SELECT id, correlation_id, priority, payload, status, created_at
                   FROM inbox_messages
                   WHERE to_agent='axon'
                   AND message_type='work_order'
                   AND created_at > now() - (%s || ' hours')::interval
                   ORDER BY created_at DESC
                   LIMIT %s""",
                (str(hours_back), limit)
            )
            rows = cur.fetchall()
            conn.close()

            orders = []
            for row in rows:
                m = dict(row)
                payload = m['payload'] if isinstance(m['payload'], dict) else json.loads(m['payload'])
                orders.append({
                    'id': str(m['id'])[:8],
                    'correlation_id': m['correlation_id'],
                    'op': payload.get('op', '?'),
                    'action': payload.get('action', '?'),
                    'description': payload.get('description', '')[:50],
                    'priority': m['priority'],
                    'status': m['status'],
                    'created_at': m['created_at'].isoformat() + 'Z',
                })

            return {'count': len(orders), 'hours_back': hours_back, 'work_orders': orders}
        except Exception as e:
            return {'error': f'axon_list failed: {e}'}

    @mcp.tool()
    def axon_cancel(correlation_id: str) -> dict:
        """Attempt to cancel a pending work order by marking it expired.
        Only works if Axon has not yet read the message (status=pending).
        If Axon is already executing, this will not stop it — use axon_status to check first.

        correlation_id: the UUID from axon_submit."""

        try:
            import psycopg2
            conn = psycopg2.connect(
                host=os.environ.get('INBOX_DB_HOST', 'YOUR_POSTGRES_IP'),
                dbname='lumina_inbox',
                user=os.environ.get('INBOX_DB_USER', 'lumina_inbox_user'),
                password=os.environ.get('INBOX_DB_PASS', ''),
                connect_timeout=5,
            )
            cur = conn.cursor()
            cur.execute(
                """UPDATE inbox_messages
                   SET status='expired', expires_at=now()
                   WHERE correlation_id=%s
                   AND to_agent='axon'
                   AND status='pending'""",
                (correlation_id,)
            )
            cancelled = cur.rowcount
            conn.commit()
            conn.close()

            if cancelled == 0:
                return {
                    'cancelled': 0,
                    'note': 'No pending messages found. Axon may already be executing. Use axon_status to check.',
                }
            return {'cancelled': cancelled, 'correlation_id': correlation_id}
        except Exception as e:
            return {'error': f'axon_cancel failed: {e}'}
