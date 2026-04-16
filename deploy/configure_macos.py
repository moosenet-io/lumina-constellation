#!/usr/bin/env python3
"""
configure_macos.py — macOS / Apple Silicon configuration. (DT.5)

Creates Ollama launchd override with OLLAMA_KEEP_ALIVE and other env vars.
No kernel params needed on Apple Silicon — Metal handles VRAM automatically.

Usage:
    python3 deploy/configure_macos.py --preset apple_silicon_128
    python3 deploy/configure_macos.py --preset apple_silicon_128 --dry-run
"""

import argparse
import os
import platform
import subprocess
import sys
import yaml
from pathlib import Path


PRESETS_FILE = Path(__file__).parent / "model_presets.yaml"
LAUNCHD_PLIST = Path.home() / "Library/LaunchAgents/com.lumina.ollama-env.plist"


def load_preset(name: str) -> dict:
    data = yaml.safe_load(PRESETS_FILE.read_text())
    if name not in data:
        print(f"Error: preset '{name}' not found. Available: {', '.join(data.keys())}")
        sys.exit(1)
    return data[name]


def write_launchd_plist(ollama_env: dict, dry_run: bool):
    """Write a launchd plist that sets Ollama env vars system-wide."""
    env_dict = "\n".join(
        f"            <key>{k}</key>\n            <string>{v}</string>"
        for k, v in ollama_env.items()
    )
    plist = f"""<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.lumina.ollama-env</string>
    <key>ProgramArguments</key>
    <array>
        <string>/bin/launchctl</string>
        <string>setenv</string>
        <string>OLLAMA_KEEP_ALIVE</string>
        <string>-1</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>EnvironmentVariables</key>
    <dict>
{env_dict}
    </dict>
</dict>
</plist>
"""

    if dry_run:
        print(f"  [dry-run] Would write {LAUNCHD_PLIST}:")
        for k, v in ollama_env.items():
            print(f"    {k}={v}")
    else:
        LAUNCHD_PLIST.parent.mkdir(parents=True, exist_ok=True)
        LAUNCHD_PLIST.write_text(plist)
        print(f"  ✓ Written {LAUNCHD_PLIST}")

        # Load the plist
        result = subprocess.run(["launchctl", "load", str(LAUNCHD_PLIST)],
                                capture_output=True, text=True)
        if result.returncode == 0:
            print("  ✓ Launchd plist loaded")
        else:
            print(f"  ! launchctl load: {result.stderr.strip()[:100]}")
            print(f"  ! Try: launchctl load {LAUNCHD_PLIST}")

        # Also set immediately in current session
        for k, v in ollama_env.items():
            subprocess.run(["launchctl", "setenv", k, str(v)], capture_output=True)
        print("  ✓ Environment set for current session")


def main():
    if platform.system() != "Darwin":
        print("Error: this script is for macOS only")
        sys.exit(1)

    parser = argparse.ArgumentParser(description="Configure macOS for Lumina")
    parser.add_argument("--preset", default="apple_silicon_128",
                        help="Model preset name (default: apple_silicon_128)")
    parser.add_argument("--dry-run", action="store_true",
                        help="Show what would be done without writing")
    args = parser.parse_args()

    preset = load_preset(args.preset)
    dry = args.dry_run

    print(f"\n{'='*55}")
    print(f"  macOS Configuration — {args.preset}")
    if dry:
        print("  [DRY RUN — no changes will be made]")
    print(f"{'='*55}\n")

    # No kernel params on macOS
    params = preset.get("kernel_params", {})
    if params:
        print("Note: Kernel params not applicable on macOS (Metal handles VRAM)")

    # Ollama env via launchd
    ollama_env = preset.get("ollama_env", {})
    if ollama_env:
        print("Step 1: Ollama environment (launchd)")
        write_launchd_plist(ollama_env, dry)

        print("\nStep 2: Restart Ollama")
        if dry:
            print("  [dry-run] Would run: brew services restart ollama")
        else:
            result = subprocess.run(["brew", "services", "restart", "ollama"],
                                   capture_output=True, text=True)
            if result.returncode == 0:
                print("  ✓ Ollama restarted")
            else:
                print("  ! Could not restart Ollama via brew. Restart manually.")

    # Summary
    print(f"\n{'='*55}")
    models = preset.get("models", [])
    print(f"  Fleet preset: {args.preset}")
    print(f"  No reboot needed (Metal manages VRAM on Apple Silicon)")
    print(f"  Models to pull ({len(models)}):")
    for m in models:
        print(f"    ollama pull {m['name']}  ({m['vram_gb']}GB, {m['role']})")
    if not dry:
        print(f"\n  Next: bash deploy/install.sh --preset {args.preset}")
    print(f"{'='*55}\n")


if __name__ == "__main__":
    main()
