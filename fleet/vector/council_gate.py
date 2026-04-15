"""
council_gate.py — Vector gate failure escalation via Obsidian Circle. (OC.6)

Called when a task accumulates >= 3 Calx gate failures in a single loop,
indicating the task is stuck. Asks the council: split, reduce scope, or escalate?

Usage:
    from vector.council_gate import check_and_escalate
    action = check_and_escalate(task_id, task_desc, gate_fail_count, gate_fail_types)
"""

import os
import sys
import json
import logging
from typing import Optional

log = logging.getLogger('vector.council_gate')

# Gate failure threshold before escalating to council
GATE_FAIL_THRESHOLD = int(os.environ.get('VECTOR_GATE_FAIL_THRESHOLD', '3'))

# Council budget for stuck-task diagnosis
COUNCIL_BUDGET = float(os.environ.get('VECTOR_COUNCIL_BUDGET', '0.05'))

# Add fleet root to path for obsidian_circle import
_FLEET_DIR = os.environ.get('FLEET_DIR', '/opt/lumina-fleet')
sys.path.insert(0, os.path.join(_FLEET_DIR, 'fleet'))

try:
    from obsidian_circle.engine import convene as _convene
    _OC_OK = True
except ImportError:
    _OC_OK = False
    log.warning('obsidian_circle not available — gate escalation will use fallback')


_ESCALATION_SCHEMA = {
    'type': 'object',
    'properties': {
        'recommendation': {
            'type': 'string',
            'enum': ['split', 'reduce_scope', 'escalate', 'retry'],
        },
        'reasoning': {'type': 'string'},
        'suggested_subtasks': {
            'type': 'array',
            'items': {'type': 'string'},
        },
        'escalation_message': {'type': 'string'},
        'confidence': {'type': 'number'},
    },
}


def check_and_escalate(
    task_id: str,
    task_description: str,
    gate_fail_count: int,
    gate_fail_types: Optional[list] = None,
    cost_so_far: float = 0.0,
) -> dict:
    """
    Check if a task has hit the gate failure threshold and escalate to council.

    Args:
        task_id:           Vector loop task identifier
        task_description:  Human-readable task description
        gate_fail_count:   Number of gate failures so far in this loop
        gate_fail_types:   List of Calx trigger types that fired (e.g. ['T2', 'T3'])
        cost_so_far:       USD spent on this task so far

    Returns dict:
        should_escalate  bool — True if threshold was hit
        action           'split' | 'reduce_scope' | 'escalate' | 'retry' | 'continue'
        council_result   Full council result dict (if escalated)
        message          Human-readable recommendation for Lumina/operator
    """
    if gate_fail_count < GATE_FAIL_THRESHOLD:
        return {
            'should_escalate': False,
            'action': 'continue',
            'message': f'Gate fails: {gate_fail_count}/{GATE_FAIL_THRESHOLD} — continuing',
        }

    log.warning(
        f'Task {task_id} hit gate threshold: {gate_fail_count} failures '
        f'({gate_fail_types}) — consulting council'
    )

    if not _OC_OK:
        # Fallback: no council, auto-escalate to operator
        return {
            'should_escalate': True,
            'action': 'escalate',
            'council_result': None,
            'message': (
                f'Task stuck after {gate_fail_count} gate failures '
                f'({gate_fail_types}). Council unavailable — escalating to operator.'
            ),
        }

    gate_types_str = ', '.join(gate_fail_types) if gate_fail_types else 'unknown'

    question = f"""Vector autonomous dev loop is stuck on a task.

Task ID: {task_id}
Task description: {task_description}
Gate failures: {gate_fail_count} (types: {gate_types_str})
Cost so far: ${cost_so_far:.4f}

Gate failure types:
  T1 = Syntax/compilation error (code doesn't parse or compile)
  T2 = Test failure (tests are failing after the change)
  T3 = Security gate (security scanner flagged an issue)
  T4 = Review gate (code review rules violated)

The task has failed {gate_fail_count} times. Recommend one of:
  split         — Break this task into 2-3 smaller subtasks
  reduce_scope  — Narrow the task scope to make it achievable
  escalate      — This needs human/Lumina intervention, stop the loop
  retry         — The failures look transient, retry with same approach

Provide specific subtasks if recommending 'split', and a clear message to the operator if recommending 'escalate'."""

    try:
        council_result = _convene(
            question=question,
            circle='quick',  # Fast single-model decision for stuck tasks
            output_schema=_ESCALATION_SCHEMA,
            budget=COUNCIL_BUDGET,
        )

        action = 'escalate'  # Safe default
        message = f'Task stuck after {gate_fail_count} gate failures.'

        structured = council_result.get('result')
        if isinstance(structured, dict):
            action = structured.get('recommendation', 'escalate')
            reasoning = structured.get('reasoning', '')
            subtasks = structured.get('suggested_subtasks', [])
            esc_msg = structured.get('escalation_message', '')

            if action == 'split' and subtasks:
                message = f'Council recommends splitting task into subtasks:\n' + '\n'.join(f'  • {s}' for s in subtasks)
            elif action == 'escalate':
                message = esc_msg or f'Task requires operator intervention after {gate_fail_count} failures.'
            elif action == 'reduce_scope':
                message = f'Council recommends reducing scope: {reasoning[:200]}'
            else:
                message = f'Council recommends: {action}. {reasoning[:200]}'
        else:
            # Unstructured synthesis
            message = council_result.get('synthesis', '')[:400]

        return {
            'should_escalate': True,
            'action': action,
            'council_result': council_result,
            'message': message,
        }

    except Exception as e:
        log.error(f'Council gate escalation failed: {e}')
        return {
            'should_escalate': True,
            'action': 'escalate',
            'council_result': None,
            'message': f'Task stuck after {gate_fail_count} failures. Council error ({e}) — escalating to operator.',
        }
