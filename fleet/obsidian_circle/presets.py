"""
presets.py — Obsidian Circle circle presets. (OC.2)

7 built-in presets:
  quick        — Mr. Wizard solo, fast answer
  architecture — 3 personas (Prism), structural design
  security     — 2 adversarial personas, threat modeling
  cost         — 2 cost-focused personas, efficiency review
  research     — 4 distinct models, multi-source synthesis
  full         — 7 personas, maximum deliberation
  custom       — User-defined, loaded from constellation.yaml

Custom presets CRUD via constellation.yaml under council.circles.
"""

import copy
import os
from pathlib import Path
from typing import Optional

_CONSTELLATION_YAML = Path(os.environ.get('FLEET_DIR', '/opt/lumina-fleet')) / 'fleet' / 'constellation.yaml'

# ── Built-in presets ─────────────────────────────────────────────────────────

_BUILTIN_PRESETS: dict = {
    'quick': {
        'display_name': 'Quick',
        'description': 'Mr. Wizard solo — fast single-model answer',
        'members': [
            {'id': 'wizard', 'model': 'Lumina', 'persona_id': 'pragmatist',
             'max_tokens': 800, 'temperature': 0.3},
        ],
        'tools': ['nexus_check', 'plane_list_issues'],
        'synthesis_model': 'Lumina',
        'prism_model': 'Lumina',
        'default_schema': None,
    },

    'architecture': {
        'display_name': 'Architecture',
        'description': '3 Prism personas — Architect, Skeptic, Pragmatist',
        'members': [
            {'id': 'architect', 'model': 'claude-opus-4-6', 'persona_id': 'architect',
             'max_tokens': 800, 'temperature': 0.3},
            {'id': 'skeptic', 'model': 'Lumina', 'persona_id': 'skeptic',
             'max_tokens': 600, 'temperature': 0.2},
            {'id': 'pragmatist', 'model': 'Lumina Fast', 'persona_id': 'pragmatist',
             'max_tokens': 500, 'temperature': 0.3},
        ],
        'tools': [],
        'synthesis_model': 'Lumina',
        'prism_model': 'Lumina',
        'default_schema': {
            'type': 'object',
            'properties': {
                'recommendation': {'type': 'string'},
                'trade_offs': {'type': 'array', 'items': {'type': 'string'}},
                'risks': {'type': 'array', 'items': {'type': 'string'}},
                'confidence': {'type': 'number'},
                'summary': {'type': 'string'},
            },
        },
    },

    'security': {
        'display_name': 'Security',
        'description': 'Adversarial review — Security Auditor + Skeptic',
        'members': [
            {'id': 'security', 'model': 'Lumina', 'persona_id': 'security',
             'max_tokens': 700, 'temperature': 0.2},
            {'id': 'skeptic', 'model': 'Lumina', 'persona_id': 'skeptic',
             'max_tokens': 600, 'temperature': 0.1},
        ],
        'tools': [],
        'synthesis_model': 'Lumina',
        'prism_model': 'Lumina',
        'default_schema': {
            'type': 'object',
            'properties': {
                'severity': {'type': 'string'},
                'findings': {'type': 'array', 'items': {'type': 'string'}},
                'mitigations': {'type': 'array', 'items': {'type': 'string'}},
                'confidence': {'type': 'number'},
            },
        },
    },

    'cost': {
        'display_name': 'Cost',
        'description': 'Cost + efficiency review — Cost Optimizer + Pragmatist',
        'members': [
            {'id': 'cost', 'model': 'Lumina Fast', 'persona_id': 'cost',
             'max_tokens': 600, 'temperature': 0.2},
            {'id': 'pragmatist', 'model': 'Lumina Fast', 'persona_id': 'pragmatist',
             'max_tokens': 500, 'temperature': 0.3},
        ],
        'tools': [],
        'synthesis_model': 'Lumina Fast',
        'prism_model': 'Lumina Fast',
        'default_schema': {
            'type': 'object',
            'properties': {
                'estimated_cost': {'type': 'string'},
                'alternatives': {'type': 'array', 'items': {'type': 'string'}},
                'recommendation': {'type': 'string'},
                'confidence': {'type': 'number'},
            },
        },
    },

    'research': {
        'display_name': 'Research',
        'description': 'Multi-model research synthesis — 4 distinct architectures',
        'members': [
            {'id': 'architect', 'model': 'claude-opus-4-6', 'persona_id': 'architect',
             'max_tokens': 800, 'temperature': 0.3},
            {'id': 'skeptic', 'model': 'Lumina', 'persona_id': 'skeptic',
             'max_tokens': 700, 'temperature': 0.2},
            {'id': 'pragmatist', 'model': 'Lumina Fast', 'persona_id': 'pragmatist',
             'max_tokens': 500, 'temperature': 0.3},
            {'id': 'devils_advocate', 'model': 'Lumina', 'persona_id': 'devils_advocate',
             'max_tokens': 600, 'temperature': 0.5},
        ],
        'tools': [],
        'synthesis_model': 'Lumina',
        'prism_model': 'Lumina',
        'default_schema': None,
    },

    'full': {
        'display_name': 'Full Council',
        'description': 'All 7 personas — maximum deliberation, highest cost',
        'members': [
            {'id': 'architect', 'model': 'claude-opus-4-6', 'persona_id': 'architect',
             'max_tokens': 800, 'temperature': 0.3},
            {'id': 'skeptic', 'model': 'Lumina', 'persona_id': 'skeptic',
             'max_tokens': 600, 'temperature': 0.1},
            {'id': 'pragmatist', 'model': 'Lumina Fast', 'persona_id': 'pragmatist',
             'max_tokens': 500, 'temperature': 0.3},
            {'id': 'security', 'model': 'Lumina', 'persona_id': 'security',
             'max_tokens': 600, 'temperature': 0.2},
            {'id': 'user', 'model': 'Lumina Fast', 'persona_id': 'user',
             'max_tokens': 400, 'temperature': 0.4},
            {'id': 'cost', 'model': 'Lumina Fast', 'persona_id': 'cost',
             'max_tokens': 400, 'temperature': 0.2},
            {'id': 'devils_advocate', 'model': 'Lumina', 'persona_id': 'devils_advocate',
             'max_tokens': 500, 'temperature': 0.6},
        ],
        'tools': [],
        'synthesis_model': 'Lumina',
        'prism_model': 'Lumina',
        'default_schema': None,
    },

    'custom': {
        'display_name': 'Custom',
        'description': 'User-defined preset from constellation.yaml',
        'members': [
            {'id': 'pragmatist', 'model': 'Lumina', 'persona_id': 'pragmatist',
             'max_tokens': 700, 'temperature': 0.3},
        ],
        'tools': [],
        'synthesis_model': 'Lumina',
        'prism_model': 'Lumina',
        'default_schema': None,
    },
}


# ── YAML custom presets ──────────────────────────────────────────────────────

def _load_yaml_presets() -> dict:
    """Load custom presets from constellation.yaml council.circles."""
    try:
        if _CONSTELLATION_YAML.exists():
            import yaml
            data = yaml.safe_load(_CONSTELLATION_YAML.read_text())
            return data.get('council', {}).get('circles', {}) or {}
    except Exception:
        pass
    return {}


def _save_constellation(data: dict) -> bool:
    try:
        import yaml
        _CONSTELLATION_YAML.write_text(yaml.dump(data, default_flow_style=False, sort_keys=False))
        return True
    except Exception:
        return False


# ── Public API ───────────────────────────────────────────────────────────────

def resolve_preset(name: str) -> dict:
    """
    Resolve a preset by name. Returns a deep copy of the preset config.
    Falls back to 'quick' if the name is not found.
    """
    if name in _BUILTIN_PRESETS:
        return copy.deepcopy(_BUILTIN_PRESETS[name])

    yaml_presets = _load_yaml_presets()
    if name in yaml_presets:
        preset = copy.deepcopy(yaml_presets[name])
        preset.setdefault('members', _BUILTIN_PRESETS['quick']['members'])
        preset.setdefault('synthesis_model', 'Lumina')
        preset.setdefault('prism_model', 'Lumina')
        preset.setdefault('default_schema', None)
        preset.setdefault('tools', [])
        return preset

    return copy.deepcopy(_BUILTIN_PRESETS['quick'])


def list_presets() -> list:
    """List all available presets (built-in + custom)."""
    presets = []
    for name, p in _BUILTIN_PRESETS.items():
        presets.append({
            'name': name,
            'display_name': p.get('display_name', name),
            'description': p.get('description', ''),
            'member_count': len(p.get('members', [])),
            'source': 'builtin',
        })

    for name, p in _load_yaml_presets().items():
        if name not in _BUILTIN_PRESETS:
            presets.append({
                'name': name,
                'display_name': p.get('display_name', name),
                'description': p.get('description', ''),
                'member_count': len(p.get('members', [])),
                'source': 'custom',
            })

    return presets


def save_custom_preset(name: str, preset: dict) -> bool:
    """Save a custom preset to constellation.yaml under council.circles."""
    if name in _BUILTIN_PRESETS:
        return False  # Built-ins are immutable

    try:
        import yaml
        data = {}
        if _CONSTELLATION_YAML.exists():
            data = yaml.safe_load(_CONSTELLATION_YAML.read_text()) or {}

        data.setdefault('council', {}).setdefault('circles', {})[name] = preset
        return _save_constellation(data)
    except Exception:
        return False


def delete_custom_preset(name: str) -> bool:
    """Delete a custom preset. Built-ins cannot be deleted."""
    if name in _BUILTIN_PRESETS:
        return False

    try:
        import yaml
        data = yaml.safe_load(_CONSTELLATION_YAML.read_text()) or {}
        circles = data.get('council', {}).get('circles', {})
        if name in circles:
            del circles[name]
            return _save_constellation(data)
    except Exception:
        pass
    return False
