#!/usr/bin/env python3
"""
household_config.py — Household agent topology and topic routing config.

Defines which agents participate in the household constellation and
which agents subscribe to each event topic.  Serialisable to/from JSON
for persistence (e.g. saving overrides to disk or passing over the wire).

Usage:
    from nexus.household_config import HOUSEHOLD_AGENTS, HOUSEHOLD_TOPICS, route_household_event
"""

import json
import logging
from typing import Dict, List, Optional

log = logging.getLogger('household_config')

# ── Constellation membership ──────────────────────────────────────────────────

HOUSEHOLD_AGENTS: List[str] = ['lumina', 'partner']

# ── Topic → subscriber mapping ────────────────────────────────────────────────
# Each topic key maps to a list of agent IDs that should receive that event.
# Reasoning per topic:
#   grocery_update        → both shop / plan together
#   calendar_event        → both need schedule awareness
#   chore_reminder        → both share household tasks
#   finance_alert         → lumina only (infra + budget oversight); partner gets a
#                           summary via lumina if the operator wishes, not raw alerts
#   shopping_list_update  → both collaborate on shopping

HOUSEHOLD_TOPICS: Dict[str, List[str]] = {
    'grocery_update':       ['lumina', 'partner'],
    'calendar_event':       ['lumina', 'partner'],
    'chore_reminder':       ['lumina', 'partner'],
    'finance_alert':        ['lumina'],
    'shopping_list_update': ['lumina', 'partner'],
}

# ── Routing ───────────────────────────────────────────────────────────────────

def route_household_event(
    event_type: str,
    payload: dict,
    priority: str = 'normal',
    dry_run: bool = False,
) -> dict:
    """
    Route a household event to the correct agents based on HOUSEHOLD_TOPICS.

    Calls send_household() for each subscribed agent.  Import is deferred to
    avoid a circular dependency between config and routing modules.

    Args:
        event_type: Event key from HOUSEHOLD_TOPICS.
        payload:    Event data dict.
        priority:   Message priority ('critical'|'urgent'|'normal'|'low').
        dry_run:    If True, returns the routing plan without sending.

    Returns:
        dict with keys:
            event_type   — echoed back
            subscribers  — list of agent IDs that will receive the message
            results      — list of send results (empty if dry_run)
            dry_run      — bool echoed back
    """
    subscribers = HOUSEHOLD_TOPICS.get(event_type)
    if subscribers is None:
        log.warning('route_household_event: unknown event_type "%s"', event_type)
        return {
            'event_type': event_type,
            'subscribers': [],
            'results': [],
            'dry_run': dry_run,
            'error': f'Unknown event_type: {event_type}',
        }

    if dry_run:
        log.info('route dry-run [%s] → %s', event_type, subscribers)
        return {
            'event_type': event_type,
            'subscribers': subscribers,
            'results': [],
            'dry_run': True,
        }

    # Deferred import prevents circular import with household_routing
    from household_routing import send_household  # noqa: PLC0415

    results = []
    for agent in subscribers:
        try:
            result = send_household(
                from_agent='router',
                to_agent=agent,
                event_type=event_type,
                payload=payload,
                priority=priority,
            )
            results.append({'agent': agent, **result})
        except Exception as exc:
            log.error('Failed to route %s → %s: %s', event_type, agent, exc)
            results.append({'agent': agent, 'status': 'error', 'error': str(exc)})

    log.info('route [%s] sent to %d agents', event_type, len(results))
    return {
        'event_type': event_type,
        'subscribers': subscribers,
        'results': results,
        'dry_run': False,
    }

# ── JSON serialisation ────────────────────────────────────────────────────────

def to_json() -> str:
    """Serialise current config to a JSON string."""
    return json.dumps(
        {
            'household_agents': HOUSEHOLD_AGENTS,
            'household_topics': HOUSEHOLD_TOPICS,
        },
        indent=2,
    )


def from_json(data: str) -> dict:
    """
    Deserialise a JSON config string.

    Returns the parsed dict. Does NOT mutate module-level constants —
    callers that need dynamic config should build their own routing layer
    on top of the returned dict.
    """
    parsed = json.loads(data)
    if 'household_agents' not in parsed or 'household_topics' not in parsed:
        raise ValueError('JSON must contain household_agents and household_topics keys')
    return parsed


def get_subscribers(event_type: str) -> List[str]:
    """Return the subscriber list for a given event_type, or [] if unknown."""
    return HOUSEHOLD_TOPICS.get(event_type, [])


def is_household_agent(agent_id: str) -> bool:
    """Return True if agent_id is a registered household agent."""
    return agent_id in HOUSEHOLD_AGENTS


# ── CLI ───────────────────────────────────────────────────────────────────────

if __name__ == '__main__':
    import argparse

    logging.basicConfig(level=logging.INFO, format='%(levelname)s %(message)s')

    parser = argparse.ArgumentParser(description='Household config inspection')
    sub = parser.add_subparsers(dest='cmd')

    sub.add_parser('show', help='Print current config as JSON')

    p = sub.add_parser('subscribers', help='List subscribers for an event type')
    p.add_argument('event_type')

    p = sub.add_parser('route', help='Route an event (dry-run by default)')
    p.add_argument('event_type')
    p.add_argument('--payload', default='{}')
    p.add_argument('--priority', default='normal')
    p.add_argument('--send', action='store_true', help='Actually send (not dry-run)')

    args = parser.parse_args()

    if args.cmd == 'show':
        print(to_json())
    elif args.cmd == 'subscribers':
        print(json.dumps(get_subscribers(args.event_type), indent=2))
    elif args.cmd == 'route':
        result = route_household_event(
            args.event_type,
            json.loads(args.payload),
            args.priority,
            dry_run=not args.send,
        )
        print(json.dumps(result, indent=2))
    else:
        parser.print_help()
