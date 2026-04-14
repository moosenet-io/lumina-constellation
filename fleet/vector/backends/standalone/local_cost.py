"""Standalone CostGate with hard cap per run."""

import json, os
from typing import Optional
from interfaces import CostGate


class LocalCostGate(CostGate):
    def __init__(self, max_per_run: float = 5.00, state_file: str = './vector-cost.json'):
        self.max_per_run = max_per_run
        self.state_file = state_file
        self._spent = self._load_spent()

    def _load_spent(self) -> float:
        try:
            return json.loads(open(self.state_file).read()).get('spent', 0.0)
        except Exception:
            return 0.0

    def _save_spent(self):
        with open(self.state_file, 'w') as f:
            json.dump({'spent': self._spent, 'max': self.max_per_run}, f)

    def check_budget(self, estimated_cost: float) -> bool:
        return (self._spent + estimated_cost) <= self.max_per_run

    def record_spend(self, amount: float, description: str = '') -> None:
        self._spent += amount
        self._save_spent()

    def get_remaining(self) -> float:
        return max(0.0, self.max_per_run - self._spent)

    def request_approval(self, reason: str, amount_needed: float) -> bool:
        print(f'\n[COST GATE] Budget exhausted. Spent: ${self._spent:.2f} / ${self.max_per_run:.2f}')
        print(f'Reason: {reason}. Additional needed: ${amount_needed:.2f}')
        try:
            response = input('Approve additional spend? [y/N] ')
            if response.lower() == 'y':
                self.max_per_run += amount_needed
                return True
        except Exception:
            pass
        return False
