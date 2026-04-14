#!/usr/bin/env python3
"""
guardrails.py — Two-scope guardrails system.
System guardrails: apply to all projects (tool names, infra paths).
Project guardrails: apply to specific project (domain terms, functions).
"""
import os
import re
import logging
from pathlib import Path
from typing import Optional

log = logging.getLogger('vector.guardrails')

# Tool/path patterns that indicate system-scope corrections
SYSTEM_SCOPE_PATTERNS = [
    r'\b(git|sed|awk|grep|npm|pip|cargo|docker|kubectl|terraform)\b',
    r'/(etc|usr|var|opt|sys|proc|dev)\b',
    r'\b(systemd|crontab|ssh|firewall|iptables)\b',
]

# Promotion threshold: N times same correction → system guardrail
AUTO_PROMOTE_THRESHOLD = 3


class GuardrailsManager:
    """Manages system and project-level guardrails."""

    def __init__(self, memory_store=None, project_name: str = ''):
        self.memory = memory_store
        self.project_name = project_name
        self._system_guardrails: list[str] = []
        self._project_guardrails: list[str] = []
        self._correction_counts: dict[str, int] = {}

    def load(self):
        """Load system and project guardrails from memory or local files."""
        # System guardrails
        if self.memory:
            try:
                system_results = self.memory.query('vector system guardrails rules', top_k=10)
                for r in system_results:
                    content = r if isinstance(r, str) else r.get('content', '')
                    if content and 'system-guardrail' in content.lower():
                        self._system_guardrails.append(content)
            except Exception as e:
                log.debug(f'Could not load system guardrails from memory: {e}')

        # Fallback: local file
        local_sys = Path('/opt/lumina-fleet/vector/system-guardrails.md')
        if local_sys.exists() and not self._system_guardrails:
            self._system_guardrails = [
                line.strip() for line in local_sys.read_text().splitlines()
                if line.strip() and not line.startswith('#')
            ]

        # Project guardrails
        if self.project_name and self.memory:
            try:
                proj_results = self.memory.query(f'vector {self.project_name} guardrails', top_k=5)
                for r in proj_results:
                    content = r if isinstance(r, str) else r.get('content', '')
                    if content:
                        self._project_guardrails.append(content)
            except Exception as e:
                log.debug(f'Could not load project guardrails: {e}')

        # Fallback: local project file
        if self.project_name:
            local_proj = Path(f'./vector-projects/{self.project_name}/guardrails.md')
            if local_proj.exists() and not self._project_guardrails:
                self._project_guardrails = [
                    line.strip() for line in local_proj.read_text().splitlines()
                    if line.strip() and not line.startswith('#')
                ]

        log.info(f'Guardrails loaded: {len(self._system_guardrails)} system, {len(self._project_guardrails)} project')

    def get_context(self) -> str:
        """Build the guardrails context string to inject into prompts."""
        parts = []
        if self._system_guardrails:
            parts.append('[SYSTEM GUARDRAILS — apply to all tasks]')
            parts.extend(f'- {g}' for g in self._system_guardrails[:10])
        if self._project_guardrails:
            parts.append(f'[PROJECT GUARDRAILS — {self.project_name}]')
            parts.extend(f'- {g}' for g in self._project_guardrails[:10])
        return '\n'.join(parts)

    def classify_correction(self, correction: str) -> str:
        """Classify a Calx correction as 'system' or 'project' scope."""
        if any(re.search(p, correction, re.IGNORECASE) for p in SYSTEM_SCOPE_PATTERNS):
            return 'system'
        return 'project'

    def record_correction(self, correction: str, scope: str = None):
        """Record a correction. Auto-promote to system if threshold reached."""
        if scope is None:
            scope = self.classify_correction(correction)

        key = correction[:80].strip()
        self._correction_counts[key] = self._correction_counts.get(key, 0) + 1
        count = self._correction_counts[key]

        if count >= AUTO_PROMOTE_THRESHOLD and scope == 'project':
            log.info(f'Guardrails: auto-promoting correction to system (count={count}): {key[:50]}')
            self._system_guardrails.append(f'[auto-promoted] {key}')
            if self.memory:
                try:
                    self.memory.store(
                        f'vector-system-guardrails/{key[:40].replace(" ", "-")}',
                        f'[system-guardrail] {key}',
                        layer='patterns'
                    )
                except Exception:
                    pass

    @property
    def system_count(self) -> int:
        return len(self._system_guardrails)

    @property
    def project_count(self) -> int:
        return len(self._project_guardrails)
