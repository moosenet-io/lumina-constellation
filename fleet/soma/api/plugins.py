# PRIVACY: No conversation content, no secret values. See Doc 31 Part B.

import ast
import json
import os
import re
import shlex
import subprocess
from pathlib import Path
from typing import Any

from fastapi import APIRouter

router = APIRouter(prefix="/api/plugins", tags=["plugins"])

FLEET_DIR = Path(os.environ.get("LUMINA_FLEET_DIR", "/opt/lumina-fleet"))
REPO_ROOT = Path(__file__).resolve().parents[3]
LOCAL_PLUGIN_DIRS = [
    FLEET_DIR / "plugins",
    FLEET_DIR / "vector" / "plugins",
    REPO_ROOT / "terminus" / "plugins",
    REPO_ROOT / "fleet" / "vector" / "plugins",
]


def _description(path: Path) -> str:
    try:
        module = ast.parse(path.read_text(encoding="utf-8", errors="replace"))
        return ast.get_docstring(module) or ""
    except Exception:
        return ""


def _tools_count(path: Path) -> int:
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except Exception:
        return 0
    decorated = len(re.findall(r"@(?:mcp\.)?tool", text))
    tools_list = re.search(r"\bTOOLS\s*=\s*\[([^\]]*)\]", text, re.S)
    if tools_list:
        listed = len(re.findall(r"\b[a-zA-Z_][a-zA-Z0-9_]*\b", tools_list.group(1)))
    else:
        listed = 0
    return max(decorated, listed)


def _category(path: Path) -> str:
    parts = set(path.parts)
    if "vector" in parts:
        return "vector"
    if "terminus" in parts or "ai-mcp" in parts:
        return "terminus"
    return "fleet"


def _plugin_record(path: Path, source: str, enabled: bool = True) -> dict[str, Any]:
    return {
        "name": path.stem,
        "enabled": enabled,
        "category": _category(path),
        "tools_count": _tools_count(path),
        "tool_count": _tools_count(path),
        "description": _description(path),
        "source": source,
        "filename": path.name,
    }


def _local_plugins() -> list[dict[str, Any]]:
    plugins: dict[str, dict[str, Any]] = {}
    for directory in LOCAL_PLUGIN_DIRS:
        if not directory.exists():
            continue
        source = "deployed" if str(directory).startswith(str(FLEET_DIR)) else "repo"
        for path in sorted(directory.glob("*.py")):
            if path.name.startswith("_"):
                continue
            record = _plugin_record(path, source)
            plugins.setdefault(record["name"], record)
    return sorted(plugins.values(), key=lambda row: row["name"].lower())


def _terminus_plugins() -> list[dict[str, Any]]:
    pvm_host = os.environ.get("PVM_SSH_HOST", "")
    terminus_ct = os.environ.get("TERMINUS_CT", "")
    if not pvm_host or not terminus_ct:
        return []

    remote_code = """
import ast, json, os, re
base = '/opt/ai-mcp/plugins'
items = []
files = [] if not os.path.isdir(base) else sorted(
    f for f in os.listdir(base) if f.endswith('.py') and not f.startswith('_')
)
for filename in files:
    path = os.path.join(base, filename)
    text = open(path, encoding='utf-8', errors='replace').read()
    match = re.search(r'\\bTOOLS\\s*=\\s*\\[([^\\]]*)\\]', text, re.S)
    listed = len(re.findall(r'\\b[a-zA-Z_][a-zA-Z0-9_]*\\b', match.group(1))) if match else 0
    decorated = len(re.findall(r'@(?:mcp\\.)?tool', text))
    items.append({
        'name': filename[:-3],
        'enabled': True,
        'category': 'terminus',
        'tools_count': max(decorated, listed),
        'tool_count': max(decorated, listed),
        'description': ast.get_docstring(ast.parse(text)) or '',
        'source': 'terminus',
        'filename': filename,
    })
print(json.dumps(items))
"""
    command = f"python3 -c {shlex.quote(remote_code)}"
    try:
        result = subprocess.run(
            ["ssh", pvm_host, f"pct exec {terminus_ct} -- {command}"],
            capture_output=True,
            text=True,
            timeout=15,
        )
        if result.returncode == 0 and result.stdout.strip():
            return json.loads(result.stdout)
    except Exception:
        return []
    return []


@router.get("/installed")
async def get_installed_plugins():
    plugins: dict[str, dict[str, Any]] = {}
    for item in _local_plugins() + _terminus_plugins():
        plugins.setdefault(item["name"], item)
    rows = sorted(plugins.values(), key=lambda row: row["name"].lower())
    return {"ok": True, "count": len(rows), "plugins": rows}


@router.get("/available")
async def get_available_plugins():
    registry_path = FLEET_DIR / "plugins" / "registry.json"
    if not registry_path.exists():
        registry_path = REPO_ROOT / "fleet" / "plugins" / "registry.json"
    try:
        data = json.loads(registry_path.read_text(encoding="utf-8")) if registry_path.exists() else []
    except Exception:
        data = []
    plugins = data.get("plugins", []) if isinstance(data, dict) else (data if isinstance(data, list) else [])
    return {"ok": True, "count": len(plugins), "plugins": plugins}
