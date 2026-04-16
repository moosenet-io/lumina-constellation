#!/usr/bin/env python3
"""
configure_strix_halo.py — Strix Halo (AMD Ryzen AI Max) configuration. (DT.4)

Sets kernel parameters for maximum GPU memory allocation and creates
Ollama systemd override with required environment variables.

Usage:
    python3 deploy/configure_strix_halo.py --preset strix_halo_128
    python3 deploy/configure_strix_halo.py --preset strix_halo_128 --dry-run
"""

import argparse
import os
import sys
import yaml
from pathlib import Path


PRESETS_FILE = Path(__file__).parent / "model_presets.yaml"
OLLAMA_OVERRIDE_DIR = Path("/etc/systemd/system/ollama.service.d")
OLLAMA_OVERRIDE_FILE = OLLAMA_OVERRIDE_DIR / "lumina-override.conf"

# Bootloader detection
GRUB_DEFAULT = Path("/etc/default/grub")
SYSTEMD_BOOT_ENTRIES = Path("/boot/loader/entries")


def load_preset(name: str) -> dict:
    data = yaml.safe_load(PRESETS_FILE.read_text())
    if name not in data:
        print(f"Error: preset '{name}' not found. Available: {', '.join(data.keys())}")
        sys.exit(1)
    return data[name]


def detect_bootloader() -> str:
    if GRUB_DEFAULT.exists():
        return "grub"
    if SYSTEMD_BOOT_ENTRIES.exists():
        return "systemd-boot"
    return "unknown"


def build_kernel_cmdline(params: dict) -> str:
    """Build kernel command line additions from params dict."""
    return " ".join(f"{k}={v}" for k, v in params.items())


def write_grub_config(params: dict, dry_run: bool):
    """Update GRUB_CMDLINE_LINUX_DEFAULT with AMD GPU params."""
    cmdline = build_kernel_cmdline(params)
    content = GRUB_DEFAULT.read_text() if GRUB_DEFAULT.exists() else ""

    if "GRUB_CMDLINE_LINUX_DEFAULT" in content:
        import re
        # Add params if not already present
        missing = [f"{k}={v}" for k, v in params.items() if f"{k}=" not in content]
        if not missing:
            print("  ✓ GRUB already has required kernel params")
            return
        add = " ".join(missing)
        new_content = re.sub(
            r'(GRUB_CMDLINE_LINUX_DEFAULT=")(.*?)(")',
            lambda m: f'{m.group(1)}{m.group(2)} {add}{m.group(3)}',
            content
        )
    else:
        new_content = content + f'\nGRUB_CMDLINE_LINUX_DEFAULT="{cmdline}"\n'

    if dry_run:
        print(f"  [dry-run] Would write to {GRUB_DEFAULT}:")
        print(f"  + Kernel params: {cmdline}")
        print(f"  + Then run: sudo update-grub")
    else:
        GRUB_DEFAULT.write_text(new_content)
        print(f"  ✓ Updated {GRUB_DEFAULT}")
        print(f"  ! Run 'sudo update-grub && sudo reboot' to apply")


def write_systemd_boot_config(params: dict, dry_run: bool):
    """Add kernel params to systemd-boot entries."""
    entries = list(SYSTEMD_BOOT_ENTRIES.glob("*.conf")) if SYSTEMD_BOOT_ENTRIES.exists() else []
    if not entries:
        print("  ✗ No systemd-boot entries found")
        return
    cmdline = build_kernel_cmdline(params)
    for entry in entries:
        if dry_run:
            print(f"  [dry-run] Would add to {entry}: options {cmdline}")
        else:
            text = entry.read_text()
            if "options" in text:
                if any(f"{k}=" in text for k in params):
                    print(f"  ✓ {entry.name}: params already present")
                    continue
                text = text.replace("options ", f"options {cmdline} ", 1)
            else:
                text += f"\noptions {cmdline}\n"
            entry.write_text(text)
            print(f"  ✓ Updated {entry.name}")
    if not dry_run:
        print("  ! Reboot required to apply kernel params")


def write_ollama_override(ollama_env: dict, dry_run: bool):
    """Write systemd override for Ollama environment variables."""
    lines = ["[Service]"]
    for k, v in ollama_env.items():
        lines.append(f'Environment="{k}={v}"')
    content = "\n".join(lines) + "\n"

    if dry_run:
        print(f"  [dry-run] Would write {OLLAMA_OVERRIDE_FILE}:")
        for line in lines:
            print(f"    {line}")
    else:
        OLLAMA_OVERRIDE_DIR.mkdir(parents=True, exist_ok=True)
        OLLAMA_OVERRIDE_FILE.write_text(content)
        print(f"  ✓ Written {OLLAMA_OVERRIDE_FILE}")
        print("  ! Run 'sudo systemctl daemon-reload && sudo systemctl restart ollama'")


def main():
    parser = argparse.ArgumentParser(description="Configure Strix Halo for Lumina")
    parser.add_argument("--preset", default="strix_halo_128",
                        help="Model preset name (default: strix_halo_128)")
    parser.add_argument("--dry-run", action="store_true",
                        help="Show what would be done without writing")
    args = parser.parse_args()

    preset = load_preset(args.preset)
    dry = args.dry_run

    if "strix_halo" not in args.preset:
        print(f"Warning: preset '{args.preset}' may not be a Strix Halo preset")

    print(f"\n{'='*55}")
    print(f"  Strix Halo Configuration — {args.preset}")
    if dry:
        print("  [DRY RUN — no changes will be made]")
    print(f"{'='*55}\n")

    # Kernel params
    params = preset.get("kernel_params", {})
    if params:
        print(f"Step 1: Kernel parameters ({build_kernel_cmdline(params)})")
        bootloader = detect_bootloader()
        print(f"  Detected bootloader: {bootloader}")
        if bootloader == "grub":
            write_grub_config(params, dry)
        elif bootloader == "systemd-boot":
            write_systemd_boot_config(params, dry)
        else:
            print(f"  ! Unknown bootloader. Add these to your kernel cmdline:")
            print(f"    {build_kernel_cmdline(params)}")
    else:
        print("Step 1: No kernel params for this preset")

    # Ollama override
    ollama_env = preset.get("ollama_env", {})
    if ollama_env:
        print(f"\nStep 2: Ollama systemd override")
        write_ollama_override(ollama_env, dry)
    else:
        print("\nStep 2: No Ollama env vars for this preset")

    # Summary
    print(f"\n{'='*55}")
    models = preset.get("models", [])
    print(f"  Fleet preset: {args.preset}")
    print(f"  Models to pull ({len(models)}):")
    for m in models:
        print(f"    ollama pull {m['name']}  ({m['vram_gb']}GB, {m['role']})")
    if not dry:
        print(f"\n  Next steps:")
        print(f"    1. Reboot to apply kernel params (required for VRAM allocation)")
        print(f"    2. Run: bash deploy/install.sh --preset {args.preset}")
    print(f"{'='*55}\n")


if __name__ == "__main__":
    main()
