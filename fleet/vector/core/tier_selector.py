#!/usr/bin/env python3
"""
tier_selector.py — Per-chunk inference tier selection.
Caches available models from LiteLLM at startup, selects tier per task.
"""
import os
import json
import urllib.request
import logging
from typing import Optional

log = logging.getLogger('vector.tier_selector')

# Task type → model tier mapping
TIER_RULES = [
    # (keywords_list, tier)
    (['scaffold', 'boilerplate', 'test writing', 'format conversion', 'stub', 'template'], 'tier1'),
    (['refactor', 'design', 'debug', 'optimize', 'algorithm'], 'tier2'),
    (['security', 'architecture', 'critical', 'audit', 'vulnerability'], 'tier3'),
]


class TierSelector:
    """Selects the cheapest adequate model for each task type."""

    TIER_PREFERENCE = {
        'tier1': ['local-qwen2.5-7b', 'Lumina Fast', 'claude-haiku'],
        'tier2': ['claude-sonnet', 'Lumina'],
        'tier3': ['claude-sonnet', 'claude-opus'],
    }

    def __init__(self, litellm_url: str = None, litellm_key: str = None):
        self.litellm_url = litellm_url or os.environ.get('LITELLM_URL', 'http://YOUR_LITELLM_IP:4000')
        self.litellm_key = litellm_key or os.environ.get('LITELLM_MASTER_KEY', '')
        self._available_models: list[str] = []
        self._loaded = False

    def load_available_models(self) -> list[str]:
        """Fetch available models from LiteLLM at startup."""
        try:
            req = urllib.request.Request(
                f'{self.litellm_url}/v1/models',
                headers={'Authorization': f'Bearer {self.litellm_key}'}
            )
            with urllib.request.urlopen(req, timeout=5) as r:
                data = json.load(r)
                self._available_models = [m['id'] for m in data.get('data', [])]
                self._loaded = True
                log.info(f'TierSelector: {len(self._available_models)} models available')
                return self._available_models
        except Exception as e:
            log.warning(f'TierSelector: cannot fetch models ({e}), using defaults')
            self._available_models = ['claude-sonnet', 'Lumina Fast']
            self._loaded = True
            return self._available_models

    def select_model(self, task_name: str, task_description: str = '') -> tuple[str, str]:
        """Returns (model_id, tier) for the task. Loads models on first call."""
        if not self._loaded:
            self.load_available_models()

        tier = self._classify_tier(task_name, task_description)
        model = self._pick_model(tier)
        log.debug(f'TierSelector: task="{task_name[:40]}" tier={tier} model={model}')
        return model, tier

    def _classify_tier(self, task_name: str, task_description: str) -> str:
        text = (task_name + ' ' + task_description).lower()
        for keywords, tier in TIER_RULES:
            if any(kw in text for kw in keywords):
                return tier
        return 'tier2'  # default

    def _pick_model(self, tier: str) -> str:
        """Pick the first available model from tier preference list."""
        preferences = self.TIER_PREFERENCE.get(tier, self.TIER_PREFERENCE['tier2'])
        for model in preferences:
            if model in self._available_models or not self._available_models:
                return model
        return preferences[-1]  # fallback to last preference
