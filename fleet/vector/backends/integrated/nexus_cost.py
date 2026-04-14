"""
Vector Nexus CostGate — tracks spend, checks budget, requests approval via Nexus.
Falls back to local JSON file if Nexus DB unavailable.
"""
import os
import json
import logging
import time
from pathlib import Path
from datetime import datetime, date

log = logging.getLogger('vector.cost.nexus')

INBOX_DB_HOST = os.environ.get('INBOX_DB_HOST', '')
INBOX_DB_USER = os.environ.get('INBOX_DB_USER', 'lumina_inbox')
INBOX_DB_PASS = os.environ.get('INBOX_DB_PASS', '')


class NexusCostGate:
    """
    CostGate backed by Nexus PostgreSQL for cross-session spend tracking.
    Falls back to local JSON file if DB unavailable.
    """

    def __init__(self, max_cost_per_run: float = 5.00, budget_key: str = 'vector/daily_budget'):
        self.max_cost_per_run = max_cost_per_run
        self.budget_key = budget_key
        self._spent_this_run = 0.0
        self._db = None
        self._local_file = Path('./vector-cost.json')
        self._try_connect()
        self._daily_start = date.today().isoformat()
        self._daily_spent = self._load_daily_spent()

    def _try_connect(self):
        if not INBOX_DB_HOST:
            log.info('NexusCostGate: no DB configured, using local fallback')
            return
        try:
            import psycopg2
            self._db = psycopg2.connect(
                host=INBOX_DB_HOST, dbname='lumina_inbox',
                user=INBOX_DB_USER, password=INBOX_DB_PASS,
                connect_timeout=5
            )
            log.info('NexusCostGate connected to Nexus DB')
        except Exception as e:
            log.warning(f'NexusCostGate DB unavailable: {e}')

    def _load_daily_spent(self) -> float:
        """Load today's spend from DB or local file."""
        today = date.today().isoformat()
        if self._db:
            try:
                cur = self._db.cursor()
                cur.execute("""
                    SELECT COALESCE(SUM((payload->>'amount')::float), 0)
                    FROM inbox_messages
                    WHERE from_agent = 'vector'
                      AND message_type = 'cost_record'
                      AND created_at::date = CURRENT_DATE
                """)
                return float(cur.fetchone()[0])
            except Exception as e:
                log.warning(f'DB spend query failed: {e}')
        # Local fallback
        if self._local_file.exists():
            try:
                data = json.loads(self._local_file.read_text())
                if data.get('date') == today:
                    return float(data.get('daily_spent', 0))
            except Exception:
                pass
        return 0.0

    def check_budget(self, estimated_cost: float = 0.05) -> bool:
        """Return True if we have budget remaining."""
        return (self._spent_this_run + estimated_cost) <= self.max_cost_per_run

    def get_remaining(self) -> float:
        return max(0.0, self.max_cost_per_run - self._spent_this_run)

    def record_spend(self, amount: float):
        """Record a spend event."""
        self._spent_this_run += amount
        self._daily_spent += amount

        if self._db:
            try:
                import psycopg2.extras
                cur = self._db.cursor()
                cur.execute("""
                    INSERT INTO inbox_messages (from_agent, to_agent, message_type, payload, priority, status)
                    VALUES ('vector', 'lumina', 'cost_record', %s, 'low', 'processed')
                """, (json.dumps({'amount': amount, 'run_total': self._spent_this_run, 'daily_total': self._daily_spent}),))
                self._db.commit()
            except Exception as e:
                log.warning(f'Cost record DB write failed: {e}')
                self._save_local()
        else:
            self._save_local()

    def _save_local(self):
        try:
            self._local_file.write_text(json.dumps({
                'date': date.today().isoformat(),
                'run_spent': self._spent_this_run,
                'daily_spent': self._daily_spent,
                'max_cost_per_run': self.max_cost_per_run,
                'updated_at': datetime.utcnow().isoformat()
            }, indent=2))
        except Exception as e:
            log.warning(f'Local cost save failed: {e}')

    def request_approval(self, reason: str, additional_budget: float) -> bool:
        """
        Request additional budget via Nexus. Non-blocking — returns False immediately.
        Lumina will re-send the work order with higher budget if approved.
        """
        if self._db:
            try:
                cur = self._db.cursor()
                cur.execute("""
                    INSERT INTO inbox_messages (from_agent, to_agent, message_type, payload, priority, status)
                    VALUES ('vector', 'lumina', 'escalation', %s, 'urgent', 'pending')
                """, (json.dumps({
                    'reason': 'budget_exceeded',
                    'detail': reason,
                    'spent': self._spent_this_run,
                    'additional_requested': additional_budget,
                    'current_limit': self.max_cost_per_run
                }),))
                self._db.commit()
                log.info(f'Budget approval request sent to Lumina via Nexus')
            except Exception as e:
                log.warning(f'Could not send approval request: {e}')
        return False  # Always return False — Lumina must re-trigger with approval
