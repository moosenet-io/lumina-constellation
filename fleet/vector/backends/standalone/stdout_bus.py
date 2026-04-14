"""Standalone MessageBus using stdout + log file."""

import json, sys, os
from datetime import datetime
from typing import Optional, List, Dict
from interfaces import MessageBus


class StdoutMessageBus(MessageBus):
    def __init__(self, log_file: str = './vector.log', interactive: bool = False):
        self.log_file = log_file
        self.interactive = interactive
        self._pending = []  # In-memory queue for escalations awaiting response

    def _log(self, entry: dict):
        line = json.dumps(entry)
        print(line, file=sys.stderr)
        try:
            with open(self.log_file, 'a') as f:
                f.write(line + '\n')
        except Exception:
            pass

    def send(self, to: str, msg_type: str, payload: Dict, priority: str = 'normal', correlation_id: str = '') -> Optional[str]:
        import uuid
        msg_id = str(uuid.uuid4())
        entry = {'id': msg_id, 'to': to, 'type': msg_type, 'priority': priority,
                 'payload': payload, 'ts': datetime.utcnow().isoformat() + 'Z'}
        self._log(entry)

        if msg_type == 'escalation' and priority in ('urgent', 'critical'):
            print(f'\n[VECTOR ESCALATION] {payload.get("reason", "Unknown")}', file=sys.stderr)
            if self.interactive:
                response = input('How should I proceed? ')
                self._pending.append({'type': 'response', 'content': response})
            else:
                escalation_file = self.log_file.replace('.log', '-escalation.json')
                with open(escalation_file, 'w') as f:
                    json.dump(entry, f, indent=2)
                print(f'[vector] Escalation written to {escalation_file}. Pausing.', file=sys.stderr)

        return msg_id

    def check(self) -> Dict:
        return {'pending': len(self._pending), 'by_priority': {'normal': len(self._pending)}}

    def read(self, limit: int = 5) -> List[Dict]:
        msgs = self._pending[:limit]
        self._pending = self._pending[limit:]
        return msgs

    def ack(self, message_ids: List[str]) -> bool:
        return True  # no-op in standalone
