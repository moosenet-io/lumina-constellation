# PRIVACY: No conversation content. Secret endpoint returns NAMES ONLY, never values.
# See Doc 31 Part B.

import os
from pathlib import Path
from typing import Any

from fastapi import APIRouter

router = APIRouter(prefix="/api/config", tags=["config"])

FLEET_DIR = Path(os.environ.get("LUMINA_FLEET_DIR", "/opt/lumina-fleet"))
REPO_FLEET_DIR = Path(__file__).resolve().parents[2]
CONFIG_CANDIDATES = [
    FLEET_DIR / "constellation.yaml",
    REPO_FLEET_DIR / "constellation.yaml",
]


def _load_yaml(path: Path) -> dict[str, Any]:
    try:
        import yaml

        return yaml.safe_load(path.read_text(encoding="utf-8")) or {}
    except Exception:
        return {}


def load_config() -> dict[str, Any]:
    for path in CONFIG_CANDIDATES:
        if path.exists():
            return _load_yaml(path)
    return {}


def _display_name(data: dict[str, Any], fallback: str) -> str:
    return str(data.get("display_name") or data.get("name") or fallback)


def _safe_value(key: str, value: Any) -> Any:
    sensitive_fragments = ("token", "secret", "password", "api_key", "apikey", "webhook")
    if any(fragment in key.lower() for fragment in sensitive_fragments):
        return bool(value)
    if isinstance(value, dict):
        return {k: _safe_value(k, v) for k, v in value.items()}
    if isinstance(value, list):
        return [_safe_value(key, item) for item in value]
    return value


def _safe_section(value: Any) -> Any:
    if isinstance(value, dict):
        return {k: _safe_value(k, v) for k, v in value.items()}
    return value if value is not None else {}


@router.get("/general")
async def get_general_config():
    cfg = load_config()
    lead = cfg.get("lead_agent", {})
    constellation = cfg.get("constellation", {})
    return {
        "assistant_name": _display_name(lead, "Lumina"),
        "timezone": cfg.get("timezone") or os.environ.get("LUMINA_TIMEZONE", "UTC"),
        "language": cfg.get("language", "en"),
        "operator_id": cfg.get("operator", {}).get("id") or os.environ.get("LUMINA_OPERATOR_ID", ""),
        "constellation_name": constellation.get("name", ""),
        "tagline": constellation.get("tagline", ""),
    }


@router.get("/modules")
async def get_modules_config():
    cfg = load_config()
    modules = cfg.get("modules", {})
    if not isinstance(modules, dict):
        return []
    return [
        {
            "name": name,
            "display_name": _display_name(settings if isinstance(settings, dict) else {}, name),
            "enabled": bool(settings.get("enabled", True)) if isinstance(settings, dict) else True,
            "description": settings.get("description", "") if isinstance(settings, dict) else "",
        }
        for name, settings in sorted(modules.items())
    ]


@router.get("/inference")
async def get_inference_config():
    cfg = load_config()
    inference = cfg.get("inference", {}) if isinstance(cfg.get("inference", {}), dict) else {}
    return {
        "fleet_preset": inference.get("preset") or inference.get("fleet_preset") or os.environ.get("LUMINA_MODEL_PRESET", ""),
        "models": inference.get("models", []),
        "routing": inference.get("routing", {}),
        "providers": _safe_section(inference.get("providers", {})),
    }


@router.get("/channels")
async def get_channels_config():
    cfg = load_config()
    channels = cfg.get("channels", {}) if isinstance(cfg.get("channels", {}), dict) else {}
    return {
        "matrix": _safe_section(channels.get("matrix", {})),
        "email": _safe_section(channels.get("email", {})),
        "webhook": _safe_section(channels.get("webhook", {})),
    }
