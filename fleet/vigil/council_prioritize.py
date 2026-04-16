"""
council_prioritize.py — Vigil briefing prioritization via Obsidian Circle. (OC.6)

Called optionally at briefing start to decide section priority and tone.
Uses the 'quick' preset (single model, fast) to keep cost minimal.

Usage:
    from vigil.council_prioritize import get_briefing_priority
    priority = get_briefing_priority(briefing_type, available_sections, context)
"""

import os
import sys
import logging
from typing import Optional

log = logging.getLogger('vigil.council_prioritize')

_FLEET_DIR = os.environ.get('FLEET_DIR', '/opt/lumina-fleet')
sys.path.insert(0, os.path.join(_FLEET_DIR, 'fleet'))

try:
    from obsidian_circle.engine import convene as _convene
    _OC_OK = True
except ImportError:
    _OC_OK = False

# Keep cost under $0.02 for briefing prioritization
PRIORITY_BUDGET = 0.02

_PRIORITY_SCHEMA = {
    'type': 'object',
    'properties': {
        'priority_sections': {
            'type': 'array',
            'items': {'type': 'string'},
        },
        'tone': {
            'type': 'string',
        },
        'lead_with': {
            'type': 'string',
        },
        'skip_sections': {
            'type': 'array',
            'items': {'type': 'string'},
        },
        'confidence': {'type': 'number'},
    },
}

# Default priority when council is unavailable
_DEFAULT_MORNING = {
    'priority_sections': ['greeting', 'calendar', 'weather', 'commute', 'today_tasks', 'news'],
    'tone': 'energetic and focused',
    'lead_with': 'calendar',
    'skip_sections': [],
}

_DEFAULT_AFTERNOON = {
    'priority_sections': ['inbox', 'today_tasks', 'commute', 'system_status'],
    'tone': 'concise and practical',
    'lead_with': 'today_tasks',
    'skip_sections': [],
}


def get_briefing_priority(
    briefing_type: str,
    available_sections: list,
    context: Optional[dict] = None,
) -> dict:
    """
    Ask the council how to prioritize a Vigil briefing.

    Args:
        briefing_type:      'morning' or 'afternoon'
        available_sections: List of section names that have data
        context:            Optional dict with context hints, e.g.:
                            {'alerts': ['Axon DB down'], 'weather': 'heat warning'}

    Returns dict:
        priority_sections   Ordered list of sections to lead with
        tone                Suggested briefing tone
        lead_with           Single most important section to open with
        skip_sections       Sections to skip (empty by default)
        used_council        bool — whether council was actually used
    """
    default = _DEFAULT_MORNING if briefing_type == 'morning' else _DEFAULT_AFTERNOON

    if not _OC_OK:
        return {**default, 'used_council': False}

    # Only call council if there's something to reason about
    alerts = (context or {}).get('alerts', [])
    if not alerts and len(available_sections) <= 4:
        # Standard briefing, no need for council
        return {**default, 'used_council': False}

    sections_str = ', '.join(available_sections)
    alerts_str = '\n'.join(f'  - {a}' for a in alerts) if alerts else '  None'
    ctx_extra = ''
    if context:
        for k, v in context.items():
            if k != 'alerts':
                ctx_extra += f'  {k}: {v}\n'

    question = f"""Vigil is preparing a {briefing_type} briefing for the operator (non-technical).

Available sections: {sections_str}

Active alerts and context:
{alerts_str}
{ctx_extra}

the operator's situation: He's about to start his {briefing_type} and needs the most important information first. He reads quickly and gets overwhelmed by long briefings.

Recommend:
1. Which sections to prioritize (first 4-5)
2. What tone to set (energetic/calm/urgent/practical)
3. What to lead with (single most important item)
4. Any sections to skip if alerts dominate"""

    try:
        result = _convene(
            question=question,
            circle='quick',
            output_schema=_PRIORITY_SCHEMA,
            budget=PRIORITY_BUDGET,
        )

        structured = result.get('result')
        if isinstance(structured, dict):
            priority = {
                'priority_sections': structured.get('priority_sections', default['priority_sections']),
                'tone': structured.get('tone', default['tone']),
                'lead_with': structured.get('lead_with', default['lead_with']),
                'skip_sections': structured.get('skip_sections', []),
                'used_council': True,
                'council_cost': result.get('cost_usd', 0),
            }
            log.info(f'Briefing priority from council: lead_with={priority["lead_with"]}, tone={priority["tone"]}')
            return priority

    except Exception as e:
        log.warning(f'Council prioritization failed: {e} — using defaults')

    return {**default, 'used_council': False}
