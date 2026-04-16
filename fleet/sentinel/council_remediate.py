"""
council_remediate.py — Sentinel auto-remediation diagnosis via Obsidian Circle. (OC.6)

When Sentinel detects a critical alert, this module asks the council:
  - What is the severity?
  - Can this be auto-remediated?
  - If yes, what's the safe command?

Uses 'security' preset (adversarial + skeptic) to avoid unsafe auto-remediations.

Usage:
    from sentinel.council_remediate import diagnose_remediation
    diagnosis = diagnose_remediation(alert_name, alert_message, health_data)
"""

import json
import logging
import os
import subprocess
import sys
from typing import Optional

log = logging.getLogger('sentinel.council_remediate')

_FLEET_DIR = os.environ.get('FLEET_DIR', '/opt/lumina-fleet')
sys.path.insert(0, os.path.join(_FLEET_DIR, 'fleet'))

try:
    from obsidian_circle.engine import convene as _convene
    _OC_OK = True
except ImportError:
    _OC_OK = False

# Budget for remediation diagnosis (security preset = 2 models)
REMEDIATE_BUDGET = 0.05

# Alerts that are NEVER auto-remediated regardless of council output
_NEVER_AUTO_REMEDIATE = {
    'ironclaw',     # Lead agent — requires human oversight
    'matrix',       # Communication channel — human must validate
    'postgres',     # Database — auto-restart risks data loss
    'llm_cost',     # Cost circuit break — human must review before re-enabling
}

# Maximum auto-remediation confidence threshold (must be very high for auto-action)
AUTO_REMEDIATE_CONFIDENCE = 0.85

_DIAGNOSIS_SCHEMA = {
    'type': 'object',
    'properties': {
        'severity': {
            'type': 'string',
        },
        'root_cause': {'type': 'string'},
        'can_auto_remediate': {'type': 'string'},
        'remediation_command': {'type': 'string'},
        'escalation_message': {'type': 'string'},
        'confidence': {'type': 'number'},
    },
}


def diagnose_remediation(
    alert_name: str,
    alert_message: str,
    health_data: Optional[dict] = None,
    dry_run: bool = True,
) -> dict:
    """
    Ask the council to diagnose a Sentinel alert and recommend remediation.

    Args:
        alert_name:    The check name (e.g., 'axon_db', 'ollama_gpu')
        alert_message: The full alert message from alert_rules.py
        health_data:   Optional health check data dict for context
        dry_run:       If True, never actually execute — just return the diagnosis

    Returns dict:
        severity              'low' | 'medium' | 'high' | 'critical'
        root_cause            Short explanation of what the council thinks went wrong
        can_auto_remediate    bool — council recommendation
        remediation_command   Shell command to run (if can_auto_remediate)
        escalation_message    Message for Lumina/operator (if escalating)
        action_taken          What Sentinel actually did
        council_used          bool
    """
    # Safety gate: never auto-remediate certain services regardless
    blocked = alert_name in _NEVER_AUTO_REMEDIATE

    if not _OC_OK or blocked:
        reason = f'blocked ({alert_name} is in never-auto-remediate list)' if blocked else 'council not available'
        return {
            'severity': 'high',
            'root_cause': 'Council unavailable — manual review required',
            'can_auto_remediate': False,
            'remediation_command': None,
            'escalation_message': f'{alert_message} ({reason})',
            'action_taken': 'escalated',
            'council_used': False,
        }

    # Build context for council
    health_ctx = ''
    if health_data:
        check = health_data.get('checks', {}).get(alert_name, {})
        health_ctx = f'\nHealth data for {alert_name}: {json.dumps(check, indent=2)}'

    question = f"""Sentinel has detected a system alert that may need remediation.

Alert name: {alert_name}
Alert message: {alert_message}{health_ctx}

System context:
- This is a home automation Lumina Constellation system
- Services run in isolated service containers on virtualization
- the operator (non-technical) is the operator — he cannot SSH or run commands
- Auto-remediation should only be used for clearly safe, reversible actions
- Examples of safe auto-remediations: service restart, clearing a queue, temp file cleanup
- Examples of UNSAFE auto-remediations: database operations, credential changes, network changes

Evaluate:
1. What is the severity? (low/medium/high/critical)
2. What is the likely root cause?
3. Can this be safely auto-remediated without human oversight?
   Answer 'yes', 'no', or 'maybe' — err strongly toward 'no' when uncertain.
4. If yes: what is the exact shell command to remediate? (must be safe and reversible)
5. What message should go to the operator via Matrix?"""

    try:
        result = _convene(
            question=question,
            circle='security',  # Adversarial + skeptic for safe remediation decisions
            output_schema=_DIAGNOSIS_SCHEMA,
            budget=REMEDIATE_BUDGET,
        )

        diagnosis = {
            'council_used': True,
            'council_confidence': result.get('confidence', 0),
            'council_cost': result.get('cost_usd', 0),
        }

        structured = result.get('result')
        if isinstance(structured, dict):
            can_remediate_raw = structured.get('can_auto_remediate', 'no')
            can_remediate = (
                str(can_remediate_raw).lower() in ('yes', 'true', '1')
                and result.get('confidence', 0) >= AUTO_REMEDIATE_CONFIDENCE
                and not blocked
                and not dry_run
            )

            diagnosis.update({
                'severity': structured.get('severity', 'high'),
                'root_cause': structured.get('root_cause', 'Unknown'),
                'can_auto_remediate': can_remediate,
                'remediation_command': structured.get('remediation_command') if can_remediate else None,
                'escalation_message': structured.get('escalation_message', alert_message),
            })

            # Execute remediation if approved
            if can_remediate and diagnosis.get('remediation_command'):
                cmd = diagnosis['remediation_command']
                log.warning(f'Auto-remediation approved for {alert_name}: {cmd}')
                try:
                    proc = subprocess.run(
                        cmd, shell=True, capture_output=True, text=True, timeout=30
                    )
                    if proc.returncode == 0:
                        diagnosis['action_taken'] = f'auto_remediated: {cmd}'
                        diagnosis['remediation_output'] = proc.stdout[:200]
                    else:
                        diagnosis['action_taken'] = f'remediation_failed: {proc.stderr[:100]}'
                        diagnosis['can_auto_remediate'] = False
                except Exception as exec_err:
                    diagnosis['action_taken'] = f'remediation_error: {exec_err}'
                    diagnosis['can_auto_remediate'] = False
            else:
                diagnosis['action_taken'] = 'escalated'

        else:
            # Unstructured response — safe default
            diagnosis.update({
                'severity': 'high',
                'root_cause': result.get('synthesis', '')[:200],
                'can_auto_remediate': False,
                'remediation_command': None,
                'escalation_message': alert_message,
                'action_taken': 'escalated',
            })

        return diagnosis

    except Exception as e:
        log.error(f'Council remediation diagnosis failed: {e}')
        return {
            'severity': 'high',
            'root_cause': f'Council error: {e}',
            'can_auto_remediate': False,
            'remediation_command': None,
            'escalation_message': alert_message,
            'action_taken': 'escalated',
            'council_used': False,
        }
