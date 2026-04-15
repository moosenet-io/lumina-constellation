#!/usr/bin/env python3
"""
generate_litellm_keys.py — Myelin virtual key generation. (MY.2)

Creates all consumer virtual keys in LiteLLM with per-key budgets,
RPM limits, and Spectra access metadata.

Usage:
  python3 generate_litellm_keys.py [--dry-run] [--list]

Environment:
  LITELLM_URL        LiteLLM proxy URL (default: http://YOUR_LITELLM_HOST:4000)
  LITELLM_MASTER_KEY LiteLLM master key for admin operations

Keys are stored in LiteLLM's database. Deploy the generated key values
to each consumer's .env file (LITELLM_API_KEY).

DO NOT commit actual key values to the repo.
"""

import json
import os
import sys
import time
import urllib.request
import urllib.error
from datetime import datetime, timezone

LITELLM_URL = os.environ.get("LITELLM_URL", "http://YOUR_LITELLM_HOST:4000")
LITELLM_MASTER_KEY = os.environ.get("LITELLM_MASTER_KEY", "")

# Consumer registry — all LiteLLM consumers
CONSUMERS = [
    {
        "id": "MY.1",
        "alias": "MY.1-peter-operator",
        "description": "Peter (operator) — unlimited budget, subscription tokens",
        "budget": None,          # unlimited
        "budget_duration": "1d",
        "rpm_limit": 60,
        "models": [],            # all models
        "spectra_enabled": True,
        "spectra_daily_budget": -1,  # unlimited
    },
    {
        "id": "MY.2",
        "alias": "MY.2-lumina-orchestrator",
        "description": "Lumina orchestrator — main agent",
        "budget": 5.0,
        "budget_duration": "1d",
        "rpm_limit": 30,
        "models": [],
        "spectra_enabled": True,
        "spectra_daily_budget": 200,
    },
    {
        "id": "MY.3",
        "alias": "MY.3-ironclaw-agent",
        "description": "IronClaw agent (Spectra disabled — no browser access)",
        "budget": 2.0,
        "budget_duration": "1d",
        "rpm_limit": 30,
        "models": [],
        "spectra_enabled": False,
        "spectra_daily_budget": 0,
    },
    {
        "id": "MY.4",
        "alias": "MY.4-vigil-briefings",
        "description": "Vigil briefing agent",
        "budget": 1.0,
        "budget_duration": "1d",
        "rpm_limit": 10,
        "models": [],
        "spectra_enabled": True,
        "spectra_daily_budget": 20,
    },
    {
        "id": "MY.5",
        "alias": "MY.5-sentinel-ops",
        "description": "Sentinel ops monitoring",
        "budget": 0.50,
        "budget_duration": "1d",
        "rpm_limit": 5,
        "models": [],
        "spectra_enabled": True,
        "spectra_daily_budget": 10,
    },
    {
        "id": "MY.6",
        "alias": "MY.6-seer-research",
        "description": "Seer research agent",
        "budget": 2.0,
        "budget_duration": "1d",
        "rpm_limit": 20,
        "models": [],
        "spectra_enabled": True,
        "spectra_daily_budget": 50,
    },
    {
        "id": "MY.7",
        "alias": "MY.7-vector-dev",
        "description": "Vector autonomous dev loops",
        "budget": 5.0,
        "budget_duration": "1d",
        "rpm_limit": 30,
        "models": [],
        "spectra_enabled": True,
        "spectra_daily_budget": 30,
    },
    {
        "id": "MY.8",
        "alias": "MY.8-guest-readonly",
        "description": "Guest readonly access",
        "budget": 0.25,
        "budget_duration": "1d",
        "rpm_limit": 5,
        "models": [],
        "spectra_enabled": False,
        "spectra_daily_budget": 0,
    },
    {
        "id": "MY.9",
        "alias": "MY.9-shared-household",
        "description": "Shared household members",
        "budget": 0.50,
        "budget_duration": "1d",
        "rpm_limit": 10,
        "models": [],
        "spectra_enabled": False,
        "spectra_daily_budget": 0,
    },
]


def _api(method: str, path: str, data: dict = None) -> dict:
    url = f"{LITELLM_URL}{path}"
    body = json.dumps(data).encode() if data else None
    req = urllib.request.Request(
        url, data=body, method=method,
        headers={
            "Authorization": f"Bearer {LITELLM_MASTER_KEY}",
            "Content-Type": "application/json",
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            return json.loads(r.read())
    except urllib.error.HTTPError as e:
        return {"error": e.read().decode()[:200]}


def generate_key(consumer: dict, dry_run: bool = False) -> dict:
    if dry_run:
        print(f"  [dry-run] Would generate: {consumer['alias']}")
        return {"key": "sk-DRY-RUN", "dry_run": True}

    payload = {
        "key_alias": consumer["alias"],
        "budget_duration": consumer["budget_duration"],
        "rpm_limit": consumer["rpm_limit"],
        "models": consumer["models"],
        "metadata": {
            "consumer_id": consumer["id"],
            "description": consumer["description"],
            "spectra_enabled": consumer["spectra_enabled"],
            "spectra_daily_budget": consumer["spectra_daily_budget"],
            "role": consumer["id"],
            "generated_at": datetime.now(timezone.utc).isoformat(),
        },
    }
    if consumer["budget"] is not None:
        payload["max_budget"] = consumer["budget"]

    return _api("POST", "/key/generate", payload)


def list_keys() -> list:
    result = _api("GET", "/key/list")
    return result.get("keys", [])


def main():
    dry_run = "--dry-run" in sys.argv
    list_mode = "--list" in sys.argv

    if not LITELLM_MASTER_KEY:
        print("ERROR: LITELLM_MASTER_KEY not set")
        sys.exit(1)

    if list_mode:
        keys = list_keys()
        print(f"\nCurrent virtual keys ({len(keys)} total):")
        for k in keys:
            alias = k.get("key_alias", "?")
            budget = k.get("max_budget", "unlimited")
            spend = k.get("spend", 0)
            print(f"  {alias}: budget={budget}/day spend={spend:.4f}")
        return

    print(f"\n=== Myelin Virtual Key Generation {'(DRY RUN)' if dry_run else ''} ===\n")
    generated = {}

    for consumer in CONSUMERS:
        print(f"Generating {consumer['id']}: {consumer['alias']}...")
        result = generate_key(consumer, dry_run=dry_run)
        if "key" in result:
            generated[consumer["id"]] = {
                "alias": consumer["alias"],
                "key": result["key"],
                "budget": consumer["budget"],
                "description": consumer["description"],
            }
            print(f"  OK: {result['key'][:20]}...")
        else:
            print(f"  FAILED: {result.get('error', result)[:100]}")
        if not dry_run:
            time.sleep(1.5)

    if not dry_run and generated:
        out_path = "/tmp/myelin_keys.json"
        with open(out_path, "w") as f:
            json.dump(generated, f, indent=2)
        print(f"\nKeys saved to {out_path}")
        print("\nNEXT STEPS:")
        print("1. Copy each key to the appropriate consumer's .env file (LITELLM_API_KEY)")
        print("2. Store keys in Infisical (workspace: moosenet-services)")
        print("3. DO NOT commit key values to Git")

    print(f"\nGenerated {len(generated)}/{len(CONSUMERS)} keys.")


if __name__ == "__main__":
    main()
