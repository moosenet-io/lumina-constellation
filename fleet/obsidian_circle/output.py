"""
output.py — Obsidian Circle structured output validator. (OC.4)

Validates council synthesis output against a JSON schema.
Evaluates confidence thresholds to determine the recommended action.

Confidence thresholds:
  >= 0.8  → auto_act           (council confident, proceed automatically)
  0.5-0.8 → ask_operator       (uncertain, surface recommendation + ask Peter)
  < 0.5   → surface_deliberation (low confidence, show full deliberation log)
"""

from typing import Any, Optional


# ── Schema validation ────────────────────────────────────────────────────────

def validate_output(data: Any, schema: dict) -> tuple:
    """
    Validate and coerce data against a simplified JSON schema subset.

    Supports: type (object/array/string/number/boolean), properties,
              required, enum, items, default.

    Returns:
        (coerced_data, list_of_error_strings)
        errors is empty list on full success.
    """
    errors: list = []
    result = _validate_node(data, schema, path='root', errors=errors)
    return result, errors


def _validate_node(value: Any, schema: dict, path: str, errors: list) -> Any:
    """Recursively validate and coerce a value against a schema node."""
    if not schema:
        return value

    schema_type = schema.get('type')

    if schema_type == 'object':
        if not isinstance(value, dict):
            errors.append(f'{path}: expected object, got {type(value).__name__}')
            return value

        result = {}
        properties = schema.get('properties', {})
        required = schema.get('required', [])

        for prop, prop_schema in properties.items():
            if prop in value:
                result[prop] = _validate_node(
                    value[prop], prop_schema, f'{path}.{prop}', errors
                )
            elif prop in required:
                errors.append(f'{path}.{prop}: required field missing')
            elif 'default' in prop_schema:
                result[prop] = prop_schema['default']

        # Passthrough extra fields
        for key in value:
            if key not in properties:
                result[key] = value[key]

        return result

    elif schema_type == 'array':
        if not isinstance(value, list):
            if isinstance(value, str):
                errors.append(f'{path}: coerced string to single-element array')
                value = [value]
            else:
                errors.append(f'{path}: expected array, got {type(value).__name__}')
                return value

        items_schema = schema.get('items', {})
        return [
            _validate_node(item, items_schema, f'{path}[{i}]', errors)
            for i, item in enumerate(value)
        ]

    elif schema_type == 'string':
        if not isinstance(value, str):
            errors.append(f'{path}: coerced {type(value).__name__} to string')
            value = str(value)

        enum = schema.get('enum')
        if enum and value not in enum:
            errors.append(f'{path}: {value!r} not in allowed values {enum}')

        return value

    elif schema_type == 'number':
        if isinstance(value, str):
            try:
                coerced = float(value)
                errors.append(f'{path}: coerced string to number')
                return coerced
            except ValueError:
                errors.append(f'{path}: cannot convert {value!r} to number')
                return 0.0
        if not isinstance(value, (int, float)):
            errors.append(f'{path}: expected number, got {type(value).__name__}')
            return 0.0
        return value

    elif schema_type == 'boolean':
        if not isinstance(value, bool):
            errors.append(f'{path}: coerced to boolean')
            return bool(value)
        return value

    elif schema_type == 'integer':
        if isinstance(value, float) and value.is_integer():
            return int(value)
        if isinstance(value, str):
            try:
                return int(value)
            except ValueError:
                errors.append(f'{path}: cannot convert {value!r} to integer')
                return 0
        if not isinstance(value, int):
            errors.append(f'{path}: expected integer, got {type(value).__name__}')
            return 0
        return value

    return value


# ── Confidence threshold evaluation ─────────────────────────────────────────

def evaluate_confidence(confidence: float) -> str:
    """
    Map a confidence score (0.0-1.0) to the recommended action.

    Returns:
        'auto_act'              confidence >= 0.8
        'ask_operator'          0.5 <= confidence < 0.8
        'surface_deliberation'  confidence < 0.5
    """
    if confidence >= 0.8:
        return 'auto_act'
    elif confidence >= 0.5:
        return 'ask_operator'
    else:
        return 'surface_deliberation'


def action_label(action: str) -> str:
    """Human-readable label for an action code."""
    return {
        'auto_act': 'Auto-act (high confidence)',
        'ask_operator': 'Ask operator (moderate confidence)',
        'surface_deliberation': 'Surface deliberation (low confidence)',
    }.get(action, action)


# ── Formatting ───────────────────────────────────────────────────────────────

def format_for_operator(result: dict) -> str:
    """
    Format a council result as a plain-English message for Peter.
    Used in Matrix notifications and Soma alerts.
    """
    action = result.get('action', 'ask_operator')
    confidence = result.get('confidence', 0.0)
    synthesis = result.get('synthesis', '')
    positions = result.get('positions', [])
    cost = result.get('cost_usd', 0.0)
    circle = result.get('circle', 'quick')
    member_count = result.get('member_count', 0)
    elapsed = result.get('elapsed_s', 0)

    pct = int(confidence * 100)

    lines = [
        f'Obsidian Circle [{circle}] — {member_count} member(s) — {pct}% confidence — ${cost:.4f} — {elapsed}s',
        '',
    ]

    if action == 'auto_act':
        lines.append('Strong consensus. Proceeding automatically.')
    elif action == 'ask_operator':
        lines.append('Recommendation ready. Your input needed:')
    else:
        lines.append('Low confidence. Full deliberation surface for review:')

    lines.append('')

    # Synthesis excerpt (first 600 chars)
    if synthesis:
        excerpt = synthesis[:600]
        if len(synthesis) > 600:
            excerpt += '...'
        lines.append(excerpt)
        lines.append('')

    # Surface individual positions for low confidence results
    if action == 'surface_deliberation':
        lines.append('Individual positions:')
        for p in positions:
            if not p.get('error'):
                brief = p.get('position', '')[:150]
                conf_str = f"{p.get('confidence', 0):.0%}"
                lines.append(f"  [{p['member_id']} — {conf_str}]: {brief}...")
        lines.append('')

    lines.append(f'Cost: ${cost:.4f} | Circle: {circle} | Action: {action_label(action)}')

    return '\n'.join(lines)


def format_brief(result: dict) -> str:
    """One-line summary of a council result for logs and status tables."""
    circle = result.get('circle', 'quick')
    confidence = result.get('confidence', 0.0)
    action = result.get('action', '?')
    cost = result.get('cost_usd', 0.0)
    return (
        f"[{circle}] conf={confidence:.0%} action={action} "
        f"members={result.get('member_count', 0)} cost=${cost:.4f}"
    )
