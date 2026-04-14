#!/usr/bin/env python3
"""
Constellation Template Engine
Zero-cost message generation from YAML templates.
Usage: from template_engine import tmpl

ALWAYS use templates for: health alerts, budget alerts, price alerts,
renewal reminders, dashboard insights, commute alerts.
"""

import os
import random
from pathlib import Path
from typing import Optional

TEMPLATE_DIR = Path(os.environ.get('TEMPLATE_DIR', '/opt/lumina-fleet/shared/templates'))

_cache = {}


def _load(template_file: str) -> dict:
    """Load and cache a YAML template file."""
    if template_file not in _cache:
        try:
            import yaml
            path = TEMPLATE_DIR / template_file
            if path.exists():
                with open(path) as f:
                    _cache[template_file] = yaml.safe_load(f) or {}
            else:
                _cache[template_file] = {}
        except Exception:
            _cache[template_file] = {}
    return _cache[template_file]


def _get_template(template_file: str, key_path: str) -> Optional[str]:
    """Get a template string (or list) by dot-path key."""
    data = _load(template_file)
    keys = key_path.split('.')
    current = data
    for k in keys:
        if not isinstance(current, dict) or k not in current:
            return None
        current = current[k]
    if isinstance(current, list):
        return random.choice(current)
    return current


def tmpl(template_file: str, key_path: str, **kwargs) -> str:
    """
    Get a formatted message from a template.

    Args:
        template_file: YAML file name (e.g. 'vitals_coaching.yaml')
        key_path: Dot-path key (e.g. 'celebrations.steps_milestone')
        **kwargs: Format variables to substitute

    Returns:
        Formatted string, or fallback string if template not found.

    Example:
        msg = tmpl('vitals_coaching.yaml', 'celebrations.steps_milestone',
                   target=10000, count=5, total=7)
    """
    template = _get_template(template_file, key_path)
    if template is None:
        # Graceful fallback
        parts = key_path.split('.')
        return f"{parts[-1].replace('_', ' ')}: " + ', '.join(f"{k}={v}" for k, v in kwargs.items())
    try:
        return template.format(**kwargs)
    except KeyError as e:
        return template  # Return unformatted if variable missing


def vitals_coaching(category: str, subtype: str, **kwargs) -> str:
    """Get a vitals coaching message. category: celebrations|nudges|patterns"""
    return tmpl('vitals_coaching.yaml', f'{category}.{subtype}', **kwargs)


def budget_alert(threshold: int, **kwargs) -> str:
    """Get a budget alert. threshold: 50|80|100"""
    key = f'threshold_{threshold}'
    return tmpl('ledger_alerts.yaml', key, **kwargs)


def price_alert(alert_type: str, **kwargs) -> str:
    """Get a trading price alert. alert_type: price_spike|price_drop|fear_greed|weekly_positive|..."""
    return tmpl('meridian_alerts.yaml', alert_type, **kwargs)


def renewal_reminder(reminder_type: str, **kwargs) -> str:
    """Get a document renewal reminder. type: due_soon|due_today|overdue|service_due"""
    return tmpl('relay_reminders.yaml', reminder_type, **kwargs)


def dashboard_insight(insight_type: str, **kwargs) -> str:
    """Get a dashboard insight message. type: delta_up|delta_down|milestone"""
    return tmpl('dashboard_insights.yaml', insight_type, **kwargs)


def dashboard_tip() -> str:
    """Get a random dashboard tip (rotated daily)."""
    tips = _load('dashboard_insights.yaml').get('tips', [])
    if not tips:
        return "Ask Lumina anything via Matrix."
    # Rotate based on day of year for consistency
    from datetime import date
    idx = date.today().timetuple().tm_yday % len(tips)
    return tips[idx]


def vigil_notification(notif_type: str, **kwargs) -> str:
    """Get a Vigil notification. type: briefing_ready|health_all_ok|health_degraded|..."""
    return tmpl('vigil_notifications.yaml', notif_type, **kwargs)


if __name__ == '__main__':
    # Quick test
    print("Template engine test:")
    print(f"  Budget 80%: {budget_alert(80, category='Groceries', spent=160, budget=200, pct=80, days=12)}")
    print(f"  Vitals: {vitals_coaching('celebrations', 'streak', n=7, last_date='2 weeks ago')}")
    print(f"  Renewal: {renewal_reminder('due_soon', doc_type='Car Insurance', days=14, expiry_date='2026-04-22')}")
    print(f"  Tip: {dashboard_tip()}")
    print("OK")
