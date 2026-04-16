#!/usr/bin/env python3
"""
detect_hardware.py — Hardware detection for Lumina installer. (DT.1)

Detects CPU, GPU, RAM and recommends a deployment mode and fleet preset.

Usage:
    python3 deploy/detect_hardware.py              # JSON output
    python3 deploy/detect_hardware.py --human      # Human-readable
    python3 deploy/detect_hardware.py --preset     # Just the preset name
"""

import json
import os
import platform
import re
import shutil
import subprocess
import sys
from pathlib import Path


def _run(cmd: list, timeout: int = 5) -> str:
    """Run command, return stdout. Empty string on failure."""
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return r.stdout.strip()
    except Exception:
        return ""


def _detect_linux() -> dict:
    info = {}

    # CPU model
    cpu_info = _run(["grep", "-m1", "model name", "/proc/cpuinfo"])
    if cpu_info:
        info["chip_model"] = re.sub(r"model name\s*:\s*", "", cpu_info).strip()
    else:
        info["chip_model"] = _run(["lscpu"]).split("Model name:")[1].split("\n")[0].strip() if "Model name:" in _run(["lscpu"]) else "Unknown CPU"

    # RAM
    meminfo = Path("/proc/meminfo").read_text() if Path("/proc/meminfo").exists() else ""
    m = re.search(r"MemTotal:\s+(\d+)\s+kB", meminfo)
    info["total_ram_gb"] = round(int(m.group(1)) / 1024 / 1024) if m else 0

    # GPU detection
    info["gpu_compute"] = "none"
    info["estimated_vram_gb"] = 0
    info["platform"] = "cpu_only"

    # Try AMD (Strix Halo / discrete)
    lspci = _run(["lspci"])
    if "AMD" in lspci and any(x in lspci for x in ["Radeon", "gfx", "Display"]):
        rocm = _run(["rocm-smi", "--showmeminfo", "vram", "--json"])
        if rocm:
            try:
                d = json.loads(rocm)
                vram = 0
                for card in d.values():
                    if isinstance(card, dict):
                        vram_str = card.get("VRAM Total Memory (B)", "0")
                        vram = max(vram, int(vram_str) // (1024**3))
                info["estimated_vram_gb"] = vram
                info["gpu_compute"] = "ROCm"
            except Exception:
                info["estimated_vram_gb"] = 0

        # Detect Strix Halo (unified memory — VRAM is a slice of RAM)
        chip = info.get("chip_model", "").lower()
        if "ai max" in chip or "strix" in chip or "890m" in chip:
            info["platform"] = "strix_halo"
            # On unified memory, VRAM ~ 75% of RAM
            info["estimated_vram_gb"] = round(info["total_ram_gb"] * 0.75)
            info["gpu_compute"] = "ROCm (unified memory)"
        else:
            info["platform"] = "amd_discrete"

    # Try NVIDIA
    elif shutil.which("nvidia-smi"):
        nvout = _run(["nvidia-smi", "--query-gpu=name,memory.total", "--format=csv,noheader,nounits"])
        if nvout:
            parts = nvout.split(",")
            info["chip_model"] = parts[0].strip() if len(parts) > 0 else info.get("chip_model", "")
            info["estimated_vram_gb"] = round(int(parts[1].strip()) / 1024) if len(parts) > 1 else 0
            info["gpu_compute"] = "CUDA"
            info["platform"] = "nvidia_discrete"

    return info


def _detect_macos() -> dict:
    info = {}

    sp = _run(["system_profiler", "SPHardwareDataType"])

    # Chip model
    m = re.search(r"Chip:\s+(.+)", sp)
    info["chip_model"] = m.group(1).strip() if m else "Unknown Apple chip"

    # RAM
    m = re.search(r"Memory:\s+(\d+)\s+GB", sp)
    info["total_ram_gb"] = int(m.group(1)) if m else 0

    # Apple Silicon — unified memory
    chip = info["chip_model"].lower()
    if any(x in chip for x in ["m1", "m2", "m3", "m4"]):
        info["platform"] = "apple_silicon"
        info["gpu_compute"] = "Metal"
        # Unified memory — all RAM is usable as VRAM
        info["estimated_vram_gb"] = info["total_ram_gb"]
    else:
        info["platform"] = "cpu_only"
        info["gpu_compute"] = "none"
        info["estimated_vram_gb"] = 0

    return info


def _pick_preset(info: dict) -> str:
    """Map hardware info to a model preset name."""
    platform = info.get("platform", "cpu_only")
    vram = info.get("estimated_vram_gb", 0)
    ram = info.get("total_ram_gb", 0)

    if platform == "strix_halo":
        if ram >= 128: return "strix_halo_128"
        if ram >= 96:  return "strix_halo_96"
        return "strix_halo_64"

    elif platform == "apple_silicon":
        if ram >= 192: return "apple_silicon_192"
        if ram >= 128: return "apple_silicon_128"
        if ram >= 96:  return "apple_silicon_96"
        if ram >= 64:  return "apple_silicon_64"
        return "apple_silicon_32"

    elif platform == "nvidia_discrete":
        if vram >= 24: return "discrete_24gb"
        if vram >= 16: return "discrete_16gb"
        if vram >= 12: return "discrete_12gb"
        if vram >= 8:  return "discrete_8gb"
        return "discrete_8gb"

    elif platform == "amd_discrete":
        if vram >= 24: return "discrete_24gb"
        if vram >= 16: return "discrete_16gb"
        return "discrete_8gb"

    return "cpu_only"


def _pick_mode(info: dict) -> int:
    """1=all-in-one, 2=split inference, 3=distributed"""
    vram = info.get("estimated_vram_gb", 0)
    if vram >= 64: return 1   # Fits everything locally
    if vram >= 16: return 1   # Fits daily drivers locally
    if vram > 0:   return 1   # Partial local with cloud fallback
    return 2                   # CPU only → split inference


def detect() -> dict:
    """Run hardware detection. Returns structured info dict."""
    system = platform.system()

    if system == "Darwin":
        info = _detect_macos()
    elif system == "Linux":
        info = _detect_linux()
    else:
        info = {
            "platform": "cpu_only",
            "chip_model": platform.processor() or "Unknown",
            "gpu_compute": "none",
            "estimated_vram_gb": 0,
            "total_ram_gb": 0,
        }

    # Add derived fields
    info["os"] = system
    info["fleet_preset"] = _pick_preset(info)
    info["recommended_mode"] = _pick_mode(info)

    return info


def main():
    info = detect()

    if "--preset" in sys.argv:
        print(info["fleet_preset"])
        return

    if "--human" in sys.argv:
        print(f"\n{'='*50}")
        print(f"  Lumina Hardware Detection")
        print(f"{'='*50}")
        print(f"  Platform:     {info.get('platform', '?')}")
        print(f"  Chip:         {info.get('chip_model', '?')}")
        print(f"  RAM:          {info.get('total_ram_gb', '?')} GB")
        print(f"  GPU compute:  {info.get('gpu_compute', 'none')}")
        print(f"  Est. VRAM:    {info.get('estimated_vram_gb', 0)} GB")
        print(f"  Fleet preset: {info.get('fleet_preset', '?')}")
        mode_names = {1: "All-in-one (recommended)", 2: "Split inference", 3: "Distributed"}
        print(f"  Deploy mode:  {mode_names.get(info.get('recommended_mode', 1), '?')}")
        print(f"{'='*50}\n")
    else:
        print(json.dumps(info, indent=2))


if __name__ == "__main__":
    main()
