#!/usr/bin/env python3
"""
generate_litellm_config.py — Generate LiteLLM config from fleet preset. (DT.13)

Reads a model preset, maps roles to LiteLLM model aliases, and outputs
a litellm_config.yaml ready for deployment.

Usage:
    python3 deploy/generate_litellm_config.py --preset strix_halo_128
    python3 deploy/generate_litellm_config.py --preset strix_halo_128 --output /config/litellm_config.yaml
    python3 deploy/generate_litellm_config.py --preset auto   # detect hardware
"""

import argparse
import os
import sys
from pathlib import Path

try:
    import yaml
except ImportError:
    print("Error: pyyaml not installed. Run: pip install pyyaml")
    sys.exit(1)

PRESETS_FILE = Path(__file__).parent / "model_presets.yaml"

# Role → LiteLLM alias mapping
ROLE_ALIASES = {
    "frontier":  ["Lumina-Frontier", "Lumina-Reasoning", "default"],
    "reasoning": ["Lumina-Reasoning", "Mr-Wizard", "default"],
    "primary":   ["Lumina-Claw", "default", "Lumina"],
    "code":      ["Lumina-Code", "Lumina-Claw"],
    "fast":      ["Lumina-Fast", "Lumina-Scout"],
    "embeddings":["Lumina-Embed", "Lumina-Fast"],
}


def load_preset(name: str) -> dict:
    if not PRESETS_FILE.exists():
        print(f"Error: {PRESETS_FILE} not found")
        sys.exit(1)
    data = yaml.safe_load(PRESETS_FILE.read_text())
    if name == "auto":
        try:
            sys.path.insert(0, str(Path(__file__).parent))
            from detect_hardware import detect
            hw = detect()
            name = hw.get("fleet_preset", "cpu_only")
            print(f"Detected preset: {name}")
        except Exception as e:
            print(f"Hardware detection failed: {e}. Using cpu_only.")
            name = "cpu_only"
    if name not in data:
        print(f"Error: preset '{name}' not found. Available: {', '.join(data.keys())}")
        sys.exit(1)
    return name, data[name]


def generate_config(preset_name: str, preset: dict, ollama_host: str) -> dict:
    """Generate LiteLLM config dict from preset."""
    models = preset.get("models", [])
    ollama_env = preset.get("ollama_env", {})

    model_list = []
    model_aliases = {}

    for model in models:
        role = model.get("role", "primary")
        model_name = model.get("name", "")
        quant = model.get("quant", "Q4_K_M")

        ollama_model_string = f"ollama_chat/{model_name}"
        litellm_model_id = model_name.replace(":", "-").replace(".", "_")

        entry = {
            "model_name": litellm_model_id,
            "litellm_params": {
                "model": ollama_model_string,
                "api_base": ollama_host,
            }
        }
        model_list.append(entry)

        # Register role aliases
        aliases = ROLE_ALIASES.get(role, [role])
        for alias in aliases:
            if alias not in model_aliases:
                model_aliases[alias] = litellm_model_id

    # Always include cloud fallback entries (commented approach via environment)
    # OpenRouter fallback (optional — only active if OPENROUTER_API_KEY is set)
    if os.environ.get("OPENROUTER_API_KEY"):
        model_list.append({
            "model_name": "openrouter-sonnet",
            "litellm_params": {
                "model": "openrouter/anthropic/claude-sonnet-4-6",
                "api_key": "os.environ/OPENROUTER_API_KEY",
            }
        })
        model_list.append({
            "model_name": "openrouter-free",
            "litellm_params": {
                "model": "openrouter/google/gemini-flash-1.5",
                "api_key": "os.environ/OPENROUTER_API_KEY",
            }
        })
        model_aliases.setdefault("cloud-fallback", "openrouter-free")

    config = {
        "model_list": model_list,
        "litellm_settings": {
            "drop_params": True,
            "model_alias_map": model_aliases,
        },
        "general_settings": {
            "master_key": "os.environ/LITELLM_MASTER_KEY",
            "database_url": "os.environ/DATABASE_URL",
        },
        "router_settings": {
            "fallbacks": [
                {model_aliases.get("default", "cpu_only"): ["openrouter-free"]}
                if os.environ.get("OPENROUTER_API_KEY") else {}
            ],
            "num_retries": 1,
        },
    }

    # Add a comment header via a special key (pyyaml will ignore unknown keys)
    return config, model_aliases


def main():
    parser = argparse.ArgumentParser(description="Generate LiteLLM config from fleet preset")
    parser.add_argument("--preset", default="auto",
                        help="Fleet preset name or 'auto' to detect hardware (default: auto)")
    parser.add_argument("--output", default="",
                        help="Output file path (default: stdout)")
    parser.add_argument("--ollama-host", default="http://host.docker.internal:11434",
                        help="Ollama API URL (default: http://host.docker.internal:11434)")
    args = parser.parse_args()

    preset_name, preset = load_preset(args.preset)
    config, aliases = generate_config(preset_name, preset, args.ollama_host)

    # Add metadata comment at top
    header = f"""# LiteLLM Configuration — Generated by Lumina Installer
# Fleet preset: {preset_name}
# Ollama host: {args.ollama_host}
# Generated: auto
#
# Model aliases (use these in LiteLLM API calls):
"""
    for alias, model in aliases.items():
        header += f"#   {alias} → {model}\n"
    header += "#\n"

    output_yaml = header + yaml.dump(config, default_flow_style=False, allow_unicode=True)

    if args.output:
        Path(args.output).parent.mkdir(parents=True, exist_ok=True)
        Path(args.output).write_text(output_yaml)
        print(f"Written to {args.output}")
        print(f"Models: {len(config['model_list'])}")
        print(f"Aliases: {', '.join(aliases.keys())}")
    else:
        print(output_yaml)


if __name__ == "__main__":
    main()
