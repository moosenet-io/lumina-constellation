#!/usr/bin/env python3
"""
delegator.py — Execution delegation: reasoning vs scaffolding split.
Pure Python classification — no LLM cost for the decision itself.
"""
import os
from typing import Optional

# Keywords that indicate scaffolding tasks (cheap local model)
SCAFFOLD_KEYWORDS = {
    'scaffold', 'boilerplate', 'template', 'generate', 'create file',
    'write test', 'test file', 'format', 'convert', 'rename', 'move file',
    'add import', 'add comment', 'docstring', 'stub', 'placeholder',
    'lint fix', 'autofix', 'sort imports', 'fix formatting', 'add type hint',
}

# Keywords that need the primary model (Sonnet-class)
REASONING_KEYWORDS = {
    'refactor', 'design', 'debug', 'diagnose', 'investigate', 'analyze',
    'optimize', 'algorithm', 'logic error', 'race condition', 'deadlock',
    'memory leak', 'performance', 'architecture', 'pattern',
}

# Keywords that need best available (Opus-class)
BEST_KEYWORDS = {
    'security', 'vulnerability', 'exploit', 'audit', 'critical', 'production',
    'data loss', 'corruption', 'authentication', 'authorization',
}


def classify_task(task_name: str, task_description: str = '') -> str:
    """Classify a task into: 'scaffold', 'reasoning', or 'best'.
    Returns the tier name. Pure Python, zero LLM cost."""
    text = (task_name + ' ' + task_description).lower()

    # Check in order of priority: best > reasoning > scaffold
    if any(kw in text for kw in BEST_KEYWORDS):
        return 'best'
    if any(kw in text for kw in REASONING_KEYWORDS):
        return 'reasoning'
    if any(kw in text for kw in SCAFFOLD_KEYWORDS):
        return 'scaffold'
    # Default to reasoning for unknown tasks
    return 'reasoning'


class ExecutionDelegator:
    """Routes task execution to appropriate model tier."""

    def __init__(self, config: dict):
        """
        config: from vector.yaml 'delegation' section
          enabled: bool
          scaffold_model: str (e.g. 'Lumina Fast' or 'local-qwen2.5-7b')
          scaffold_endpoint: str (LiteLLM base URL)
          primary_model: str
          best_model: str
        """
        self.enabled = config.get('enabled', True)
        self.scaffold_model = config.get('scaffold_model', 'Lumina Fast')
        self.primary_model = config.get('primary_model', 'claude-sonnet')
        self.best_model = config.get('best_model', 'claude-sonnet')
        self.endpoint = config.get('scaffold_endpoint', os.environ.get('LITELLM_URL', 'http://YOUR_LITELLM_IP:4000'))
        self._tier_counts = {'scaffold': 0, 'reasoning': 0, 'best': 0}

    def select_model(self, task_name: str, task_description: str = '') -> tuple[str, str]:
        """Returns (model_name, tier) for the given task."""
        if not self.enabled:
            return self.primary_model, 'reasoning'

        tier = classify_task(task_name, task_description)
        self._tier_counts[tier] += 1

        model_map = {
            'scaffold': self.scaffold_model,
            'reasoning': self.primary_model,
            'best': self.best_model,
        }
        return model_map[tier], tier

    def stats(self) -> dict:
        """Return tier usage statistics."""
        total = sum(self._tier_counts.values())
        return {
            'total': total,
            'tiers': self._tier_counts,
            'pct_scaffold': round(self._tier_counts['scaffold'] / max(total, 1) * 100, 1),
        }
