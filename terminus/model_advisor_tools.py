"""
model_advisor_tools.py — Model Advisor MCP tools. (DT.7)
Runs on Terminus. Recommends model fleets based on VRAM and use case.
"""

import json
import os
import sys
import urllib.request
import urllib.error
from pathlib import Path
from typing import Optional

_FLEET_DIR = Path(os.environ.get("FLEET_DIR", "/opt/lumina-fleet"))
_DEPLOY_DIR = Path(__file__).parent.parent / "deploy"

try:
    import yaml
    _HAS_YAML = True
except ImportError:
    _HAS_YAML = False


def _load_presets() -> dict:
    presets_path = _DEPLOY_DIR / "model_presets.yaml"
    if not presets_path.exists() or not _HAS_YAML:
        return {}
    return yaml.safe_load(presets_path.read_text())


def _load_matrix() -> dict:
    matrix_path = _DEPLOY_DIR / "model_matrix.yaml"
    if not matrix_path.exists() or not _HAS_YAML:
        return {}
    return yaml.safe_load(matrix_path.read_text())


def _pick_preset_for_vram(vram_gb: float, platform: str = "generic") -> str:
    """Select the best preset for given VRAM."""
    presets = _load_presets()
    if not presets:
        return "cpu_only"

    candidates = []
    for name, preset in presets.items():
        alloc = preset.get("vram_allocation_gb", 0)
        if alloc <= vram_gb:
            # Prefer presets that match the platform hint
            platform_match = platform in name.lower() or platform == "generic"
            candidates.append((alloc, platform_match, name))

    if not candidates:
        return "cpu_only"

    # Sort by allocation (highest that fits) with platform preference
    candidates.sort(key=lambda x: (x[0], x[1]), reverse=True)
    return candidates[0][2]


def register_model_advisor_tools(mcp):

    @mcp.tool()
    def model_advisor_recommend(
        vram_gb: float,
        use_case: str = "general",
        platform: str = "generic",
    ) -> dict:
        """Recommend a model fleet based on available VRAM and use case.

        Args:
            vram_gb: Available VRAM in GB (or unified memory for Apple Silicon / Strix Halo)
            use_case: 'general', 'coding', 'reasoning', 'fast', or 'research'
            platform: 'strix_halo', 'apple_silicon', 'nvidia', 'amd', or 'generic'

        Returns:
            {preset_name, models, total_vram_gb, recommended_ollama_pull_commands}
        """
        presets = _load_presets()
        matrix = _load_matrix()

        preset_name = _pick_preset_for_vram(vram_gb, platform)
        preset = presets.get(preset_name, {})
        models = preset.get("models", [])

        # Apply use-case filter — boost relevant role models
        use_case_roles = {
            "coding":    ["code", "primary"],
            "reasoning": ["reasoning", "primary"],
            "fast":      ["fast", "primary"],
            "research":  ["reasoning", "primary", "fast"],
            "general":   ["primary", "fast", "code"],
        }
        preferred_roles = use_case_roles.get(use_case, ["primary", "fast"])
        sorted_models = sorted(
            models,
            key=lambda m: preferred_roles.index(m["role"]) if m["role"] in preferred_roles else 99
        )

        pull_commands = [f"ollama pull {m['name']}" for m in sorted_models]
        total_vram = sum(m.get("vram_gb", 0) for m in sorted_models)

        return {
            "ok": True,
            "preset_name": preset_name,
            "models": sorted_models,
            "total_model_vram_gb": total_vram,
            "vram_available_gb": vram_gb,
            "headroom_gb": round(vram_gb - total_vram, 1),
            "ollama_pull_commands": pull_commands,
            "ollama_env": preset.get("ollama_env", {}),
            "note": f"Use case: {use_case}. All models fit simultaneously with {round(vram_gb - total_vram, 1)}GB headroom for KV cache.",
        }

    @mcp.tool()
    def model_advisor_check_fit(
        model_name: str,
        quant: str = "Q4_K_M",
        vram_gb: float = 24.0,
    ) -> dict:
        """Check if a specific model+quantization fits in available VRAM.

        Args:
            model_name: Ollama model name (e.g., 'qwen3.5:35b-a3b')
            quant: Quantization level (e.g., 'Q4_K_M', 'Q8_0', 'F16')
            vram_gb: Available VRAM in GB

        Returns:
            {fits, model_vram_gb, headroom_gb, recommendation}
        """
        matrix = _load_matrix()

        # Normalize model name for lookup
        lookup_key = model_name.replace("/", ".")
        model_info = matrix.get(lookup_key) or matrix.get(model_name)

        if not model_info:
            return {
                "ok": False,
                "error": f"Model '{model_name}' not in matrix. Estimate: ~6GB per 7B params at Q4_K_M.",
                "fits": None,
            }

        quant_info = model_info.get("quants", {}).get(quant)
        if not quant_info:
            available_quants = list(model_info.get("quants", {}).keys())
            return {
                "ok": False,
                "error": f"Quantization '{quant}' not available. Options: {available_quants}",
                "fits": None,
            }

        model_vram = quant_info["vram_gb"]
        fits = model_vram <= vram_gb
        headroom = round(vram_gb - model_vram, 1)

        rec = ""
        if fits:
            rec = f"✓ Fits with {headroom}GB headroom for KV cache"
        else:
            shortage = round(model_vram - vram_gb, 1)
            # Find best alternative
            available_quants = {q: info["vram_gb"] for q, info in model_info.get("quants", {}).items()
                                if info["vram_gb"] <= vram_gb}
            if available_quants:
                best_alt = max(available_quants, key=available_quants.get)
                rec = f"✗ Doesn't fit ({shortage}GB short). Try {best_alt} ({available_quants[best_alt]}GB)."
            else:
                rec = f"✗ No quant fits in {vram_gb}GB. Minimum: {min(q['vram_gb'] for q in model_info.get('quants', {}).values())}GB"

        return {
            "ok": True,
            "model": model_name,
            "quant": quant,
            "model_vram_gb": model_vram,
            "vram_available_gb": vram_gb,
            "fits": fits,
            "headroom_gb": headroom,
            "recommendation": rec,
            "quality_penalty": quant_info.get("quality_penalty", 0),
        }

    @mcp.tool()
    def model_advisor_query_ollama(
        ollama_host: str = "",
        vram_gb: float = 0,
    ) -> dict:
        """Query an Ollama instance to see what's installed vs what fits.

        Args:
            ollama_host: Ollama API URL (default: OLLAMA_HOST env var or http://localhost:11434)
            vram_gb: Available VRAM for fit check (0 = skip fit check)

        Returns:
            {installed_models, missing_recommended, fits_summary}
        """
        host = ollama_host or os.environ.get("OLLAMA_HOST", "http://localhost:11434")
        host = host.rstrip("/")

        try:
            req = urllib.request.Request(f"{host}/api/tags",
                                         headers={"Content-Type": "application/json"})
            with urllib.request.urlopen(req, timeout=5) as r:
                data = json.loads(r.read())
        except Exception as e:
            return {"ok": False, "error": f"Cannot reach Ollama at {host}: {e}"}

        installed = [m["name"] for m in data.get("models", [])]
        matrix = _load_matrix()

        # Cross-reference with matrix
        summary = []
        for model_name, info in matrix.items():
            ollama_name = info.get("ollama_name", model_name)
            is_installed = any(ollama_name in m or m in ollama_name for m in installed)
            entry = {
                "model": ollama_name,
                "installed": is_installed,
                "quality": info.get("quality", "?"),
                "best_for": info.get("best_for", [])[:3],
            }
            if vram_gb > 0:
                q4 = info.get("quants", {}).get("Q4_K_M", {})
                entry["fits_q4km"] = q4.get("vram_gb", 999) <= vram_gb
                entry["vram_q4km"] = q4.get("vram_gb", "?")
            summary.append(entry)

        return {
            "ok": True,
            "ollama_host": host,
            "installed_count": len(installed),
            "installed_models": installed,
            "matrix_summary": summary,
        }
