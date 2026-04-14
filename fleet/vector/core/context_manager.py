#!/usr/bin/env python3
"""
context_manager.py — Context window prime zone management.
Tracks utilization, adapts prime_pct based on gate pass/fail history.
"""
import os
import logging
from typing import Optional

log = logging.getLogger('vector.context_manager')

MODEL_WINDOWS = {
    'claude-sonnet': 200_000,
    'claude-haiku': 200_000,
    'claude-opus': 200_000,
    'Lumina Fast': 32_768,
    'Lumina': 200_000,
    'gpt-4o': 128_000,
    'local-qwen2.5-7b': 32_768,
    'default': 100_000,
}


class ContextManager:
    """Manages context window utilization and prime zone adaptation."""

    def __init__(self, model: str = 'claude-sonnet', initial_prime_pct: int = 35,
                 state_backend=None):
        self.model = model
        self.context_window = MODEL_WINDOWS.get(model, MODEL_WINDOWS['default'])
        self.prime_pct = initial_prime_pct
        self.state = state_backend  # optional: for persistence

        self._consecutive_passes = 0
        self._iteration_count = 0

    def estimate_tokens(self, text: str) -> int:
        """Rough token estimate: ~4 chars per token."""
        return len(text) // 4

    def get_prime_limit(self) -> int:
        """Max tokens for the prime zone (task context)."""
        return int(self.context_window * self.prime_pct / 100)

    def compress_if_needed(self, content: str, label: str = '') -> str:
        """Truncate content to fit within prime zone."""
        limit = self.get_prime_limit()
        tokens = self.estimate_tokens(content)
        if tokens <= limit:
            return content
        # Truncate to limit (rough)
        char_limit = limit * 4
        log.info(f'ContextManager: compressing {label} from ~{tokens} to ~{limit} tokens')
        return content[:char_limit] + '\n[... compressed ...]'

    def record_gate_result(self, passed: bool, context_pct: float):
        """Record the result of a quality gate iteration."""
        self._iteration_count += 1

        if passed:
            self._consecutive_passes += 1
            # 5 consecutive passes → increase prime zone by 5%
            if self._consecutive_passes >= 5:
                old = self.prime_pct
                self.prime_pct = min(55, self.prime_pct + 5)
                self._consecutive_passes = 0
                if self.prime_pct != old:
                    log.info(f'ContextManager: prime_pct increased {old}% → {self.prime_pct}%')
        else:
            self._consecutive_passes = 0
            # Gate failure at high utilization → back off 10%
            if context_pct > 0.7:
                old = self.prime_pct
                self.prime_pct = max(25, self.prime_pct - 10)
                if self.prime_pct != old:
                    log.info(f'ContextManager: gate fail at {context_pct:.0%}, prime_pct {old}% → {self.prime_pct}%')

        # Every 50 iterations → re-evaluate at 35%
        if self._iteration_count % 50 == 0:
            self.prime_pct = 35
            self._consecutive_passes = 0
            log.info(f'ContextManager: 50-iter reset, prime_pct → 35%')

    def get_stats(self) -> dict:
        return {
            'model': self.model,
            'context_window': self.context_window,
            'prime_pct': self.prime_pct,
            'prime_limit_tokens': self.get_prime_limit(),
            'consecutive_passes': self._consecutive_passes,
            'iteration_count': self._iteration_count,
        }
