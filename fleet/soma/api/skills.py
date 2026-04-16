# PRIVACY: No conversation content, no secret values. See Doc 31 Part B.

import ast
import json
import os
import urllib.request
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from fastapi import APIRouter

router = APIRouter(prefix="/api/skills", tags=["skills"])

FLEET_DIR = Path(os.environ.get("LUMINA_FLEET_DIR", "/opt/lumina-fleet"))
REPO_FLEET_DIR = Path(__file__).resolve().parents[2]
SKILLS_DIRS = [FLEET_DIR / "skills", REPO_FLEET_DIR / "skills"]


def _read_yaml(text: str) -> dict[str, Any]:
    try:
        import yaml

        return yaml.safe_load(text) or {}
    except Exception:
        return {}


def _frontmatter(path: Path) -> dict[str, Any]:
    try:
        content = path.read_text(encoding="utf-8", errors="replace")
    except Exception:
        return {}
    if content.startswith("---"):
        parts = content.split("---", 2)
        if len(parts) >= 3:
            data = _read_yaml(parts[1])
            data["_body"] = parts[2].strip()
            return data
    lines = [line.strip() for line in content.splitlines() if line.strip()]
    return {"description": lines[0] if lines else ""}


def _skill_record(path: Path, status: str, source: str) -> dict[str, Any] | None:
    metadata_path = path / "SKILL.md" if path.is_dir() else path
    if not metadata_path.exists() or metadata_path.name == "README.md":
        return None
    if metadata_path.suffix not in {".md", ".yaml", ".yml", ".json", ".py"}:
        return None

    if metadata_path.suffix == ".json":
        try:
            data = json.loads(metadata_path.read_text(encoding="utf-8"))
        except Exception:
            data = {}
    elif metadata_path.suffix == ".py":
        try:
            module = ast.parse(metadata_path.read_text(encoding="utf-8", errors="replace"))
            data = {"description": ast.get_docstring(module) or ""}
        except Exception:
            data = {}
    else:
        data = _frontmatter(metadata_path)

    name = data.get("name") or (path.stem if path.is_file() else path.name)
    description = data.get("description") or data.get("summary") or ""
    stats_path = path / "stats.json" if path.is_dir() else path.with_suffix(".stats.json")
    stats: dict[str, Any] = {}
    if stats_path.exists():
        try:
            stats = json.loads(stats_path.read_text(encoding="utf-8"))
        except Exception:
            stats = {}

    proposed_at = data.get("proposed_at") or data.get("created_at")
    if not proposed_at and status == "proposed":
        try:
            proposed_at = datetime.fromtimestamp(metadata_path.stat().st_mtime, tz=timezone.utc).isoformat()
        except Exception:
            proposed_at = None

    return {
        "name": str(name),
        "source": source,
        "version": str(data.get("version") or ""),
        "status": status,
        "description": str(description),
        "proposed_at": proposed_at,
        "rationale": data.get("rationale") or data.get("why") or "",
        "usage_count": stats.get("usage_count", 0),
        "last_success": stats.get("last_success"),
    }


def _scan_status(status: str) -> list[dict[str, Any]]:
    found: dict[str, dict[str, Any]] = {}
    for skills_dir in SKILLS_DIRS:
        status_dir = skills_dir / status
        candidates: list[Path] = []
        if status_dir.exists():
            candidates.extend(sorted(status_dir.iterdir()))
        elif status == "active" and skills_dir.exists():
            candidates.extend(
                item
                for item in sorted(skills_dir.iterdir())
                if item.name not in {"proposed", "disabled", "archived"}
            )

        source = "deployed" if str(skills_dir).startswith(str(FLEET_DIR)) else "repo"
        for item in candidates:
            record = _skill_record(item, status, source)
            if record:
                found.setdefault(record["name"], record)
    return sorted(found.values(), key=lambda row: row["name"].lower())


def _agentskills_registry() -> list[dict[str, Any]]:
    url = os.environ.get("AGENTSKILLS_REGISTRY_URL", "https://agentskills.io/api/skills")
    try:
        with urllib.request.urlopen(url, timeout=2) as response:
            data = json.loads(response.read().decode("utf-8"))
    except Exception:
        return []

    items = data.get("skills", data if isinstance(data, list) else [])
    registry = []
    for item in items[:100]:
        if not isinstance(item, dict):
            continue
        registry.append(
            {
                "name": str(item.get("name") or item.get("slug") or "unnamed"),
                "source": "agentskills.io",
                "version": str(item.get("version") or ""),
                "status": "available",
                "description": str(item.get("description") or ""),
            }
        )
    return registry


@router.get("/active")
async def get_active_skills():
    skills = _scan_status("active")
    registry = _agentskills_registry()
    return {
        "ok": True,
        "count": len(skills),
        "skills": skills,
        "registry_count": len(registry),
        "registry": registry,
    }


@router.get("/proposed")
async def get_proposed_skills():
    skills = _scan_status("proposed")
    return {
        "ok": True,
        "count": len(skills),
        "skills": skills,
        "note": "" if skills else "No proposed skills found.",
    }
