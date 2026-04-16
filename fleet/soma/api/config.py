# PRIVACY: No conversation content. Secret endpoint returns NAMES ONLY, never values.
# See Doc 31 Part B.

import os
import re
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


def _secret_refs_from_env() -> list[dict[str, Any]]:
    names = set()
    for env_path in [FLEET_DIR / ".env", REPO_FLEET_DIR / ".env"]:
        if not env_path.exists():
            continue
        try:
            lines = env_path.read_text(encoding="utf-8", errors="replace").splitlines()
        except Exception:
            continue
        for line in lines:
            if "=" not in line or line.lstrip().startswith("#"):
                continue
            name = line.split("=", 1)[0].strip()
            if re.search(r"(TOKEN|SECRET|PASSWORD|PASS|API_KEY|KEY)$", name):
                names.add(name)
    return [{"name": name, "source": "env", "age_days": None} for name in sorted(names)]


def _secret_refs_from_registry() -> list[dict[str, Any]]:
    refs = []
    for path in [FLEET_DIR / "security" / "secrets_registry.yaml", REPO_FLEET_DIR / "security" / "secrets_registry.yaml"]:
        if not path.exists():
            continue
        data = _load_yaml(path)
        for item in data.get("secrets", []) if isinstance(data, dict) else []:
            if isinstance(item, dict) and item.get("name"):
                refs.append(
                    {
                        "name": item["name"],
                        "source": item.get("infisical_project", "infisical"),
                        "age_days": None,
                        "method": item.get("method", "manual"),
                    }
                )
        if refs:
            break
    return refs


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


@router.get("/secrets")
async def get_secrets_config():
    cfg = load_config()
    refs: dict[str, dict[str, Any]] = {}
    secret_refs = cfg.get("secrets", {}) if isinstance(cfg.get("secrets", {}), dict) else {}
    for name, ref in secret_refs.items():
        source = ref.get("source", "infisical") if isinstance(ref, dict) else "infisical"
        refs[name] = {"name": name, "source": source, "age_days": None}
    for item in _secret_refs_from_registry() + _secret_refs_from_env():
        refs.setdefault(item["name"], item)
    return sorted(refs.values(), key=lambda row: row["name"].lower())


@router.get("/synapse")
async def get_synapse_config():
    cfg = load_config()
    synapse = cfg.get("synapse", {}) if isinstance(cfg.get("synapse", {}), dict) else {}
    return {
        "enabled": bool(synapse.get("enabled", False)),
        "routing_rules": synapse.get("rules", synapse.get("routing_rules", [])),
        "channels": synapse.get("channels", []),
        "strength": synapse.get("strength", "gentle"),
        "max_messages_per_day": synapse.get("max_messages_per_day"),
    }


@router.get("/security")
async def get_security_config():
    cfg = load_config()
    security = cfg.get("security", {}) if isinstance(cfg.get("security", {}), dict) else {}
    return {
        "auth": _safe_section(security.get("auth", {})),
        "rate_limits": _safe_section(security.get("rate_limits", {})),
        "pii_gate": _safe_section(security.get("pii_gate", {})),
    }
