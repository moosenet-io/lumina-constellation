#!/usr/bin/env python3
"""
Naming ceremony — first-run constellation identity setup.
Run once after deploying a new Lumina instance to customize names.
Usage: python3 naming_ceremony.py
"""

import sys, yaml
from pathlib import Path

CONSTELLATION_YAML = Path('/opt/lumina-fleet/constellation.yaml')

DEFAULTS = {
    'constellation': 'MooseNet',
    'agents': {
        'lumina': 'Lumina',
        'vigil': 'Vigil',
        'sentinel': 'Sentinel',
        'axon': 'Axon',
        'seer': 'Seer',
        'vector': 'Vector',
        'cortex': 'Cortex',
        'engram': 'Engram',
    }
}

def run_ceremony():
    print("=== Lumina Constellation Naming Ceremony ===")
    print("Customize your constellation's identity (press Enter to accept defaults)\n")

    config = {}

    # Constellation name
    name = input(f"Constellation name [{DEFAULTS['constellation']}]: ").strip()
    config['constellation_name'] = name or DEFAULTS['constellation']

    # Agent names
    config['agents'] = {}
    print("\nAgent display names (these appear in Matrix messages and dashboards):")
    for codename, default in DEFAULTS['agents'].items():
        display = input(f"  {codename} [{default}]: ").strip()
        config['agents'][codename] = {'display_name': display or default}

    # Write to constellation.yaml
    if CONSTELLATION_YAML.exists():
        with open(CONSTELLATION_YAML) as f:
            existing = yaml.safe_load(f) or {}
        existing.update(config)
        config = existing

    with open(CONSTELLATION_YAML, 'w') as f:
        yaml.dump(config, f, default_flow_style=False, allow_unicode=True)

    print(f"\nConstellation '{config['constellation_name']}' configured.")
    print(f"Edit {CONSTELLATION_YAML} anytime to update names.")
    print("Run `systemctl restart ironclaw` to apply changes.")

if __name__ == '__main__':
    run_ceremony()
