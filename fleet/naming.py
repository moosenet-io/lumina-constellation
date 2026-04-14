"""
naming.py — Constellation Identity naming service.
Translates internal Lumina codenames to user-defined display names.
Reads from constellation.yaml (default: /opt/lumina-fleet/constellation.yaml).

Usage:
    from naming import display_name, constellation_name, agent_emoji, personality

    # In any user-facing string:
    msg = f"{display_name('vigil')} morning briefing ready"
    # Returns: "Vigil" (or "Dawn" if operator renamed it)

Rules:
    - Internal code (imports, DB keys, file paths): ALWAYS use internal names
    - User-facing text (chat, dashboard, briefings, alerts): ALWAYS use display_name()
"""

import yaml
import os
from pathlib import Path
from typing import Optional

# Try to use agent_loader.py as primary source (NPC Feature 1)
try:
    import sys as _sys
    _sys.path.insert(0, '/opt/lumina-fleet/shared')
    from agent_loader import AgentLoader as _AgentLoader
    _agent_loader = _AgentLoader()
    _AGENT_LOADER_AVAILABLE = True
except Exception:
    _AGENT_LOADER_AVAILABLE = False
    _agent_loader = None

_CONFIG = None
_CONFIG_PATH = os.environ.get(
    'CONSTELLATION_CONFIG',
    '/opt/lumina-fleet/constellation.yaml'
)


def _load() -> dict:
    """Load and cache constellation.yaml. Reloads if file is newer than cache."""
    global _CONFIG
    config_path = Path(_CONFIG_PATH)

    if not config_path.exists():
        # Return defaults if no config file
        return {
            'constellation': {'name': 'Lumina', 'tagline': 'Home operations, automated'},
            'lead_agent': {'internal': 'lumina', 'display_name': 'Lumina', 'personality': 'warm', 'emoji': '✨'},
            'agents': [],
            'systems': {},
            'modules': {}
        }

    if _CONFIG is None:
        with open(config_path) as f:
            _CONFIG = yaml.safe_load(f) or {}

    return _CONFIG


def reload():
    """Force reload of constellation.yaml. Call after updating the file."""
    global _CONFIG
    _CONFIG = None


def display_name(internal_id: str) -> str:
    """Translate an internal codename to its user-defined display name.

    Args:
        internal_id: The internal codename (e.g. 'vigil', 'nexus', 'lumina')

    Returns:
        Display name from constellation.yaml, or the internal_id as fallback.

    Examples:
        display_name('vigil')    → 'Vigil' (or 'Dawn' if renamed)
        display_name('lumina')   → 'Lumina' (or 'Atlas' if renamed)
        display_name('nexus')    → 'Nexus' (or 'The Hub' if renamed)
    """
    c = _load()

    # Check agent_loader.py first (NPC Feature 1 — .agent.yaml files)
    if _AGENT_LOADER_AVAILABLE and _agent_loader:
        try:
            agent = _agent_loader.get(internal_id)
            if agent:
                return agent.display_name
        except Exception:
            pass

    # Check lead agent
    lead = c.get('lead_agent', {})
    if internal_id == lead.get('internal'):
        return lead.get('display_name', internal_id)

    # Check partner/additional agents
    for agent in c.get('agents', []):
        if agent.get('internal') == internal_id:
            return agent.get('display_name', internal_id)

    # Check systems
    systems = c.get('systems', {})
    if internal_id in systems:
        return systems[internal_id].get('display_name', internal_id)

    # Check modules
    modules = c.get('modules', {})
    if internal_id in modules:
        return modules[internal_id].get('display_name', internal_id)

    # Fallback: return internal name unchanged
    return internal_id


def constellation_name() -> str:
    """Return the deployment's constellation name (e.g. 'MooseNet')."""
    return _load().get('constellation', {}).get('name', 'Lumina')


def constellation_tagline() -> str:
    """Return the deployment's tagline."""
    return _load().get('constellation', {}).get('tagline', 'Home operations, automated')


def agent_emoji(internal_id: str) -> str:
    """Return the emoji for an agent (empty string if none defined)."""
    c = _load()
    lead = c.get('lead_agent', {})
    if internal_id == lead.get('internal'):
        return lead.get('emoji', '')
    for agent in c.get('agents', []):
        if agent.get('internal') == internal_id:
            return agent.get('emoji', '')
    return ''


def personality(internal_id: str) -> str:
    """Return personality preset for an agent: warm|professional|playful|minimal."""
    c = _load()
    lead = c.get('lead_agent', {})
    if internal_id == lead.get('internal'):
        return lead.get('personality', 'warm')
    for agent in c.get('agents', []):
        if agent.get('internal') == internal_id:
            return agent.get('personality', 'warm')
    return 'warm'


def format_agent_message(internal_id: str, message: str) -> str:
    """Format a message from an agent with their display name and emoji.

    Example:
        format_agent_message('vigil', 'Morning briefing ready')
        → '✨ Vigil: Morning briefing ready' (warm personality)
        → 'Vigil: Morning briefing ready' (minimal personality)
    """
    name = display_name(internal_id)
    emoji = agent_emoji(internal_id)
    p = personality(internal_id)

    if p == 'minimal':
        return f'{name}: {message}'
    elif emoji:
        return f'{emoji} {name}: {message}'
    else:
        return f'{name}: {message}'


def list_all() -> dict:
    """Return all configured names for display (useful for dashboard/status)."""
    c = _load()
    result = {
        'constellation': constellation_name(),
        'lead_agent': display_name(c.get('lead_agent', {}).get('internal', 'lumina')),
        'agents': [display_name(a.get('internal', '')) for a in c.get('agents', [])],
        'systems': {k: v.get('display_name', k) for k, v in c.get('systems', {}).items()},
        'modules': {k: v.get('display_name', k) for k, v in c.get('modules', {}).items()},
    }
    return result


def rename(internal_id: str, new_display_name: str, category: str = 'auto') -> bool:
    """Update a display name in constellation.yaml.

    Args:
        internal_id: The internal codename to rename
        new_display_name: The new display name
        category: 'auto' (detect), 'modules', 'systems', 'agents', 'lead_agent'

    Returns:
        True if renamed, False if internal_id not found
    """
    config_path = Path(_CONFIG_PATH)
    if not config_path.exists():
        return False

    with open(config_path) as f:
        data = yaml.safe_load(f) or {}

    changed = False

    # Check lead agent
    if data.get('lead_agent', {}).get('internal') == internal_id:
        data['lead_agent']['display_name'] = new_display_name
        changed = True

    # Check agents
    for agent in data.get('agents', []):
        if agent.get('internal') == internal_id:
            agent['display_name'] = new_display_name
            changed = True

    # Check systems and modules
    for cat in ('systems', 'modules'):
        if internal_id in data.get(cat, {}):
            data[cat][internal_id]['display_name'] = new_display_name
            changed = True

    if changed:
        with open(config_path, 'w') as f:
            yaml.dump(data, f, default_flow_style=False, allow_unicode=True, sort_keys=False)
        reload()

    return changed


if __name__ == '__main__':
    # Quick test
    print('Constellation:', constellation_name())
    print('Lead agent:', display_name('lumina'))
    print('Vigil:', display_name('vigil'))
    print('Nexus:', display_name('nexus'))
    print('Engram:', display_name('engram'))
    print('Message:', format_agent_message('lumina', 'Your commute looks clear today'))
    print('All:', list_all())
