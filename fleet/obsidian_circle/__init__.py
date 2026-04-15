"""
Obsidian Circle — Multi-model reasoning council.
Deep deliberation engine for Lumina Constellation.

Usage:
    from obsidian_circle.engine import convene
    result = convene("Should we migrate to PostgreSQL?", circle="architecture")
"""
from .engine import convene
from .presets import resolve_preset, list_presets
from .personas import get_persona, list_personas
from .output import evaluate_confidence, validate_output, format_for_operator

__all__ = [
    'convene',
    'resolve_preset', 'list_presets',
    'get_persona', 'list_personas',
    'evaluate_confidence', 'validate_output', 'format_for_operator',
]
