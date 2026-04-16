#!/usr/bin/env python3
"""
synapse_scan.py — Synapse main entry point (run by systemd timer)
MooseNet · Document 26 implementation

Runs the three-stage pipeline:
  1. scanner.py  — detect candidates ($0)
  2. gate.py     — filter candidates ($0)
  3. composer.py — compose + send (local Qwen $0, fallback template $0)

Invoked by synapse.timer every 30 minutes.

Usage:
    python3 synapse_scan.py [--dry-run] [--config /path/to/config.json]
"""

import argparse
import json
import os
import sys
import time
from pathlib import Path

# Allow running from any directory
sys.path.insert(0, str(Path(__file__).parent))
sys.path.insert(0, "/opt/lumina-fleet/shared")

from scanner import SynapseScanner
from gate import SynapseGate
from composer import SynapseComposer

# ---------------------------------------------------------------------------
# Config loading
# ---------------------------------------------------------------------------

CONSTELLATION_YAML = Path(os.environ.get("CONSTELLATION_PATH", "/opt/lumina-fleet/constellation.yaml"))
DEFAULT_CONFIG_PATH = Path("/opt/lumina-fleet/synapse/config.json")


def _load_constellation_synapse() -> dict:
    """Read synapse: section from constellation.yaml (minimal YAML parser)."""
    if not CONSTELLATION_YAML.exists():
        return {}
    config = {}
    in_synapse = False
    indent_level = None
    try:
        with open(CONSTELLATION_YAML) as f:
            for line in f:
                stripped = line.rstrip()
                if stripped.strip().startswith("synapse:"):
                    in_synapse = True
                    indent_level = len(stripped) - len(stripped.lstrip())
                    continue
                if in_synapse:
                    current_indent = len(stripped) - len(stripped.lstrip())
                    if stripped.strip() == "" or (current_indent <= indent_level and stripped.strip()):
                        if not stripped.strip().startswith("#"):
                            in_synapse = False
                            continue
                    key_val = stripped.strip()
                    if ":" in key_val:
                        k, _, v = key_val.partition(":")
                        v = v.strip().strip('"\'')
                        if v.lower() == "true":
                            v = True
                        elif v.lower() == "false":
                            v = False
                        elif v.isdigit():
                            v = int(v)
                        config[k.strip()] = v
    except Exception:
        pass
    return config


def _load_config(config_path: Path = None, dry_run: bool = False) -> dict:
    """Merge config from constellation.yaml → config.json → defaults."""
    defaults = {
        "enabled": False,
        "scan_interval_minutes": 30,
        "max_messages_per_day": 3,
        "relevance_threshold": 0.6,
        "quiet_hours_start": 22,
        "quiet_hours_end": 7,
        "channel": "matrix",
        "strength": "moderate",
        "operator_name": "the operator",
        "topic_block": [],
        "topic_boost": [],
        "interests": [],
        "engram_lookback_hours": 24,
        "dry_run": dry_run,
    }

    # Layer 1: constellation.yaml synapse section
    constellation = _load_constellation_synapse()
    defaults.update(constellation)

    # Layer 2: config.json override
    path = config_path or DEFAULT_CONFIG_PATH
    if path.exists():
        try:
            with open(path) as f:
                file_cfg = json.load(f)
            defaults.update(file_cfg)
        except Exception:
            pass

    # Layer 3: CLI dry-run flag overrides all
    if dry_run:
        defaults["dry_run"] = True

    return defaults


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(description="Synapse scan — spontaneous conversation trigger")
    parser.add_argument("--dry-run", action="store_true", help="Print candidates without sending")
    parser.add_argument("--config", type=Path, help="Path to config.json override")
    args = parser.parse_args()

    config = _load_config(config_path=args.config, dry_run=args.dry_run)

    if not config.get("enabled") and not args.dry_run:
        # Silently exit — synapse is off by default
        sys.exit(0)

    if args.dry_run:
        print(f"[Synapse] dry-run mode. Config: {json.dumps(config, indent=2, default=str)}")

    start = time.time()

    # Stage 1: Detect
    scanner = SynapseScanner(config)
    candidates = scanner.scan()

    if args.dry_run:
        print(f"[Synapse] Stage 1: {len(candidates)} candidates detected")
        for c in candidates:
            print(f"  [{c['score']:.2f}] {c['type']} from {c['source']}")

    # Stage 2: Filter
    gate = SynapseGate(config)
    approved = gate.filter(candidates)

    if args.dry_run:
        print(f"[Synapse] Stage 2: {len(approved)} approved after gate")

    if not approved:
        if args.dry_run:
            print("[Synapse] Nothing to send.")
        sys.exit(0)

    # Stage 3: Compose + send
    composer = SynapseComposer(config)
    for candidate in approved:
        msg = composer.compose_and_send(candidate)
        if msg:
            gate.mark_sent(candidate)
            if args.dry_run:
                print(f"[Synapse] Sent: {msg}")

    elapsed = time.time() - start
    if args.dry_run:
        print(f"[Synapse] Done in {elapsed:.2f}s")


if __name__ == "__main__":
    main()
