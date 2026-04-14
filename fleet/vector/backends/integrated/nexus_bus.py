"""Integrated MessageBus using Nexus (direct psycopg2 to CT300)."""
import os, json, psycopg2, psycopg2.extras, uuid
from datetime import datetime
from backends.interfaces import MessageBus

class NexusMessageBus(MessageBus):
    def __init__(self, config=None):
        self.db_host = os.environ.get('INBOX_DB_HOST', 'YOUR_POSTGRES_IP')
        self.db_user = os.environ.get('INBOX_DB_USER', 'lumina_inbox_user')
        self.db_pass = os.environ.get('INBOX_DB_PASS', '')
        self.agent_id = os.environ.get('VECTOR_AGENT_ID', 'vector')

    def _db(self):
        return psycopg2.connect(host=self.db_host, dbname='lumina_inbox',
            user=self.db_user, password=self.db_pass, connect_timeout=5)

    def send(self, to, msg_type, payload, priority='normal', correlation_id=''):
        ttl = {'critical':72,'urgent':48,'normal':24,'low':12}.get(priority, 24)
        conn = self._db()
        cur = conn.cursor()
        cur.execute("""INSERT INTO inbox_messages
            (from_agent, to_agent, message_type, priority, payload, correlation_id, expires_at)
            VALUES (%s, %s, %s, %s, %s, %s, now()+(%s||' hours')::interval) RETURNING id""",
            (self.agent_id, to, msg_type, priority, json.dumps(payload), correlation_id or None, str(ttl)))
        msg_id = str(cur.fetchone()[0])
        conn.commit(); conn.close()
        return msg_id

    def check(self):
        conn = self._db()
        cur = conn.cursor()
        cur.execute("SELECT COUNT(*) FROM inbox_messages WHERE to_agent=%s AND status='pending'", (self.agent_id,))
        total = cur.fetchone()[0]
        cur.execute("SELECT priority, COUNT(*) FROM inbox_messages WHERE to_agent=%s AND status='pending' GROUP BY priority", (self.agent_id,))
        by_p = {'critical':0,'urgent':0,'normal':0,'low':0}
        for row in cur.fetchall(): by_p[row[0]] = row[1]
        conn.close()
        return {'pending': total, 'by_priority': by_p}

    def read(self, limit=5):
        conn = self._db()
        cur = conn.cursor(cursor_factory=psycopg2.extras.RealDictCursor)
        cur.execute("""SELECT id, from_agent, message_type, priority, payload, correlation_id, created_at
            FROM inbox_messages WHERE to_agent=%s AND status='pending'
            ORDER BY CASE priority WHEN 'critical' THEN 1 WHEN 'urgent' THEN 2 WHEN 'normal' THEN 3 ELSE 4 END, created_at
            LIMIT %s""", (self.agent_id, limit))
        rows = cur.fetchall()
        ids = [str(r['id']) for r in rows]
        if ids:
            cur.execute("UPDATE inbox_messages SET status='read', read_at=now() WHERE id=ANY(%s::uuid[])", (ids,))
        conn.commit(); conn.close()
        msgs = []
        for r in rows:
            m = dict(r); m['id'] = str(m['id']); m['created_at'] = m['created_at'].isoformat()+'Z'
            msgs.append(m)
        return msgs

    def ack(self, message_ids):
        if not message_ids: return True
        conn = self._db()
        cur = conn.cursor()
        cur.execute("UPDATE inbox_messages SET status='processed', processed_at=now() WHERE id=ANY(%s::uuid[]) AND status IN ('pending','read')", (message_ids,))
        conn.commit(); conn.close()
        return True
