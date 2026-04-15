#!/usr/bin/env python3
"""
Soma — Lumina Constellation Admin Panel API
FastAPI backend, port 8082. Reads/writes module config and system state.
Requires X-Soma-Key header (set via SOMA_SECRET_KEY env var).
"""

import os, json, subprocess
from pathlib import Path
from datetime import datetime, timezone
from typing import Optional
from concurrent.futures import ThreadPoolExecutor, as_completed, TimeoutError as FuturesTimeout

# IronClaw gateway token (from DB: channels.gateway_auth_token)
IRONCLAW_URL = os.environ.get("IRONCLAW_URL", "")
IRONCLAW_TOKEN = os.environ.get("IRONCLAW_GATEWAY_TOKEN", "")

# ── Cache layer (SP.1) ───────────────────────────────────────────────────────
import sys as _sys
_sys.path.insert(0, '/opt/lumina-fleet/soma')
try:
    from cache import SomaCache
    from refreshers.refresh_status import refresh_status as _refresh_status_fn
    _CACHE_AVAILABLE = True
except ImportError:
    _CACHE_AVAILABLE = False
    class SomaCache:
        def get(self, k): return None
        def set(self, k, v): pass
        def get_or_fetch(self, k): return None
        def invalidate(self, k=None): pass
        def status_report(self): return {}
        def register(self, *a, **kw): pass
        def start_background_refresh(self, app): pass

from fastapi import FastAPI, HTTPException, Header, Body, Request, Depends, Form
from fastapi.responses import JSONResponse, HTMLResponse, FileResponse, RedirectResponse
from fastapi.staticfiles import StaticFiles
from fastapi.middleware.cors import CORSMiddleware
from starlette.middleware.base import BaseHTTPMiddleware
from fastapi.templating import Jinja2Templates
from pydantic import BaseModel

app = FastAPI(title="Soma Admin API", version="1.0")
app.add_middleware(CORSMiddleware, allow_origins=["*"], allow_methods=["*"], allow_headers=["*"])

SOMA_KEY = os.environ.get("SOMA_SECRET_KEY", "soma-dev-key")
FLEET_DIR = Path("/opt/lumina-fleet")
CONSTELLATION_YAML = FLEET_DIR / "constellation.yaml"

# ── Auth module (SP.2) ────────────────────────────────────────────────────────
try:
    _sys.path.insert(0, str(Path("/opt/lumina-fleet/soma")))
    from auth import SomaAuth, add_auth_routes, require_auth, optional_auth, _soma_auth as _auth_module
    _AUTH_AVAILABLE = True
except ImportError:
    _AUTH_AVAILABLE = False
    _auth_module = None
    def require_auth(x_soma_key: str = Header(default="")):
        if SOMA_KEY and x_soma_key != SOMA_KEY:
            raise HTTPException(status_code=401, detail="Unauthorized")
        return {"username": "admin", "role": "admin", "auth_method": "header"}
    def optional_auth(x_soma_key: str = Header(default="")):
        return {"username": "admin", "role": "admin"} if (not SOMA_KEY or x_soma_key == SOMA_KEY) else None

# Initialize cache with admin token for HMAC integrity
_soma_cache = SomaCache(admin_token=SOMA_KEY)
if _CACHE_AVAILABLE:
    _soma_cache.register('/api/status', _refresh_status_fn, ttl=10)
    _soma_cache.start_background_refresh(app)

def _auth(x_soma_key: str = ""):
    """Legacy auth check for existing endpoints. New endpoints use require_auth Depends."""
    if SOMA_KEY and x_soma_key != SOMA_KEY:
        raise HTTPException(status_code=401, detail="Unauthorized")

# ── Auth redirect middleware (SP.2) ──────────────────────────────────────────
_PUBLIC_PATHS = {"/login", "/health", "/shared", "/static", "/setup", "/wizard", "/api/auth/setup"}

class AuthRedirectMiddleware(BaseHTTPMiddleware):
    """Redirect unauthenticated browser requests to /login."""
    async def dispatch(self, request: Request, call_next):
        path = request.url.path
        # Skip: public paths, API endpoints (they return 401 not redirect), assets
        if any(path.startswith(p) for p in _PUBLIC_PATHS) or path.startswith("/api/"):
            return await call_next(request)
        # Check cookie or header
        token = request.cookies.get("soma_session", "")
        key   = request.headers.get("x-soma-key", "")
        if token or key == SOMA_KEY:
            return await call_next(request)
        # No auth — redirect to login for browser requests
        accept = request.headers.get("accept", "")
        if "text/html" in accept:
            return RedirectResponse(f"/login", status_code=302)
        return await call_next(request)

app.add_middleware(AuthRedirectMiddleware)

# Register auth routes (login page, logout, /api/auth/*)
if _AUTH_AVAILABLE:
    _jinja_templates_for_auth = None  # set after jinja2_templates is created below
    @app.on_event("startup")
    async def _register_auth_routes():
        # Templates initialised later in file; re-read here
        _t = Jinja2Templates(directory="/opt/lumina-fleet/soma/templates")
        add_auth_routes(app, _t)

# ── Health ────────────────────────────────────────────────────────────────────
@app.get("/health")
def health():
    return {"status": "ok", "service": "soma", "version": "1.0"}

# ── Cache management (SP.1) ───────────────────────────────────────────────────
@app.post("/api/cache/clear")
def cache_clear(x_soma_key: str = Header(default="")):
    """Invalidate all cached data. Next request will probe live services."""
    _auth(x_soma_key)
    _soma_cache.invalidate()
    return {"ok": True, "message": "Cache cleared. Next requests will fetch live data."}

@app.get("/api/cache/status")
def cache_status(x_soma_key: str = Header(default="")):
    """Show cache age, hit/miss ratio, and last refresh time per key."""
    _auth(x_soma_key)
    return _soma_cache.status_report()

# ── Constellation config ──────────────────────────────────────────────────────
@app.get("/api/constellation")
def get_constellation(x_soma_key: str = Header(default="")):
    _auth(x_soma_key)
    try:
        import yaml
        if CONSTELLATION_YAML.exists():
            with open(CONSTELLATION_YAML) as f:
                return yaml.safe_load(f) or {}
        return {"error": "constellation.yaml not found"}
    except Exception as e:
        return {"error": str(e)}

@app.put("/api/constellation/agent/{agent_id}/display_name")
def rename_agent(agent_id: str, name: str = Body(..., embed=True), x_soma_key: str = Header(default="")):
    """Rename an agent display name. Changes take effect immediately."""
    _auth(x_soma_key)
    try:
        import yaml
        with open(CONSTELLATION_YAML) as f:
            cfg = yaml.safe_load(f) or {}
        if "agents" not in cfg:
            cfg["agents"] = {}
        if agent_id not in cfg["agents"]:
            cfg["agents"][agent_id] = {}
        cfg["agents"][agent_id]["display_name"] = name
        with open(CONSTELLATION_YAML, "w") as f:
            yaml.dump(cfg, f, default_flow_style=False)
        return {"updated": True, "agent_id": agent_id, "display_name": name}
    except Exception as e:
        return {"error": str(e)}

# ── Modules ───────────────────────────────────────────────────────────────────
@app.get("/api/modules")
def list_modules(x_soma_key: str = Header(default="")):
    _auth(x_soma_key)
    try:
        import yaml
        cfg = {}
        if CONSTELLATION_YAML.exists():
            with open(CONSTELLATION_YAML) as f:
                cfg = yaml.safe_load(f) or {}
        modules = cfg.get("modules", {})
        return {"modules": modules, "count": len(modules)}
    except Exception as e:
        return {"error": str(e)}

# ── System health ─────────────────────────────────────────────────────────────
@app.get("/api/system/health")
def system_health(x_soma_key: str = Header(default="")):
    _auth(x_soma_key)
    try:
        import urllib.request
        gateway_url = os.environ.get("GATEWAY_URL", "http://localhost:8080")
        req = urllib.request.Request(f"{gateway_url}/api/health",
            headers={"X-API-Key": os.environ.get("DASHBOARD_API_KEY", "lumina-gateway-dev")})
        with urllib.request.urlopen(req, timeout=5) as r:
            return json.load(r)
    except Exception as e:
        return {"error": str(e)[:100], "gateway": "unreachable"}

# ── Inference control ─────────────────────────────────────────────────────────
@app.get("/api/inference/status")
def inference_status(x_soma_key: str = Header(default="")):
    _auth(x_soma_key)
    try:
        import urllib.request
        litellm_url = os.environ.get("LITELLM_URL", "")
        litellm_key = os.environ.get("LITELLM_MASTER_KEY", "")
        req = urllib.request.Request(f"{litellm_url}/v1/models",
            headers={"Authorization": f"Bearer {litellm_key}"})
        with urllib.request.urlopen(req, timeout=10) as r:
            data = json.load(r)
        return {"status": "online", "models": [m["id"] for m in data.get("data", [])[:10]]}
    except Exception as e:
        return {"status": "error", "error": str(e)[:100]}

# ── Cost dashboard (Myelin) ───────────────────────────────────────────────────
@app.get("/api/cost")
def cost_summary(x_soma_key: str = Header(default="")):
    _auth(x_soma_key)
    usage_file = FLEET_DIR / "myelin" / "output" / "usage.json"
    if usage_file.exists():
        try:
            return json.loads(usage_file.read_text())
        except Exception as e:
            return {"error": str(e)}
    return {"note": "Myelin not yet collecting"}

# ── Backup status (Dura) ──────────────────────────────────────────────────────
@app.get("/api/backup/status")
def backup_status(x_soma_key: str = Header(default="")):
    _auth(x_soma_key)
    status_file = FLEET_DIR / "dura" / "output" / "backup_status.json"
    if status_file.exists():
        try:
            return json.loads(status_file.read_text())
        except Exception as e:
            return {"error": str(e)}
    return {"note": "No backup run yet"}

# ── Logs (Dura) ───────────────────────────────────────────────────────────────
@app.get("/api/logs/services")
def list_log_services(x_soma_key: str = Header(default="")):
    """List available log services with their location."""
    _auth(x_soma_key)
    return {"services": [
        {"name": "soma",     "label": "Soma (admin panel)",    "location": "local"},
        {"name": "axon",     "label": "Axon (work queue)",     "location": "local"},
        {"name": "vigil",    "label": "Vigil (briefings)",     "location": "local"},
        {"name": "sentinel-health", "label": "Sentinel Health", "location": "local"},
        {"name": "ironclaw", "label": "IronClaw (agent)",      "location": "remote-ironclaw"},
        {"name": "ai-mcp",   "label": "Terminus (MCP hub)",    "location": "remote-terminus"},
        {"name": "litellm",  "label": "LiteLLM (proxy)",       "location": "remote-litellm"},
    ]}

@app.get("/api/logs")
def recent_logs(service: str = "soma", lines: int = 100, x_soma_key: str = Header(default="")):
    """Fetch log lines for a given service. Local services read via journalctl.
    Remote services SSH to the appropriate host."""
    _auth(x_soma_key)
    lines = min(max(1, lines), 500)  # clamp 1–500

    _PVS_HOST = os.environ.get("PVS_SSH_HOST", os.environ.get("PVS_SSH_HOST", ""))
    _PVM_HOST = os.environ.get("PVM_SSH_HOST", os.environ.get("PVM_SSH_HOST", ""))

    # Local services (Soma runs on fleet host)
    LOCAL_SERVICES = {"soma", "axon", "vigil", "sentinel-health", "sentinel-metrics",
                      "inbox-monitor", "vector", "skill-evolution", "crucible"}

    try:
        if service in LOCAL_SERVICES:
            r = subprocess.run(
                ["journalctl", "-u", service, "-n", str(lines), "--no-pager", "--output=short-iso"],
                capture_output=True, text=True, timeout=10
            )
            log_lines = r.stdout.splitlines()
        elif service == "ironclaw":
            r = subprocess.run(
                [_PVS_HOST.replace("root@", "ssh root@").split()[0],
                 f"pct exec 305 -- journalctl -u ironclaw -n {lines} --no-pager --output=short-iso 2>/dev/null"],
                capture_output=True, text=True, timeout=12
            )
            # Simpler: use ssh directly
            r = subprocess.run(
                ["ssh", _PVS_HOST,
                 f"pct exec 305 -- journalctl -u ironclaw -n {lines} --no-pager --output=short-iso 2>/dev/null"],
                capture_output=True, text=True, timeout=12
            )
            log_lines = r.stdout.splitlines()
        elif service == "ai-mcp":
            r = subprocess.run(
                ["ssh", _PVM_HOST,
                 f"pct exec 214 -- journalctl -u ai-mcp -n {lines} --no-pager --output=short-iso 2>/dev/null"],
                capture_output=True, text=True, timeout=12
            )
            log_lines = r.stdout.splitlines()
        elif service == "litellm":
            r = subprocess.run(
                ["ssh", _PVM_HOST,
                 f"pct exec 215 -- journalctl -u litellm -n {lines} --no-pager --output=short-iso 2>/dev/null"],
                capture_output=True, text=True, timeout=12
            )
            log_lines = r.stdout.splitlines()
        else:
            log_lines = [f"Unknown service: {service}"]

        return {"ok": True, "service": service, "lines": log_lines[-lines:], "count": len(log_lines)}
    except Exception as e:
        return {"ok": False, "service": service, "lines": [], "error": str(e)[:150]}

# ── Validation (Dura smoke test) ──────────────────────────────────────────────
@app.post("/api/validate/smoke-test")
def run_smoke_test(x_soma_key: str = Header(default="")):
    _auth(x_soma_key)
    try:
        result = subprocess.Popen(
            ["python3", "/opt/lumina-fleet/dura/dura_smoke_test.py", "--quick"],
            stdout=open("/opt/lumina-fleet/dura/logs/smoke_test.log", "a"),
            stderr=subprocess.STDOUT
        )
        return {"status": "started", "pid": result.pid, "note": "Check /api/validate/status"}
    except Exception as e:
        return {"error": str(e)}

@app.get("/api/validate/status")
def validation_status(x_soma_key: str = Header(default="")):
    _auth(x_soma_key)
    results_file = FLEET_DIR / "dura" / "output" / "smoke_test_results.json"
    if results_file.exists():
        try:
            return json.loads(results_file.read_text())
        except Exception as e:
            return {"error": str(e)}
    return {"note": "No smoke test run yet"}

# ── Config read/write (SP.8) ──────────────────────────────────────────────────

def _load_constellation() -> dict:
    """Load constellation.yaml, returning empty dict on failure."""
    try:
        import yaml
        if CONSTELLATION_YAML.exists():
            with open(CONSTELLATION_YAML) as f:
                return yaml.safe_load(f) or {}
    except Exception:
        pass
    return {}

def _save_constellation(cfg: dict):
    """Write constellation.yaml with backup."""
    import yaml, shutil
    backup = CONSTELLATION_YAML.with_suffix(".yaml.bak")
    if CONSTELLATION_YAML.exists():
        shutil.copy2(CONSTELLATION_YAML, backup)
    with open(CONSTELLATION_YAML, "w") as f:
        yaml.dump(cfg, f, default_flow_style=False, allow_unicode=True)

def _redact(d: dict) -> dict:
    """Recursively redact sensitive keys."""
    _SENSITIVE = {"password", "token", "secret", "api_key", "app_password",
                  "access_token", "client_secret", "private_key"}
    if isinstance(d, dict):
        return {k: "[REDACTED]" if k.lower() in _SENSITIVE else _redact(v) for k, v in d.items()}
    if isinstance(d, list):
        return [_redact(i) for i in d]
    return d

@app.get("/api/config")
def get_full_config(x_soma_key: str = Header(default="")):
    """Return full constellation.yaml (sensitive values redacted)."""
    _auth(x_soma_key)
    cfg = _load_constellation()
    return {"ok": True, "config": _redact(cfg)}

@app.get("/api/config/{section}")
def get_config_section(section: str, x_soma_key: str = Header(default="")):
    """Return one section of constellation.yaml.
    Special sections: 'secrets' returns Infisical key names only."""
    _auth(x_soma_key)
    cfg = _load_constellation()
    if section == "secrets":
        # Return env var names configured (not values)
        env_file = FLEET_DIR / ".env"
        keys = []
        if env_file.exists():
            for line in env_file.read_text().splitlines():
                if "=" in line and not line.startswith("#"):
                    k = line.split("=")[0].strip()
                    if k:
                        keys.append(k)
        return {"ok": True, "section": "secrets", "keys": keys}
    if section == "modules":
        return {"ok": True, "section": "modules",
                "modules": cfg.get("modules", {}),
                "count": len(cfg.get("modules", {}))}
    data = cfg.get(section)
    if data is None:
        return {"ok": False, "error": f"Section '{section}' not found in constellation.yaml",
                "cached_at": None, "stale": False}
    return {"ok": True, "section": section, "data": _redact(data) if isinstance(data, dict) else data}

@app.put("/api/config/{section}")
async def update_config_section(
    section: str,
    request: Request,
    x_soma_key: str = Header(default=""),
):
    """Update one section of constellation.yaml. Backs up before write."""
    _auth(x_soma_key)
    _READONLY_SECTIONS = {"secrets"}
    if section in _READONLY_SECTIONS:
        raise HTTPException(400, f"Section '{section}' is read-only via this API")
    body = await request.json()
    cfg = _load_constellation()
    cfg[section] = body
    try:
        _save_constellation(cfg)
        return {"ok": True, "section": section, "updated": True}
    except Exception as e:
        return {"ok": False, "error": str(e)[:200], "cached_at": None, "stale": False}

# Legacy single-module config (kept for backward compat)
@app.get("/api/config/module/{module}")
def get_module_config_legacy(module: str, x_soma_key: str = Header(default="")):
    _auth(x_soma_key)
    config_paths = {
        "myelin": FLEET_DIR / "myelin" / "myelin_config.yaml",
        "axon": FLEET_DIR / "axon" / ".env",
    }
    path = config_paths.get(module)
    if not path or not path.exists():
        return {"ok": False, "error": f"No config found for module: {module}",
                "cached_at": None, "stale": False}
    try:
        if path.suffix == ".yaml":
            import yaml
            with open(path) as f:
                return {"ok": True, "data": _redact(yaml.safe_load(f) or {})}
        else:
            keys = [line.split("=")[0].strip() for line in path.read_text().splitlines()
                    if "=" in line and not line.startswith("#")]
            return {"ok": True, "module": module, "configured_keys": keys}
    except Exception as e:
        return {"ok": False, "error": str(e), "cached_at": None, "stale": False}

# ── Documentation API ──────────────────────────────────────────────────────────
import urllib.request as _urlreq

@app.get('/api/docs/search')
def search_docs(q: str = '', x_soma_key: str = Header(default='')):
    """Full-text search across documentation."""
    _auth(x_soma_key)
    if not q:
        return {'results': [], 'query': q}
    docs_dir = Path('/opt/lumina-fleet/docs')
    results = []
    if docs_dir.exists():
        for f in sorted(docs_dir.rglob('*.md')):
            content = f.read_text()
            if q.lower() in content.lower():
                # Find matching lines
                lines = [l for l in content.splitlines() if q.lower() in l.lower()]
                rel = str(f.relative_to(docs_dir))
                results.append({'path': rel, 'matches': lines[:3], 'total_matches': len(lines)})
    return {'query': q, 'count': len(results), 'results': results[:10]}

@app.get('/api/docs')
def list_docs(x_soma_key: str = Header(default='')):
    """List all available documentation pages."""
    _auth(x_soma_key)
    docs_dir = Path('/opt/lumina-fleet/docs')
    pages = []
    if docs_dir.exists():
        for f in sorted(docs_dir.rglob('*.md')):
            rel = str(f.relative_to(docs_dir))
            pages.append({'path': rel, 'size': f.stat().st_size})
    return {'count': len(pages), 'pages': pages}

@app.get('/api/docs/{path:path}')
def get_doc(path: str, x_soma_key: str = Header(default='')):
    """Serve documentation from Gitea lumina-docs repo."""
    _auth(x_soma_key)
    gitea_url = os.environ.get('GITEA_URL', '')
    gitea_token = os.environ.get('GITEA_TOKEN', '')
    # Try to fetch from Gitea first
    try:
        url = f'{gitea_url}/api/v1/repos/moosenet/lumina-docs/raw/{path}?token={gitea_token}'
        req = _urlreq.Request(url, headers={'Authorization': f'token {gitea_token}'})
        with _urlreq.urlopen(req, timeout=10) as r:
            content = r.read().decode('utf-8')
        return {'path': path, 'content': content, 'format': 'markdown'}
    except Exception:
        pass
    # Fallback: local /opt/lumina-fleet/docs/
    local_path = Path('/opt/lumina-fleet/docs') / path
    if local_path.exists() and local_path.is_file():
        return {'path': path, 'content': local_path.read_text(), 'format': 'markdown'}
    return {'error': f'Doc not found: {path}'}


# ── Soma Insights (NPC Feature 2) ─────────────────────────────────────────────

@app.get('/api/insights')
def get_insights(x_soma_key: str = Header(default='')):
    """Get pending Soma Insights suggestions from conversation review."""
    _auth(x_soma_key)
    try:
        import sqlite3
        conn = sqlite3.connect('/opt/lumina-fleet/soma/soma.db')
        conn.row_factory = sqlite3.Row
        cur = conn.cursor()
        cur.execute("SELECT * FROM insights WHERE status='pending' ORDER BY created_at DESC LIMIT 20")
        items = [dict(r) for r in cur.fetchall()]
        conn.close()
        return {'count': len(items), 'insights': items}
    except Exception as e:
        return {'count': 0, 'insights': [], 'error': str(e)[:100]}


@app.post('/api/insights/{insight_id}/accept')
def accept_insight(insight_id: int, x_soma_key: str = Header(default='')):
    """Accept an insight — marks it accepted and wires to Calx."""
    _auth(x_soma_key)
    try:
        import sqlite3
        conn = sqlite3.connect('/opt/lumina-fleet/soma/soma.db')
        conn.execute("UPDATE insights SET status='accepted', acted_on=datetime('now') WHERE id=?", (insight_id,))
        conn.commit(); conn.close()
        return {'status': 'accepted', 'id': insight_id}
    except Exception as e:
        return {'error': str(e)[:100]}


@app.post('/api/insights/{insight_id}/dismiss')
def dismiss_insight(insight_id: int, x_soma_key: str = Header(default='')):
    """Dismiss an insight."""
    _auth(x_soma_key)
    try:
        import sqlite3
        conn = sqlite3.connect('/opt/lumina-fleet/soma/soma.db')
        conn.execute("UPDATE insights SET status='dismissed', acted_on=datetime('now') WHERE id=?", (insight_id,))
        conn.commit(); conn.close()
        return {'status': 'dismissed', 'id': insight_id}
    except Exception as e:
        return {'error': str(e)[:100]}


@app.post('/api/insights/{insight_id}/later')
def snooze_insight(insight_id: int, x_soma_key: str = Header(default='')):
    """Snooze an insight for later."""
    _auth(x_soma_key)
    try:
        import sqlite3
        conn = sqlite3.connect('/opt/lumina-fleet/soma/soma.db')
        conn.execute("UPDATE insights SET status='later', acted_on=datetime('now') WHERE id=?", (insight_id,))
        conn.commit(); conn.close()
        return {'status': 'later', 'id': insight_id}
    except Exception as e:
        return {'error': str(e)[:100]}

# ── Wizard ─────────────────────────────────────────────────────────────────
from fastapi.responses import HTMLResponse, FileResponse

TEMPLATES_DIR = Path("/opt/lumina-fleet/soma/templates")
jinja2_templates = Jinja2Templates(directory=str(TEMPLATES_DIR))


@app.get("/setup")
@app.get("/wizard")  # kept for backward compat, redirects to /setup
def wizard_page():
    """Setup wizard — 9-step onboarding and configuration flow."""
    wizard_file = TEMPLATES_DIR / "wizard.html"
    if wizard_file.exists():
        return FileResponse(str(wizard_file), media_type="text/html")
    return HTMLResponse("<h1>Setup not found</h1>", status_code=404)


@app.post("/api/setup/accept-disclaimer")
async def accept_disclaimer(request: Request, x_soma_key: str = Header(default="")):
    """Record operator disclaimer acceptance (ADD.3). Writes disclaimer-accepted.json."""
    import socket
    try:
        local_ip = socket.gethostbyname(socket.gethostname())
    except Exception:
        local_ip = "unknown"
    record = {
        "accepted_at": datetime.now(timezone.utc).isoformat(),
        "accepted_by": "admin",
        "version": "1.0",
        "ip": local_ip,
    }
    try:
        disclaimer_file = FLEET_DIR / "disclaimer-accepted.json"
        disclaimer_file.write_text(json.dumps(record, indent=2))
        return {"ok": True, "accepted_at": record["accepted_at"]}
    except Exception as e:
        return {"ok": False, "error": str(e)[:100]}

# Redirect to setup if disclaimer not accepted (checked on startup)
@app.get("/api/setup/disclaimer-status")
def disclaimer_status():
    disclaimer_file = FLEET_DIR / "disclaimer-accepted.json"
    if disclaimer_file.exists():
        try:
            data = json.loads(disclaimer_file.read_text())
            return {"accepted": True, "accepted_at": data.get("accepted_at"), "version": data.get("version")}
        except Exception:
            pass
    return {"accepted": False}

@app.post("/api/wizard/apply")
def wizard_apply(config: dict = Body(...), x_soma_key: str = Header(default="")):
    """Apply wizard configuration. Writes constellation.yaml changes."""
    import yaml

    results = {'applied': [], 'errors': []}

    # Apply assistant name
    if config.get('name'):
        try:
            if CONSTELLATION_YAML.exists():
                with open(CONSTELLATION_YAML) as f:
                    cfg = yaml.safe_load(f) or {}
            else:
                cfg = {}
            if 'agents' not in cfg:
                cfg['agents'] = {}
            if 'lumina' not in cfg['agents']:
                cfg['agents']['lumina'] = {}
            cfg['agents']['lumina']['display_name'] = config['name']
            CONSTELLATION_YAML.parent.mkdir(parents=True, exist_ok=True)
            with open(CONSTELLATION_YAML, 'w') as f:
                yaml.dump(cfg, f, default_flow_style=False)
            results['applied'].append('Name set to: ' + config['name'])
        except Exception as e:
            results['errors'].append('Name: ' + str(e))

    # Apply chat platform preference
    if config.get('chat_platform'):
        try:
            if CONSTELLATION_YAML.exists():
                with open(CONSTELLATION_YAML) as f:
                    cfg = yaml.safe_load(f) or {}
            else:
                cfg = {}
            if 'settings' not in cfg:
                cfg['settings'] = {}
            cfg['settings']['chat_platform'] = config['chat_platform']
            with open(CONSTELLATION_YAML, 'w') as f:
                yaml.dump(cfg, f, default_flow_style=False)
            results['applied'].append('Chat platform: ' + config['chat_platform'])
        except Exception as e:
            results['errors'].append('Chat platform: ' + str(e))

    # Apply module selections
    if config.get('modules'):
        try:
            enabled = [k for k, v in config['modules'].items() if v]
            if CONSTELLATION_YAML.exists():
                with open(CONSTELLATION_YAML) as f:
                    cfg = yaml.safe_load(f) or {}
            else:
                cfg = {}
            if 'modules' not in cfg:
                cfg['modules'] = {}
            for mod_id in config['modules']:
                cfg['modules'][mod_id] = {'enabled': bool(config['modules'][mod_id])}
            with open(CONSTELLATION_YAML, 'w') as f:
                yaml.dump(cfg, f, default_flow_style=False)
            results['applied'].append('Modules configured: ' + str(len(enabled)) + ' enabled')
        except Exception as e:
            results['errors'].append('Modules: ' + str(e))

    return {'status': 'ok' if not results['errors'] else 'partial', **results}


# ── Wizard scan (kept for wizard page compatibility) ──────────────────────────

@app.get("/api/wizard/scan")
def wizard_scan_status():
    """Infrastructure health summary for wizard scan step. Returns HTML for HTMX."""
    import urllib.request as _ur
    import socket

    checks = []

    def _ping(name, url, timeout=3):
        try:
            _ur.urlopen(url, timeout=timeout)
            return {'service': name, 'status': 'ok'}
        except Exception as ex:
            return {'service': name, 'status': 'unreachable', 'detail': str(ex)[:60]}

    checks.append(_ping('Soma API', 'http://localhost:8082/health'))
    checks.append(_ping('LiteLLM', (os.environ.get('LITELLM_URL','') + '/health')))
    checks.append(_ping('Terminus MCP', (os.environ.get('TERMINUS_MCP_URL','') + '/health')))

    try:
        s = socket.create_connection((os.environ.get('INBOX_DB_HOST',''), 5432), timeout=3)
        s.close()
        checks.append({'service': 'Postgres', 'status': 'ok'})
    except Exception as ex:
        checks.append({'service': 'Postgres', 'status': 'unreachable', 'detail': str(ex)[:60]})

    ok_count = sum(1 for c in checks if c['status'] == 'ok')
    total = len(checks)
    rows = ''
    for c in checks:
        badge = 'badge-success' if c['status'] == 'ok' else 'badge-warning'
        icon = '&#x2713;' if c['status'] == 'ok' else '&#x26A0;'
        rows += (
            '<div style="display:flex;justify-content:space-between;align-items:center;'
            'padding:0.5rem 0;border-bottom:1px solid var(--border-color);">'
            '<span>' + c['service'] + '</span>'
            '<span class="' + badge + '">' + icon + ' ' + c['status'] + '</span>'
            '</div>'
        )
    summary_class = 'alert-success' if ok_count == total else 'alert-warning'
    summary = str(ok_count) + ' / ' + str(total) + ' services reachable'
    html = '<div class="' + summary_class + '" style="margin-bottom:1rem;">' + summary + '</div><div>' + rows + '</div>'
    if ok_count < total:
        html += (
            '<p style="color:var(--text-secondary);font-size:0.875rem;margin-top:1rem;">'
            'Unreachable services won\'t block setup — configure them after launch.</p>'
        )
    return HTMLResponse(content=html)


# ── Comprehensive Status (LM-355) ─────────────────────────────────────────────

import urllib.request as _urlreq_status

def _run_check(fn, timeout=3):
    """Run a check function with timeout guard. Returns result dict or error."""
    import threading
    result = {"error": "timeout", "ok": False}
    def target():
        try:
            r = fn()
            result.clear()
            result.update(r)
        except Exception as e:
            result.clear()
            result["error"] = str(e)[:120]
            result["ok"] = False
    t = threading.Thread(target=target, daemon=True)
    t.start()
    t.join(timeout)
    return result


def _check_ironclaw():
    r = subprocess.run(
        ["ssh", os.environ.get("PVS_SSH_HOST", ""),
         "pct exec 305 -- /usr/local/bin/ironclaw --version 2>&1"],
        capture_output=True, text=True, timeout=6
    )
    ver = r.stdout.strip() or r.stderr.strip()
    return {"value": ver or "unknown", "ok": bool(ver)}


def _check_active_agents():
    agents = {}
    r = subprocess.run(
        ["ssh", os.environ.get("PVS_SSH_HOST", ""), "pct exec 310 -- systemctl is-active axon.service 2>/dev/null"],
        capture_output=True, text=True, timeout=6
    )
    agents["axon"] = r.stdout.strip() == "active"
    r2 = subprocess.run(
        ["ssh", os.environ.get("PVS_SSH_HOST", ""), "pct exec 310 -- systemctl is-active sentinel-health.timer 2>/dev/null"],
        capture_output=True, text=True, timeout=6
    )
    agents["sentinel"] = r2.stdout.strip() == "active"
    r3 = subprocess.run(
        ["ssh", os.environ.get("PVS_SSH_HOST", ""), "pct exec 310 -- pgrep -f briefing.py 2>/dev/null"],
        capture_output=True, text=True, timeout=6
    )
    agents["vigil"] = bool(r3.stdout.strip())
    active_count = sum(1 for v in agents.values() if v)
    return {"value": agents, "ok": active_count > 0, "active_count": active_count}


def _check_nexus_messages():
    try:
        import psycopg2
        db_host = os.environ.get("INBOX_DB_HOST", "")
        db_user = os.environ.get("INBOX_DB_USER", "lumina")
        db_pass = os.environ.get("INBOX_DB_PASS", "")
        if not db_host:
            return {"value": None, "ok": False, "error": "INBOX_DB_HOST not set"}
        conn = psycopg2.connect(
            host=db_host, dbname="lumina_inbox",
            user=db_user, password=db_pass, connect_timeout=3
        )
        cur = conn.cursor()
        # Check table exists first — create it if not (graceful degradation)
        cur.execute("""
            SELECT EXISTS (
                SELECT FROM pg_tables
                WHERE schemaname='public' AND tablename='inbox_messages'
            )
        """)
        table_exists = cur.fetchone()[0]
        if not table_exists:
            conn.close()
            return {"value": 0, "ok": True, "note": "inbox_messages table not yet created"}
        cur.execute("SELECT COUNT(*) FROM inbox_messages WHERE status = 'pending'")
        pending = cur.fetchone()[0]
        cur.execute("SELECT COUNT(*) FROM inbox_messages WHERE status = 'acked'")
        total = pending + cur.fetchone()[0]
        conn.close()
        return {"value": pending, "ok": True, "total": total, "pending": pending}
    except ImportError:
        return {"value": None, "ok": False, "error": "psycopg2 not installed"}
    except Exception as e:
        err = str(e)[:100]
        return {"value": None, "ok": False, "error": err}


def _check_engram_facts():
    try:
        import sqlite3 as _sqlite3
        db_path = "/opt/lumina-fleet/engram/engram.db"
        if not Path(db_path).exists():
            return {"value": None, "ok": False, "error": "db not found"}
        conn = _sqlite3.connect(db_path)
        cur = conn.cursor()
        cur.execute("SELECT name FROM sqlite_master WHERE type='table'")
        tables = [row[0] for row in cur.fetchall()]
        total = 0
        for tbl in tables:
            try:
                cur.execute(f"SELECT COUNT(*) FROM \"{tbl}\"")
                total += cur.fetchone()[0]
            except Exception:
                pass
        conn.close()
        return {"value": total, "ok": True, "tables": tables}
    except Exception as e:
        return {"value": None, "ok": False, "error": str(e)[:80]}


def _check_litellm_models():
    litellm_url = os.environ.get("LITELLM_URL", "")
    litellm_key = os.environ.get("LITELLM_MASTER_KEY", "")
    req = _urlreq_status.Request(
        f"{litellm_url}/v1/models",
        headers={"Authorization": f"Bearer {litellm_key}"}
    )
    with _urlreq_status.urlopen(req, timeout=5) as r:
        data = json.load(r)
    models = [m["id"] for m in data.get("data", [])]
    return {"value": len(models), "ok": len(models) > 0, "models": models[:10]}


def _check_matrix_bridge():
    r = subprocess.run(
        ["ssh", os.environ.get("PVS_SSH_HOST", ""),
         "pct exec 306 -- systemctl is-active matrix-bridge.service 2>/dev/null"],
        capture_output=True, text=True, timeout=6
    )
    active = r.stdout.strip() == "active"
    return {"value": "running" if active else "stopped", "ok": active}


def _check_refractor_categories():
    """Count Refractor keyword categories from llm-proxy.py on the IronClaw host."""
    pvs_host = os.environ.get("PVS_HOST", "")
    r = subprocess.run(
        ["ssh", "-o", "ConnectTimeout=5", "-o", "BatchMode=yes", pvs_host,
         "pct exec 305 -- python3 -c \""
         "import re; "
         "src=open('/usr/local/bin/llm-proxy.py').read(); "
         "cats=re.findall(r'\\\"([a-z_]+)\\\"\\s*:\\s*[\\[\\{]', src); "
         "print(len(set(c for c in cats if len(c)>2)))\" 2>/dev/null"],
        capture_output=True, text=True, timeout=10
    )
    count_str = r.stdout.strip()
    try:
        count = int(count_str)
    except ValueError:
        count = 0
    return {"value": count, "ok": count > 10}


def _check_plane_projects():
    import sys as _s
    _s.path.insert(0, '/opt/plane-helper')
    from plane_helper import PlaneClient
    plane = PlaneClient()
    data = plane.get('/workspaces/moosenet/projects/')
    projects = data.get("results", data) if isinstance(data, dict) else data
    count = len(projects)
    return {"value": count, "ok": count > 0}


@app.get("/api/status")
async def get_status():
    """Comprehensive system status — all checks run concurrently with 3s timeout each.
    Results served from cache (10s TTL) with background refresh. First load: live probe."""
    import time
    # Try cache first (SP.1 caching layer)
    if _CACHE_AVAILABLE:
        cached = _soma_cache.get('/api/status')
        if cached is not None:
            return cached

    now = time.monotonic()
    _dummy_ts = now  # unused but kept for compatibility

    check_fns = {
        "ironclaw_version":     _check_ironclaw,
        "active_agents":        _check_active_agents,
        "nexus_messages":       _check_nexus_messages,
        "engram_facts":         _check_engram_facts,
        "litellm_models":       _check_litellm_models,
        "matrix_bridge":        _check_matrix_bridge,
        "refractor_categories": _check_refractor_categories,
        "plane_projects":       _check_plane_projects,
    }

    checks = {}
    with ThreadPoolExecutor(max_workers=8) as ex:
        futures = {ex.submit(_run_check, fn): name for name, fn in check_fns.items()}
        for future in as_completed(futures, timeout=8):
            name = futures[future]
            try:
                checks[name] = future.result(timeout=0.1)
            except Exception as e:
                checks[name] = {"value": None, "ok": False, "error": str(e)[:60]}

    # Fill any that timed out
    for name in check_fns:
        if name not in checks:
            checks[name] = {"value": None, "ok": False, "error": "timeout"}

    failed_names = [name for name, c in checks.items() if not c.get("ok", False)]
    failed = len(failed_names)
    core_checks = {"ironclaw_version", "litellm_models"}
    core_down = [n for n in failed_names if n in core_checks]

    if failed == 0:
        overall = "ok"
        status_label = "ALL SYSTEMS OK"
    elif core_down:
        overall = "critical"
        status_label = f"CRITICAL — {', '.join(core_down)} down"
    else:
        overall = "degraded"
        status_label = f"DEGRADED — {', '.join(failed_names)}"

    # Cost summary from Myelin output
    cost_summary = None
    try:
        usage_file = FLEET_DIR / "myelin" / "output" / "usage.json"
        if usage_file.exists():
            cost_data = json.loads(usage_file.read_text())
            cost_summary = {
                "today_usd": cost_data.get("today_usd", 0),
                "budget_warn": 2.0,
                "budget_hard": 10.0,
            }
    except Exception:
        pass

    result = {
        "status": overall,
        "status_label": status_label,
        "checks": checks,
        "failed_services": failed_names,
        "cost": cost_summary,
        "timestamp": datetime.now(timezone.utc).isoformat(),
    }

    if _CACHE_AVAILABLE:
        _soma_cache.set('/api/status', result)
    return result


@app.post("/api/chat")
async def chat_proxy(request: Request, x_soma_key: str = Header(default="")):
    """Proxy chat to IronClaw HTTP gateway. Returns SSE stream."""
    from fastapi.responses import StreamingResponse
    import urllib.request as _urlreq

    _auth(x_soma_key)
    body = await request.json()
    message = body.get("message", "").strip()
    if not message:
        return JSONResponse({"error": "message required"}, status_code=400)

    def stream_response():
        try:
            data = json.dumps({
                "model": "claude-sonnet",
                "messages": [{"role": "user", "content": message}],
                "stream": False,
                "max_tokens": 500,
            }).encode()
            req = _urlreq.Request(
                f"{IRONCLAW_URL}/v1/chat/completions",
                data=data,
                headers={
                    "Content-Type": "application/json",
                    "Authorization": f"Bearer {IRONCLAW_TOKEN}",
                },
                method="POST"
            )
            with _urlreq.urlopen(req, timeout=60) as resp:
                result = json.load(resp)
                content = result.get("choices", [{}])[0].get("message", {}).get("content", "")
                yield f"data: {json.dumps({'content': content})}\n\n"
        except Exception as e:
            yield f"data: {json.dumps({'error': str(e)[:200]})}\n\n"
        yield "data: [DONE]\n\n"

    return StreamingResponse(stream_response(), media_type="text/event-stream")


@app.get("/api/chat/test")
def chat_test(x_soma_key: str = Header(default="")):
    """Test IronClaw connectivity."""
    _auth(x_soma_key)
    try:
        import urllib.request as _urlreq
        req = _urlreq.Request(
            f"{IRONCLAW_URL}/v1/models",
            headers={"Authorization": f"Bearer {IRONCLAW_TOKEN}"}
        )
        with _urlreq.urlopen(req, timeout=5) as r:
            return {"connected": True, "url": IRONCLAW_URL, "status": r.status}
    except Exception as e:
        return {"connected": False, "url": IRONCLAW_URL, "error": str(e)[:100]}




# ── Page routes ───────────────────────────────────────────────────────────────
# ALL routes use jinja2_templates.TemplateResponse — never FileResponse for templates.
# FileResponse serves raw bytes and DOES NOT render Jinja2 syntax.

def _tmpl(request, template_name, extra_ctx=None):
    """Render a template via Jinja2. Raises 404 if template missing."""
    # NOTE: This FastAPI version uses TemplateResponse(request, name, context)
    return jinja2_templates.TemplateResponse(request, template_name, ctx)

@app.get("/")
async def root_page(request: Request):
    status_data = await get_status()
    return jinja2_templates.TemplateResponse(
        request, "status.html",
        {"status": status_data, "active_page": "status"}
    )

@app.get("/status")
async def status_page(request: Request):
    """Render status dashboard with live data via Jinja2."""
    status_data = await get_status()
    return jinja2_templates.TemplateResponse(
        request, "status.html",
        {"status": status_data, "active_page": "status"}
    )

@app.get("/config")
def config_page(request: Request):
    return jinja2_templates.TemplateResponse(request, "config.html", {"active_page": "config"})

@app.get("/skills")
def skills_page(request: Request):
    return jinja2_templates.TemplateResponse(request, "skills.html", {"active_page": "skills"})

@app.get("/plugins")
def plugins_page(request: Request):
    tmpl = TEMPLATES_DIR / "plugins.html"
    if not tmpl.exists():
        from fastapi.responses import RedirectResponse
        return RedirectResponse(url="/skills")
    return jinja2_templates.TemplateResponse(request, "plugins.html", {"active_page": "plugins"})

@app.get("/sessions")
def sessions_page(request: Request):
    return jinja2_templates.TemplateResponse(request, "sessions.html", {"active_page": "sessions"})

@app.get("/cron")
def cron_page(request: Request):
    return jinja2_templates.TemplateResponse(request, "cron.html", {"active_page": "cron"})

@app.get("/timers")
def timers_page(request: Request):
    return jinja2_templates.TemplateResponse(request, "cron.html", {"active_page": "cron"})

@app.get("/logs")
def logs_page(request: Request):
    return jinja2_templates.TemplateResponse(request, "logs.html", {"active_page": "logs"})


@app.get("/api/sessions")
def list_sessions(page: int = 1, limit: int = 20, x_soma_key: str = Header(default="")):
    """List conversation sessions from IronClaw on the agent host."""
    _auth(x_soma_key)
    limit = min(limit, 100)
    offset = (page - 1) * limit
    _PVS_HOST = os.environ.get("PVS_SSH_HOST", os.environ.get("PVS_SSH_HOST", ""))
    _IC_CT = os.environ.get("IRONCLAW_CT", "305")
    try:
        # Query IronClaw SQLite DB on the agent host via SSH
        query = (
            f"python3 -c \""
            f"import sqlite3,json,os; "
            f"db=[p for p in ['/root/.ironclaw/ironclaw.db','/opt/ironclaw/ironclaw.db'] if os.path.exists(p)]; "
            f"conn=sqlite3.connect(db[0]) if db else None; "
            f"rows=[]; total=0; "
            f"[rows.extend(conn.execute('SELECT id,channel,created_at,message_count FROM sessions ORDER BY created_at DESC LIMIT {limit} OFFSET {offset}').fetchall()) or setattr(conn,'_ok',True) for _ in [1]] if conn else None; "
            f"[total:=conn.execute('SELECT COUNT(*) FROM sessions').fetchone()[0] for _ in [1]] if conn else None; "
            f"print(json.dumps({{'sessions':[dict(zip(['id','channel','created_at','message_count'],r)) for r in rows],'total':total,'db':db[0] if db else None}})) if conn else print(json.dumps({{'sessions':[],'total':0,'db':None,'note':'ironclaw.db not found'}}))"
            f"\" 2>/dev/null"
        )
        r = subprocess.run(
            ["ssh", _PVS_HOST, f"pct exec {_IC_CT} -- {query}"],
            capture_output=True, text=True, timeout=12
        )
        if r.returncode == 0 and r.stdout.strip():
            data = json.loads(r.stdout.strip())
            return {
                "ok": True,
                "sessions": data.get("sessions", []),
                "total": data.get("total", 0),
                "page": page,
                "limit": limit,
                "note": data.get("note"),
            }
        return {"ok": True, "sessions": [], "total": 0,
                "note": "Session history unavailable — IronClaw DB not accessible via SSH"}
    except Exception as e:
        return {"ok": False, "sessions": [], "total": 0,
                "error": str(e)[:150],
                "note": "Check PVS_SSH_HOST and IRONCLAW_CT env vars"}

@app.get("/api/sessions/search")
def search_sessions(q: str = "", x_soma_key: str = Header(default="")):
    """Full-text search across session messages."""
    _auth(x_soma_key)
    if not q:
        return {"ok": False, "error": "q parameter required", "results": []}
    # For now return guidance — full FTS5 requires IronClaw schema knowledge
    return {"ok": True, "results": [], "note": f"Search for '{q}' — FTS requires IronClaw schema access"}


def _fmt_timer_ts(us: int) -> str:
    """Convert systemd microsecond epoch timestamp to human-readable string."""
    if not us or us <= 0:
        return ""
    try:
        import time as _time
        dt = datetime.fromtimestamp(us / 1_000_000, tz=timezone.utc)
        now = datetime.now(timezone.utc)
        delta = dt - now
        secs = int(delta.total_seconds())
        if secs < 0:
            secs = -secs
            if secs < 60:
                return f"{secs}s ago"
            if secs < 3600:
                return f"{secs // 60}m ago"
            if secs < 86400:
                return f"{secs // 3600}h ago"
            return f"{secs // 86400}d ago"
        else:
            if secs < 60:
                return f"in {secs}s"
            if secs < 3600:
                return f"in {secs // 60}m"
            if secs < 86400:
                h, m = divmod(secs // 60, 60)
                return f"in {h}h {m}m" if m else f"in {h}h"
            return f"in {secs // 86400}d"
    except Exception:
        return str(us)


@app.get("/api/timers")
def list_timers(x_soma_key: str = Header(default="")):
    """List all systemd timers with status and schedule."""
    _auth(x_soma_key)
    try:
        r = subprocess.run(
            ["systemctl", "list-timers", "--all", "--no-pager", "--output=json"],
            capture_output=True, text=True, timeout=10
        )
        timers = []
        if r.returncode == 0 and r.stdout.strip().startswith("["):
            raw = json.loads(r.stdout)
            for t in raw:
                name = t.get("unit", "")
                if not name.endswith(".timer"):
                    continue
                timers.append({
                    "name": name,
                    "description": t.get("description", ""),
                    "active": t.get("active", "") == "active",
                    "enabled": t.get("enabled", "") in ("enabled", "static"),
                    "next": _fmt_timer_ts(t.get("next", 0)),
                    "last": _fmt_timer_ts(t.get("last", 0)),
                    "passed": _fmt_timer_ts(t.get("passed", 0)),
                    "unit": name.replace(".timer", ".service"),
                })
        else:
            # Fallback: parse text output
            for line in r.stdout.splitlines():
                if ".timer" not in line or "UNIT" in line or "timers listed" in line:
                    continue
                parts = line.split()
                name = next((p for p in parts if p.endswith(".timer")), None)
                if name:
                    timers.append({
                        "name": name,
                        "active": True,
                        "raw": line[:120].strip(),
                        "unit": name.replace(".timer", ".service"),
                    })
        return {"ok": True, "count": len(timers), "timers": timers}
    except Exception as e:
        return {"ok": False, "count": 0, "timers": [], "error": str(e)[:100]}


@app.post("/api/timers/{timer_name}/trigger")
def trigger_timer(timer_name: str, x_soma_key: str = Header(default="")):
    _auth(x_soma_key)
    safe = timer_name.replace(".timer",".service").replace("/","").replace("..","")
    if not safe.endswith(".service"): safe += ".service"
    try:
        result = subprocess.run(["systemctl", "start", safe], capture_output=True, text=True, timeout=15)
        return {"ok": result.returncode == 0, "triggered": result.returncode == 0,
                "service": safe, "error": result.stderr[:100] if result.returncode != 0 else None}
    except Exception as e:
        return {"ok": False, "triggered": False, "error": str(e)[:100]}

@app.post("/api/timers/{timer_name}/enable")
def enable_timer(timer_name: str, x_soma_key: str = Header(default="")):
    _auth(x_soma_key)
    safe = timer_name.replace("/","").replace("..","")
    if not safe.endswith(".timer"): safe += ".timer"
    try:
        r = subprocess.run(["systemctl", "enable", "--now", safe], capture_output=True, text=True, timeout=10)
        return {"ok": r.returncode == 0, "timer": safe}
    except Exception as e:
        return {"ok": False, "error": str(e)[:100]}

@app.post("/api/timers/{timer_name}/disable")
def disable_timer(timer_name: str, x_soma_key: str = Header(default="")):
    _auth(x_soma_key)
    safe = timer_name.replace("/","").replace("..","")
    if not safe.endswith(".timer"): safe += ".timer"
    try:
        r = subprocess.run(["systemctl", "disable", "--now", safe], capture_output=True, text=True, timeout=10)
        return {"ok": r.returncode == 0, "timer": safe}
    except Exception as e:
        return {"ok": False, "error": str(e)[:100]}

@app.get("/api/timers/{timer_name}/output")
def timer_output(timer_name: str, lines: int = 50, x_soma_key: str = Header(default="")):
    """Return last N lines of the associated service journal."""
    _auth(x_soma_key)
    safe = timer_name.replace(".timer",".service").replace("/","").replace("..","")
    if not safe.endswith(".service"): safe += ".service"
    lines = min(max(1, lines), 500)
    try:
        r = subprocess.run(
            ["journalctl", "-u", safe, "-n", str(lines), "--no-pager", "--output=short-iso"],
            capture_output=True, text=True, timeout=10
        )
        return {"ok": True, "service": safe, "lines": r.stdout.splitlines()}
    except Exception as e:
        return {"ok": False, "service": safe, "lines": [], "error": str(e)[:100]}


@app.get("/status/grid", response_class=HTMLResponse)
async def status_grid(request: Request):
    """Return just the grid cards HTML fragment for HTMX polling."""
    status_data = await get_status()
    return jinja2_templates.TemplateResponse(
        request, "status_grid.html",
        {"status": status_data}
    )


# ── Skills endpoints (LM-358) ─────────────────────────────────────────────────

def _parse_skill_frontmatter(skill_dir: Path):
    """Parse SKILL.md YAML frontmatter from a skill directory."""
    skill_md = skill_dir / "SKILL.md"
    if not skill_md.exists():
        return None
    content = skill_md.read_text()
    if not content.startswith("---"):
        return {"name": skill_dir.name, "description": "", "raw": True}
    parts = content.split("---", 2)
    if len(parts) < 3:
        return {"name": skill_dir.name, "description": ""}
    try:
        import yaml
        fm = yaml.safe_load(parts[1]) or {}
        fm["_body"] = parts[2].strip()
        return fm
    except Exception:
        return {"name": skill_dir.name, "description": ""}


def _get_skill_stats(skill_dir: Path):
    """Read usage_count and last_success from skill stats.json if present."""
    stats_file = skill_dir / "stats.json"
    if stats_file.exists():
        try:
            return json.loads(stats_file.read_text())
        except Exception:
            pass
    return {"usage_count": 0, "last_success": None}


@app.get("/api/skills")
def list_skills(status: str = "all", x_soma_key: str = Header(default="")):
    """List skills from fleet host filesystem. Handles flat .md files and SKILL.md subdirs."""
    _auth(x_soma_key)
    skills_base = FLEET_DIR / "skills"
    result = []
    scan_statuses = ["active", "proposed", "disabled"] if status == "all" else [status]

    for status_label in scan_statuses:
        sdir = skills_base / status_label
        if not sdir.exists():
            continue

        # Scan both patterns: subdirectories with SKILL.md, and flat .md files
        for item in sorted(sdir.iterdir()):
            fm = None
            slug = item.name
            if item.is_dir():
                fm = _parse_skill_frontmatter(item)
                slug = item.name
            elif item.is_file() and item.suffix == ".md" and item.name != "README.md":
                # Flat .md format — parse frontmatter directly
                content = item.read_text()
                slug = item.stem
                if content.startswith("---"):
                    parts = content.split("---", 2)
                    if len(parts) >= 3:
                        try:
                            import yaml as _yaml
                            fm = _yaml.safe_load(parts[1]) or {}
                            fm["_body"] = parts[2].strip()
                        except Exception:
                            fm = {"name": slug, "description": ""}
                    else:
                        fm = {"name": slug, "description": ""}
                else:
                    fm = {"name": slug, "description": content[:100]}

            if fm is None:
                continue
            stats = _get_skill_stats(item) if item.is_dir() else {}
            result.append({
                "name": fm.get("name", slug),
                "slug": slug,
                "description": fm.get("description", ""),
                "version": fm.get("version", ""),
                "agent": fm.get("agent", ""),
                "tags": fm.get("tags", []),
                "status": status_label,
                "usage_count": stats.get("usage_count", 0),
                "last_success": stats.get("last_success"),
                "pitfalls_count": len(fm.get("pitfalls", [])),
            })

    return {"ok": True, "count": len(result), "skills": result}


@app.delete("/api/skills/{skill_name}")
def delete_skill(skill_name: str, x_soma_key: str = Header(default="")):
    """Archive a skill (move to /archived/ directory)."""
    _auth(x_soma_key)
    skills_base = FLEET_DIR / "skills"
    for status_label in ["active", "proposed"]:
        skill_path = skills_base / status_label / skill_name
        if skill_path.exists():
            archived_path = skills_base / "archived" / skill_name
            archived_path.parent.mkdir(parents=True, exist_ok=True)
            skill_path.rename(archived_path)
            return {"archived": True, "skill": skill_name, "from": status_label}
    raise HTTPException(status_code=404, detail=f"Skill not found: {skill_name}")


@app.post("/api/skills/{skill_name}/approve")
def approve_skill(skill_name: str, x_soma_key: str = Header(default="")):
    """Move a skill from proposed/ to active/."""
    _auth(x_soma_key)
    skills_base = FLEET_DIR / "skills"
    proposed_path = skills_base / "proposed" / skill_name
    if not proposed_path.exists():
        raise HTTPException(status_code=404, detail=f"Skill not found in proposed/: {skill_name}")
    active_path = skills_base / "active" / skill_name
    active_path.parent.mkdir(parents=True, exist_ok=True)
    proposed_path.rename(active_path)
    return {"approved": True, "skill": skill_name, "now": "active"}


# ── Plugins endpoints (LM-358) ────────────────────────────────────────────────

@app.get("/api/plugins")
def list_plugins(x_soma_key: str = Header(default="")):
    """List plugins from MCP hub /opt/ai-mcp/plugins/ via single batched SSH call."""
    _auth(x_soma_key)
    # First try main tools dir (not plugins subdir — tools live in /opt/ai-mcp/ directly)
    _PVM_HOST = os.environ.get("PVM_SSH_HOST", os.environ.get("PVM_SSH_HOST", ""))
    _TERMINUS_CT = os.environ.get("TERMINUS_CT", "214")
    try:
        # Single batched command: list files + tool counts + server.py registrations
        batch_cmd = (
            "python3 -c \""
            "import os, re, json; "
            "d='/opt/ai-mcp'; "
            "srv=open(d+'/server.py').read() if os.path.exists(d+'/server.py') else ''; "
            "plugins=[]; "
            "files=[f for f in os.listdir(d) if f.endswith('_tools.py') or (f.endswith('.py') and 'tools' in f and not f.startswith('_'))]; "
            "files+=[f for f in os.listdir(d+'/plugins') if f.endswith('.py') and not f.startswith('_')] if os.path.isdir(d+'/plugins') else []; "
            "[plugins.append({"
            "  'filename':f,"
            "  'size':os.path.getsize(d+'/'+f) if os.path.exists(d+'/'+f) else os.path.getsize(d+'/plugins/'+f),"
            "  'tool_count':len(re.findall(r'@mcp\\\\.tool', open(d+'/'+f if os.path.exists(d+'/'+f) else d+'/plugins/'+f).read())),"
            "  'enabled':f[:-3] in srv,"
            "  'location': 'plugins' if not os.path.exists(d+'/'+f) else 'core',"
            "}) for f in files]; "
            "print(json.dumps(plugins))"
            "\" 2>/dev/null"
        )
        r = subprocess.run(
            ["ssh", _PVM_HOST,
             f"pct exec {_TERMINUS_CT} -- {batch_cmd}"],
            capture_output=True, text=True, timeout=15
        )
        if r.returncode != 0 or not r.stdout.strip():
            raise RuntimeError(f"SSH failed: {r.stderr[:100]}")
        import json as _json
        raw = _json.loads(r.stdout.strip())
        plugins = [
            {
                "name": p["filename"].replace("_tools.py", "").replace(".py", ""),
                "filename": p["filename"],
                "size": p.get("size", 0),
                "tool_count": p.get("tool_count", 0),
                "enabled": p.get("enabled", False),
                "location": p.get("location", "core"),
            }
            for p in raw
        ]
        plugins.sort(key=lambda p: p["name"])
        return {"ok": True, "count": len(plugins), "plugins": plugins}
    except Exception as e:
        return {"ok": False, "count": 0, "plugins": [],
                "error": str(e)[:200],
                "hint": "Check PVM_SSH_HOST env var and SSH access to Terminus"}


# ── Vector API endpoints (SP.V6) ──────────────────────────────────────────────

_VECTOR_DIR = FLEET_DIR / "vector"
_VECTOR_STATE_FILE = _VECTOR_DIR / "state.json"
_VECTOR_HISTORY_FILE = _VECTOR_DIR / "history.json"
_VECTOR_CALX_FILE = _VECTOR_DIR / "calx.json"
_VECTOR_CONFIG_FILE = _VECTOR_DIR / "config.json"
_VECTOR_GUARDRAILS_FILE = _VECTOR_DIR / "guardrails.json"

# Pulse import for loop timers
_sys.path.insert(0, str(FLEET_DIR / "shared"))
try:
    import pulse as _pulse_mod
    _PULSE_MOD_OK = True
except ImportError:
    _PULSE_MOD_OK = False


def _read_json_file(path: Path, default=None):
    """Read a JSON file, return default if missing or corrupt."""
    try:
        if path.exists():
            return json.loads(path.read_text())
    except Exception:
        pass
    return default if default is not None else {}


def _write_json_file(path: Path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=2))


@app.get("/api/vector/status")
def vector_status(x_soma_key: str = Header(default="")):
    """Active loops, delegates, system health summary."""
    state = _read_json_file(_VECTOR_STATE_FILE, {})
    loops = state.get("loops", {})
    active = {lid: l for lid, l in loops.items() if l.get("status") == "running"}

    # Compute today's cost from history
    today = datetime.now(timezone.utc).date().isoformat()
    history = _read_json_file(_VECTOR_HISTORY_FILE, [])
    if isinstance(history, dict):
        history = history.get("tasks", [])
    today_cost = sum(t.get("cost_usd", 0) for t in history
                     if t.get("completed_at", "").startswith(today))

    # Gate pass rate (last 7 days)
    calx = _read_json_file(_VECTOR_CALX_FILE, [])
    if isinstance(calx, dict):
        calx = calx.get("events", [])
    total_iters = sum(t.get("iterations", 0) for t in history)
    total_corrections = len(calx)
    gate_pass_rate = round((1 - total_corrections / max(total_iters, 1)) * 100, 1) if total_iters > 0 else 100.0

    # Vector service status
    vector_pid = None
    vector_uptime = None
    try:
        result = subprocess.run(
            ["pgrep", "-f", "vector"],
            capture_output=True, text=True, timeout=3
        )
        if result.returncode == 0:
            vector_pid = result.stdout.strip().split("\n")[0]
    except Exception:
        pass

    cfg = _read_json_file(_VECTOR_CONFIG_FILE, {})

    return {
        "ok": True,
        "active_loop_count": len(active),
        "today_cost_usd": round(today_cost, 4),
        "gate_pass_rate": gate_pass_rate,
        "vector_running": vector_pid is not None,
        "vector_pid": vector_pid,
        "delegation_enabled": cfg.get("delegation_enabled", True),
        "scaffold_model": cfg.get("scaffold_model", "qwen3-8b"),
        "guardrail_count": len(_read_json_file(_VECTOR_GUARDRAILS_FILE, {}).get("system", [])),
    }


@app.get("/api/vector/loops")
def vector_loops(x_soma_key: str = Header(default="")):
    """List active loops with sub-process tree and elapsed times."""
    state = _read_json_file(_VECTOR_STATE_FILE, {})
    loops = state.get("loops", {})

    result = []
    for loop_id, loop in loops.items():
        elapsed = "?"
        if _PULSE_MOD_OK:
            elapsed = _pulse_mod.timer_elapsed(f"loop_{loop_id}")
        result.append({
            "id": loop_id,
            "task": loop.get("task", "Unknown task"),
            "status": loop.get("status", "unknown"),
            "phase": loop.get("phase", "PLAN"),
            "iteration": loop.get("iteration", 0),
            "max_iterations": loop.get("max_iterations", 25),
            "model": loop.get("model", "unknown"),
            "cost_usd": loop.get("cost_usd", 0),
            "budget_usd": loop.get("budget_usd", 2.0),
            "elapsed": elapsed,
            "source": loop.get("source", "soma"),
            "delegates": loop.get("delegates", []),
        })

    return {"ok": True, "loops": result, "count": len(result)}


@app.get("/api/vector/loops/{loop_id}/logs")
def vector_loop_logs(loop_id: str, lines: int = 20, x_soma_key: str = Header(default="")):
    """Live log tail for a specific loop."""
    log_file = _VECTOR_DIR / "logs" / f"{loop_id}.log"
    if not log_file.exists():
        return {"ok": True, "loop_id": loop_id, "lines": [],
                "note": "No log file found for this loop"}
    try:
        content = log_file.read_text()
        all_lines = content.strip().split("\n")
        return {"ok": True, "loop_id": loop_id,
                "lines": all_lines[-lines:], "total_lines": len(all_lines)}
    except Exception as e:
        return {"ok": False, "error": str(e)[:100]}


@app.post("/api/vector/submit")
async def vector_submit(request: Request, x_soma_key: str = Header(default="")):
    """Submit a new task to Vector. Returns loop ID."""
    data = await request.json()
    task = data.get("task", "").strip()
    if not task:
        raise HTTPException(400, "task required")

    loop_id = f"loop_{int(datetime.now(timezone.utc).timestamp())}"
    state = _read_json_file(_VECTOR_STATE_FILE, {"loops": {}})
    state.setdefault("loops", {})[loop_id] = {
        "task": task,
        "status": "queued",
        "phase": "PLAN",
        "iteration": 0,
        "max_iterations": data.get("max_iterations", 25),
        "model": data.get("model_tier", "auto"),
        "cost_usd": 0.0,
        "budget_usd": float(data.get("cost_budget", 2.0)),
        "repo": data.get("repo", "/opt/lumina-fleet"),
        "plane_project": data.get("plane_project"),
        "source": "soma",
        "submitted_at": datetime.now(timezone.utc).isoformat(),
        "delegates": [],
    }
    _write_json_file(_VECTOR_STATE_FILE, state)

    if _PULSE_MOD_OK:
        _pulse_mod.timer_start(f"loop_{loop_id}")

    return {"ok": True, "loop_id": loop_id, "status": "queued",
            "message": f"Loop {loop_id} queued. Vector will pick it up on next poll."}


@app.post("/api/vector/loops/{loop_id}/pause")
def vector_loop_pause(loop_id: str, x_soma_key: str = Header(default="")):
    state = _read_json_file(_VECTOR_STATE_FILE, {"loops": {}})
    if loop_id not in state.get("loops", {}):
        raise HTTPException(404, "Loop not found")
    state["loops"][loop_id]["status"] = "paused"
    _write_json_file(_VECTOR_STATE_FILE, state)
    return {"ok": True, "loop_id": loop_id, "status": "paused"}


@app.post("/api/vector/loops/{loop_id}/resume")
def vector_loop_resume(loop_id: str, x_soma_key: str = Header(default="")):
    state = _read_json_file(_VECTOR_STATE_FILE, {"loops": {}})
    if loop_id not in state.get("loops", {}):
        raise HTTPException(404, "Loop not found")
    state["loops"][loop_id]["status"] = "running"
    _write_json_file(_VECTOR_STATE_FILE, state)
    return {"ok": True, "loop_id": loop_id, "status": "running"}


@app.post("/api/vector/loops/{loop_id}/abort")
def vector_loop_abort(loop_id: str, x_soma_key: str = Header(default="")):
    state = _read_json_file(_VECTOR_STATE_FILE, {"loops": {}})
    if loop_id not in state.get("loops", {}):
        raise HTTPException(404, "Loop not found")
    state["loops"][loop_id]["status"] = "aborted"
    state["loops"][loop_id]["aborted_at"] = datetime.now(timezone.utc).isoformat()
    _write_json_file(_VECTOR_STATE_FILE, state)
    return {"ok": True, "loop_id": loop_id, "status": "aborted"}


@app.post("/api/vector/loops/{loop_id}/escalate")
async def vector_loop_escalate(loop_id: str, request: Request, x_soma_key: str = Header(default="")):
    """Surface loop problem to operator via Nexus and pause the loop."""
    state = _read_json_file(_VECTOR_STATE_FILE, {"loops": {}})
    if loop_id not in state.get("loops", {}):
        raise HTTPException(404, "Loop not found")
    data = await request.json() if await request.body() else {}
    loop = state["loops"][loop_id]
    loop["status"] = "paused"
    loop["escalated"] = True
    loop["escalation_message"] = data.get("message", "Operator review requested")
    _write_json_file(_VECTOR_STATE_FILE, state)
    return {"ok": True, "loop_id": loop_id, "status": "paused",
            "escalated": True, "message": loop["escalation_message"]}


@app.get("/api/vector/history")
def vector_history(limit: int = 50, x_soma_key: str = Header(default="")):
    """Completed tasks from Plane/SQLite."""
    history = _read_json_file(_VECTOR_HISTORY_FILE, [])
    if isinstance(history, dict):
        history = history.get("tasks", [])
    history = sorted(history, key=lambda t: t.get("completed_at", ""), reverse=True)
    return {"ok": True, "tasks": history[:limit], "total": len(history)}


@app.get("/api/vector/calx")
def vector_calx(limit: int = 100, trigger_type: str = "", x_soma_key: str = Header(default="")):
    """Calx behavioral correction history."""
    events = _read_json_file(_VECTOR_CALX_FILE, [])
    if isinstance(events, dict):
        events = events.get("events", [])
    if trigger_type:
        events = [e for e in events if e.get("trigger_type") == trigger_type]
    events = sorted(events, key=lambda e: e.get("timestamp", ""), reverse=True)
    return {"ok": True, "events": events[:limit], "total": len(events)}


@app.get("/api/vector/calx/stats")
def vector_calx_stats(x_soma_key: str = Header(default="")):
    """Calx trigger counts by type and period."""
    events = _read_json_file(_VECTOR_CALX_FILE, [])
    if isinstance(events, dict):
        events = events.get("events", [])

    today = datetime.now(timezone.utc).date().isoformat()
    seven_days_ago = (datetime.now(timezone.utc) - __import__('datetime').timedelta(days=7)).date().isoformat()

    counts = {"T1_TEST": 0, "T2_STYLE": 0, "T3_SECURITY": 0, "T4_PROMISE": 0}
    today_counts = dict(counts)
    week_counts = dict(counts)

    for e in events:
        t = e.get("trigger_type", "")
        if t in counts:
            counts[t] += 1
            ts = e.get("timestamp", "")[:10]
            if ts == today:
                today_counts[t] = today_counts.get(t, 0) + 1
            if ts >= seven_days_ago:
                week_counts[t] = week_counts.get(t, 0) + 1

    return {"ok": True, "all_time": counts, "today": today_counts, "last_7d": week_counts}


@app.get("/api/vector/prs")
def vector_prs(x_soma_key: str = Header(default="")):
    """Open PRs from Gitea with vector/ branch prefix."""
    gitea_url = os.environ.get("GITEA_URL", "")
    gitea_token = os.environ.get("GITEA_TOKEN", "")
    try:
        import urllib.request as _ur
        req = _ur.Request(
            f"{gitea_url}/api/v1/repos/search?limit=50&token={gitea_token}",
            headers={"Authorization": f"token {gitea_token}"} if gitea_token else {}
        )
        prs = []
        with _ur.urlopen(req, timeout=8) as r:
            repos = json.loads(r.read()).get("data", [])
        for repo in repos[:10]:
            try:
                pr_req = _ur.Request(
                    f"{gitea_url}/api/v1/repos/{repo['full_name']}/pulls?state=open&limit=20",
                    headers={"Authorization": f"token {gitea_token}"} if gitea_token else {}
                )
                with _ur.urlopen(pr_req, timeout=5) as r2:
                    repo_prs = json.loads(r2.read())
                for pr in repo_prs:
                    if pr.get("head", {}).get("label", "").startswith("vector/"):
                        prs.append({
                            "id": pr["number"],
                            "title": pr["title"],
                            "repo": repo["full_name"],
                            "branch": pr.get("head", {}).get("label", ""),
                            "url": pr.get("html_url", ""),
                            "created_at": pr.get("created_at", ""),
                            "files_changed": pr.get("changed_files", 0),
                        })
            except Exception:
                continue
        return {"ok": True, "prs": prs, "count": len(prs)}
    except Exception as e:
        return {"ok": False, "prs": [], "count": 0, "error": str(e)[:150],
                "note": "Set GITEA_URL and GITEA_TOKEN env vars"}


@app.get("/api/vector/models")
def vector_models(x_soma_key: str = Header(default="")):
    """Model performance data from history."""
    history = _read_json_file(_VECTOR_HISTORY_FILE, [])
    if isinstance(history, dict):
        history = history.get("tasks", [])

    model_stats = {}
    for task in history:
        model = task.get("model", "unknown")
        if model not in model_stats:
            model_stats[model] = {"iterations": 0, "passes": 0, "cost_usd": 0, "tasks": 0}
        model_stats[model]["iterations"] += task.get("iterations", 0)
        model_stats[model]["passes"] += task.get("gate_passes", 0)
        model_stats[model]["cost_usd"] += task.get("cost_usd", 0)
        model_stats[model]["tasks"] += 1

    result = []
    for model, stats in model_stats.items():
        iters = stats["iterations"]
        result.append({
            "model": model,
            "tasks": stats["tasks"],
            "total_iterations": iters,
            "gate_pass_rate": round(stats["passes"] / max(iters, 1) * 100, 1),
            "avg_cost_usd": round(stats["cost_usd"] / max(stats["tasks"], 1), 4),
            "total_cost_usd": round(stats["cost_usd"], 4),
        })

    return {"ok": True, "models": result}


@app.get("/api/vector/guardrails")
def vector_guardrails(x_soma_key: str = Header(default="")):
    """Loaded guardrails (system + project)."""
    data = _read_json_file(_VECTOR_GUARDRAILS_FILE, {"system": [], "project": []})
    return {"ok": True,
            "system": data.get("system", []),
            "project": data.get("project", []),
            "count": len(data.get("system", [])) + len(data.get("project", []))}


@app.put("/api/vector/config")
async def vector_config_update(request: Request, x_soma_key: str = Header(default="")):
    """Update Vector configuration."""
    data = await request.json()
    cfg = _read_json_file(_VECTOR_CONFIG_FILE, {})
    allowed_keys = {"default_model", "scaffold_model", "delegation_enabled",
                    "max_iterations", "cost_limit_usd", "calx_t1", "calx_t2",
                    "calx_t3", "calx_t4", "auto_escalate_threshold"}
    for k, v in data.items():
        if k in allowed_keys:
            cfg[k] = v
    _write_json_file(_VECTOR_CONFIG_FILE, cfg)
    return {"ok": True, "config": cfg}


@app.get("/api/vector/oauth")
def vector_oauth(x_soma_key: str = Header(default="")):
    """OAuth session status for each provider."""
    providers = [
        {"name": "Claude (Anthropic)", "key": "claude", "status": "not_configured",
         "note": "Use claude-code CLI for OAuth"},
        {"name": "ChatGPT (OpenAI)", "key": "openai", "status": "not_configured"},
        {"name": "Gemini (Google)", "key": "gemini", "status": "not_configured"},
        {"name": "OpenRouter", "key": "openrouter",
         "status": "configured" if os.environ.get("OPENROUTER_API_KEY") else "not_configured",
         "has_key": bool(os.environ.get("OPENROUTER_API_KEY"))},
        {"name": "Local Ollama", "key": "ollama",
         "status": "configured" if os.environ.get("OLLAMA_URL") else "checking"},
    ]
    return {"ok": True, "providers": providers}


@app.post("/api/vector/oauth/{provider}/refresh")
def vector_oauth_refresh(provider: str, x_soma_key: str = Header(default="")):
    """Trigger re-auth flow for a provider (returns redirect URL for OAuth popup)."""
    oauth_urls = {
        "claude": "https://claude.ai/login",
        "openai": "https://platform.openai.com/login",
        "gemini": "https://console.cloud.google.com/",
        "openrouter": "https://openrouter.ai/keys",
    }
    if provider not in oauth_urls:
        raise HTTPException(404, f"Unknown provider: {provider}")
    return {"ok": True, "provider": provider,
            "oauth_url": oauth_urls[provider],
            "note": "Open this URL to authenticate, then update the token in Infisical"}


def _get_soma_plane():
    """Return a PlaneClient instance from plane-helper (Soma read-only dashboard use)."""
    import sys as _s
    _s.path.insert(0, '/opt/plane-helper')
    from plane_helper import PlaneClient
    return PlaneClient()


@app.get("/api/vector/plane")
def vector_plane_summary(x_soma_key: str = Header(default="")):
    """Cross-project Plane summary via plane-helper."""
    _auth(x_soma_key)
    try:
        plane = _get_soma_plane()
        data = plane.get('/workspaces/moosenet/projects/')
        projects = data.get("results", data) if isinstance(data, dict) else data
        return {
            "ok": True,
            "projects": [
                {"id": p.get("id"), "name": p.get("name"), "identifier": p.get("identifier"),
                 "total_issues": p.get("total_issues", 0)}
                for p in projects
            ],
            "count": len(projects),
        }
    except Exception as e:
        return {"ok": False, "projects": [], "count": 0, "error": str(e)[:150]}


@app.get("/api/vector/plane/{project_id}")
def vector_plane_project(project_id: str, x_soma_key: str = Header(default="")):
    """Plane project metrics via plane-helper."""
    _auth(x_soma_key)
    try:
        plane = _get_soma_plane()
        items = plane.list_issues(project_id, state="all")
        done = sum(1 for i in items if i.get("state_detail", {}).get("group") == "done")
        in_progress = sum(1 for i in items if i.get("state_detail", {}).get("group") == "started")
        backlog = len(items) - done - in_progress
        return {
            "ok": True, "project_id": project_id,
            "total": len(items), "done": done, "in_progress": in_progress, "backlog": backlog,
            "top_open": [
                {"id": i.get("sequence_id"), "title": i.get("name"),
                 "priority": i.get("priority", "none")}
                for i in items if i.get("state_detail", {}).get("group") != "done"
            ][:5],
        }
    except Exception as e:
        return {"ok": False, "error": str(e)[:150]}


# ── Council endpoints (SP.C4 / SP.V7) ────────────────────────────────────────

_COUNCIL_SESSIONS: dict = {}  # session_id → session state (in-memory)

_COUNCIL_PERSONAS = [
    {"id": "architect",     "name": "Architect",      "prompt": "You are a senior systems architect. Prioritize scalability, maintainability, and clean boundaries."},
    {"id": "skeptic",       "name": "Skeptic",         "prompt": "You are a critical reviewer. Challenge assumptions, find edge cases, identify what could go wrong."},
    {"id": "pragmatist",    "name": "Pragmatist",      "prompt": "You are a pragmatic engineer. Prioritize shipping speed, simplicity, and what works today."},
    {"id": "security",      "name": "Security",        "prompt": "You are a security auditor. Evaluate attack surfaces, credential handling, and trust boundaries."},
    {"id": "user",          "name": "User",            "prompt": "You are the end user. Evaluate from the perspective of someone who will use this daily."},
    {"id": "cost",          "name": "Cost",            "prompt": "You are a cost optimizer. Evaluate inference spend, resource usage, and operational overhead."},
    {"id": "devils_advocate","name": "Devil's Advocate","prompt": "Argue against the proposed approach. What's the strongest case for doing something completely different?"},
]


@app.get("/api/council/personas")
def council_personas(x_soma_key: str = Header(default="")):
    """List available personas (built-in + custom)."""
    custom = _read_json_file(FLEET_DIR / "council" / "personas.json", [])
    return {"ok": True, "builtin": _COUNCIL_PERSONAS, "custom": custom,
            "all": _COUNCIL_PERSONAS + custom}


@app.post("/api/council/personas")
async def council_persona_create(request: Request, x_soma_key: str = Header(default="")):
    """Create or update a custom persona."""
    data = await request.json()
    if not data.get("id") or not data.get("name") or not data.get("prompt"):
        raise HTTPException(400, "id, name, and prompt required")
    personas_file = FLEET_DIR / "council" / "personas.json"
    personas = _read_json_file(personas_file, [])
    personas = [p for p in personas if p["id"] != data["id"]]
    personas.append({"id": data["id"], "name": data["name"], "prompt": data["prompt"]})
    _write_json_file(personas_file, personas)
    return {"ok": True, "persona": data}


@app.post("/api/council/start")
async def council_start(request: Request, x_soma_key: str = Header(default="")):
    """Start a deliberation session. Returns session_id for SSE streams."""
    import uuid
    data = await request.json()
    question = data.get("question", "").strip()
    if not question:
        raise HTTPException(400, "question required")

    session_id = str(uuid.uuid4())[:8]
    models = data.get("models", ["claude-sonnet-4-6"])
    mode = data.get("mode", "multi")  # multi or prism
    personas = data.get("personas", [])

    _COUNCIL_SESSIONS[session_id] = {
        "question": question,
        "models": models,
        "mode": mode,
        "personas": personas,
        "status": "running",
        "responses": {},
        "started_at": datetime.now(timezone.utc).isoformat(),
    }

    return {"ok": True, "session_id": session_id,
            "question": question, "models": models, "mode": mode,
            "stream_urls": [f"/api/council/{session_id}/stream/{m}" for m in models]}


@app.get("/api/council/{session_id}/stream/{model}")
def council_stream(session_id: str, model: str):
    """SSE stream for a model's response in a council session."""
    from fastapi.responses import StreamingResponse

    def event_generator():
        if session_id not in _COUNCIL_SESSIONS:
            yield f"data: {{\"error\": \"Session {session_id} not found\"}}\n\n"
            return
        session = _COUNCIL_SESSIONS[session_id]
        # Stub: in production, this streams from LiteLLM
        yield f"data: {{\"session_id\": \"{session_id}\", \"model\": \"{model}\", \"status\": \"streaming\"}}\n\n"
        yield f"data: {{\"text\": \"Council deliberation for: {session['question'][:50]}\"}}\n\n"
        yield f"data: {{\"text\": \"[Model {model} response would stream here via LiteLLM]\"}}\n\n"
        yield f"data: {{\"done\": true, \"model\": \"{model}\"}}\n\n"

    return StreamingResponse(event_generator(), media_type="text/event-stream")


@app.get("/api/council/{session_id}/synthesis")
def council_synthesis(session_id: str, x_soma_key: str = Header(default="")):
    """Mr. Wizard synthesis after all models respond."""
    if session_id not in _COUNCIL_SESSIONS:
        raise HTTPException(404, "Session not found")
    session = _COUNCIL_SESSIONS[session_id]
    return {
        "ok": True,
        "session_id": session_id,
        "question": session["question"],
        "models": session["models"],
        "synthesis": "Synthesis pending — all models must complete before synthesis is available.",
        "status": session["status"],
    }


@app.post("/api/council/{session_id}/save")
def council_save(session_id: str, x_soma_key: str = Header(default="")):
    """Archive a council session as a report."""
    if session_id not in _COUNCIL_SESSIONS:
        raise HTTPException(404, "Session not found")
    session = _COUNCIL_SESSIONS[session_id]
    reports_dir = FLEET_DIR / "council" / "reports"
    reports_dir.mkdir(parents=True, exist_ok=True)
    report_file = reports_dir / f"{session_id}.json"
    report_file.write_text(json.dumps(session, indent=2))
    return {"ok": True, "session_id": session_id, "saved_to": str(report_file)}


# ── Wiki endpoints ────────────────────────────────────────────────────────────

WIKI_DIR = FLEET_DIR / "soma" / "wiki"

def _render_markdown(md_text: str) -> str:
    """Convert markdown to HTML. Falls back to pre-wrapped text if markdown not available."""
    try:
        import markdown
        return markdown.markdown(md_text, extensions=['tables', 'fenced_code', 'toc'])
    except ImportError:
        import html
        return f'<pre>{html.escape(md_text)}</pre>'

def _wiki_nav() -> list[dict]:
    """Build navigation tree from wiki directory."""
    nav = []
    if not WIKI_DIR.exists():
        return nav
    for item in sorted(WIKI_DIR.rglob("*.md")):
        rel = item.relative_to(WIKI_DIR)
        parts = rel.parts
        title = item.stem.replace('-', ' ').replace('_', ' ').title()
        nav.append({
            "path": str(rel).replace("\\", "/"),
            "title": title,
            "section": parts[0] if len(parts) > 1 else "root",
            "depth": len(parts) - 1,
        })
    return nav

@app.get("/api/wiki/pages")
def wiki_pages(x_soma_key: str = Header(default="")):
    """List all wiki pages."""
    return {"pages": _wiki_nav(), "count": len(_wiki_nav())}

@app.get("/api/wiki/{path:path}")
def wiki_page(path: str, x_soma_key: str = Header(default="")):
    """Render a wiki page as HTML."""
    # Path traversal guard
    if ".." in path or path.startswith("/"):
        raise HTTPException(status_code=400, detail="Invalid path")
    page_file = WIKI_DIR / path
    if not page_file.exists():
        page_file = WIKI_DIR / (path + ".md")
    if not page_file.exists():
        raise HTTPException(status_code=404, detail=f"Wiki page not found: {path}")
    md_text = page_file.read_text()
    return {"path": path, "html": _render_markdown(md_text), "raw": md_text[:500]}

@app.get("/vector")
def vector_page(request: Request):
    return jinja2_templates.TemplateResponse(request, "vector.html", {"active_page": "vector"})

@app.get("/wiki")
def wiki_home_page(request: Request):
    return jinja2_templates.TemplateResponse(request, "wiki.html", {"active_page": "wiki"})

@app.get("/wiki/{path:path}")
def wiki_sub_page(request: Request, path: str):
    return jinja2_templates.TemplateResponse(request, "wiki.html", {"active_page": "wiki", "wiki_path": path})


# ── Synapse config (SY.5) ─────────────────────────────────────────────────────

SYNAPSE_DEFAULTS = {
    "enabled": False,
    "strength": "moderate",
    "quiet_hours": {"start": "22:00", "end": "08:00"},
    "max_messages_per_day": 3,
    "topic_blocklist": [],
    "topic_boosts": [],
    "agent_toggles": {},
}


def _get_synapse_config() -> dict:
    cfg = _load_constellation()
    synapse = cfg.get("synapse", {})
    merged = {**SYNAPSE_DEFAULTS, **synapse}
    return merged


def _set_synapse_config(data: dict):
    cfg = _load_constellation()
    cfg["synapse"] = data
    _save_constellation(cfg)


@app.get("/api/config/synapse")
def get_synapse_config(x_soma_key: str = Header(default="")):
    """Return current synapse section from constellation.yaml."""
    _auth(x_soma_key)
    return {"ok": True, "config": _get_synapse_config()}


@app.put("/api/config/synapse")
async def put_synapse_config(request: Request, x_soma_key: str = Header(default="")):
    """Write synapse section to constellation.yaml (full replace)."""
    _auth(x_soma_key)
    data = await request.json()
    # Validate strength
    if "strength" in data and data["strength"] not in ("gentle", "moderate", "enthusiastic"):
        raise HTTPException(400, "strength must be gentle, moderate, or enthusiastic")
    # Validate max_messages_per_day
    if "max_messages_per_day" in data:
        data["max_messages_per_day"] = max(1, min(20, int(data["max_messages_per_day"])))
    try:
        _set_synapse_config(data)
        return {"ok": True, "config": _get_synapse_config()}
    except Exception as e:
        return {"ok": False, "error": str(e)[:200]}


@app.get("/synapse")
def synapse_page(request: Request):
    """Synapse history + config page."""
    return jinja2_templates.TemplateResponse(request, "synapse.html", {"active_page": "synapse"})


# ── Synapse history (SY.6) ────────────────────────────────────────────────────

SYNAPSE_LOG_PATH = Path(os.environ.get("SYNAPSE_LOG_PATH", "/opt/lumina-fleet/synapse/gate_log.json"))
SYNAPSE_FEEDBACK_PATH = Path(os.environ.get("SYNAPSE_FEEDBACK_PATH", "/opt/lumina-fleet/synapse/feedback.json"))


@app.get("/api/synapse/history")
def synapse_history(
    limit: int = 50,
    trigger_type: str = "",
    x_soma_key: str = Header(default=""),
):
    """Return sent Synapse messages from gate_log.json."""
    _auth(x_soma_key)
    try:
        if not SYNAPSE_LOG_PATH.exists():
            return {"ok": True, "count": 0, "entries": [], "note": "No history yet"}
        log = json.loads(SYNAPSE_LOG_PATH.read_text())
        # Optionally filter by trigger type
        if trigger_type:
            log = [e for e in log if e.get("type") == trigger_type]
        # Newest first
        log = list(reversed(log))[:min(limit, 200)]
        # Attach feedback
        feedback = {}
        if SYNAPSE_FEEDBACK_PATH.exists():
            feedback = json.loads(SYNAPSE_FEEDBACK_PATH.read_text())
        for entry in log:
            entry_id = str(entry.get("ts", ""))
            entry["feedback"] = feedback.get(entry_id, None)
        return {"ok": True, "count": len(log), "entries": log}
    except Exception as e:
        return {"ok": False, "count": 0, "entries": [], "error": str(e)[:200]}


@app.post("/api/synapse/feedback")
async def synapse_feedback(request: Request, x_soma_key: str = Header(default="")):
    """Record thumbs-up/down feedback for a Synapse message. Triggers SY.7 adjustments."""
    _auth(x_soma_key)
    data = await request.json()
    entry_ts = str(data.get("ts", ""))
    vote = data.get("vote", "")  # "up" or "down"
    if not entry_ts or vote not in ("up", "down"):
        raise HTTPException(400, "ts and vote (up/down) required")

    try:
        feedback = {}
        if SYNAPSE_FEEDBACK_PATH.exists():
            feedback = json.loads(SYNAPSE_FEEDBACK_PATH.read_text())

        feedback[entry_ts] = {
            "vote": vote,
            "ts": time.time(),
            "trigger_type": data.get("trigger_type", ""),
        }

        # Keep last 1000 feedback entries
        if len(feedback) > 1000:
            oldest = sorted(feedback.keys())[:len(feedback) - 1000]
            for k in oldest:
                del feedback[k]

        SYNAPSE_FEEDBACK_PATH.parent.mkdir(parents=True, exist_ok=True)
        SYNAPSE_FEEDBACK_PATH.write_text(json.dumps(feedback, indent=2))

        # SY.7: trigger feedback loop adjustments
        _apply_synapse_feedback_loop(feedback)

        return {"ok": True, "ts": entry_ts, "vote": vote}
    except Exception as e:
        return {"ok": False, "error": str(e)[:200]}


# ── Synapse feedback loop (SY.7) ──────────────────────────────────────────────

def _apply_synapse_feedback_loop(feedback: dict):
    """
    Apply feedback loop rules to constellation.yaml synapse config:
    - 5+ ignored (no vote, older than 24h) → reduce strength one level
    - 3+ thumbs-down on same category → auto-block topic
    - thumbs-up → boost topic +0.1 (stored as boost entry)
    Called after every feedback write.
    """
    try:
        cfg = _get_synapse_config()
        now = time.time()
        day = 86400

        votes = list(feedback.values())
        downvotes = [v for v in votes if v.get("vote") == "down"]
        upvotes   = [v for v in votes if v.get("vote") == "up"]

        # Count thumbs-down by trigger_type
        type_downs: dict = {}
        for v in downvotes:
            t = v.get("trigger_type", "unknown")
            type_downs[t] = type_downs.get(t, 0) + 1

        blocklist = list(cfg.get("topic_blocklist", []))
        boosts = list(cfg.get("topic_boosts", []))
        changed = False

        # Auto-block categories with 3+ downvotes
        for ttype, count in type_downs.items():
            if count >= 3 and ttype not in blocklist and ttype != "unknown":
                blocklist.append(ttype)
                changed = True

        # Thumbs-up → add boost entry for trigger_type if not already present
        for v in upvotes:
            ttype = v.get("trigger_type", "")
            if ttype and ttype not in boosts:
                boosts.append(ttype)
                changed = True

        # Strength reduction: count entries with no vote (ignored) in last 24h
        if SYNAPSE_LOG_PATH.exists():
            try:
                log = json.loads(SYNAPSE_LOG_PATH.read_text())
                recent = [e for e in log if (now - e.get("ts", 0)) < day]
                ignored = [e for e in recent if str(e.get("ts", "")) not in feedback]
                strength_map = {"enthusiastic": "moderate", "moderate": "gentle", "gentle": "gentle"}
                current_strength = cfg.get("strength", "moderate")
                if len(ignored) >= 5 and current_strength != "gentle":
                    cfg["strength"] = strength_map[current_strength]
                    changed = True
            except Exception:
                pass

        if changed:
            cfg["topic_blocklist"] = blocklist
            cfg["topic_boosts"] = boosts
            _set_synapse_config(cfg)
    except Exception:
        pass  # Never fail due to feedback loop


# ── Security (SEC.10) ─────────────────────────────────────────────────────────

_SECURITY_DIR = FLEET_DIR / "security"


def _load_security_data() -> dict:
    """Load secrets registry + rotation state. Returns merged list."""
    import yaml as _yaml
    registry_path = _SECURITY_DIR / "secrets_registry.yaml"
    state_path = _SECURITY_DIR / "rotation_state.json"

    registry = []
    if registry_path.exists():
        try:
            data = _yaml.safe_load(registry_path.read_text())
            registry = data.get("secrets", [])
        except Exception:
            pass

    state = {}
    if state_path.exists():
        try:
            state = json.loads(state_path.read_text())
        except Exception:
            pass

    now = datetime.now(timezone.utc)
    WARN_THRESHOLD = 0.80
    results = []
    for secret in registry:
        name = secret["name"]
        max_age = secret.get("max_age_days", 365)
        entry = state.get(name, {})
        last_rotated_str = entry.get("last_rotated")

        if not last_rotated_str:
            results.append({
                "name": name,
                "description": secret.get("description", ""),
                "method": secret.get("method", "manual"),
                "max_age_days": max_age,
                "age_days": None,
                "days_remaining": None,
                "last_rotated": None,
                "status": "unknown",
                "services": secret.get("services", []),
                "auto_rotatable": secret.get("method") in ("random_hex_32", "gitea_api"),
                "manual_instructions": secret.get("manual_instructions", ""),
            })
            continue

        try:
            last_rotated = datetime.fromisoformat(last_rotated_str)
            age = now - last_rotated
            age_days = age.days
            days_remaining = max_age - age_days
            if age_days >= max_age:
                status = "expired"
            elif age_days >= max_age * WARN_THRESHOLD:
                status = "warn"
            else:
                status = "ok"
        except Exception:
            age_days = None
            days_remaining = None
            status = "unknown"
            last_rotated_str = None

        results.append({
            "name": name,
            "description": secret.get("description", ""),
            "method": secret.get("method", "manual"),
            "max_age_days": max_age,
            "age_days": age_days,
            "days_remaining": days_remaining,
            "last_rotated": last_rotated_str,
            "status": status,
            "services": secret.get("services", []),
            "auto_rotatable": secret.get("method") in ("random_hex_32", "gitea_api"),
            "manual_instructions": secret.get("manual_instructions", ""),
        })
    return {"secrets": results}


@app.get("/api/security/secrets")
def security_secrets(x_soma_key: str = Header(default="")):
    """Return all secrets with age, status, and rotation metadata."""
    _auth(x_soma_key)
    try:
        return {"ok": True, **_load_security_data()}
    except Exception as e:
        return {"ok": False, "error": str(e)[:200], "secrets": []}


@app.post("/api/security/rotate/{secret_name}")
async def security_rotate(secret_name: str, x_soma_key: str = Header(default="")):
    """Trigger rotation for an auto-rotatable secret."""
    _auth(x_soma_key)
    try:
        _sys.path.insert(0, str(_SECURITY_DIR))
        import rotation as _rotation
        import yaml as _yaml
        registry_path = _SECURITY_DIR / "secrets_registry.yaml"
        data = _yaml.safe_load(registry_path.read_text())
        secret = next((s for s in data.get("secrets", []) if s["name"] == secret_name), None)
        if not secret:
            raise HTTPException(404, f"Secret {secret_name} not in registry")
        if secret.get("method") not in ("random_hex_32", "gitea_api"):
            raise HTTPException(400, f"{secret_name} requires manual rotation")
        state = _rotation._load_state()
        ok = _rotation.rotate_secret(secret, state, force=True)
        _rotation._save_state(state)
        return {"ok": ok, "name": secret_name}
    except HTTPException:
        raise
    except Exception as e:
        return {"ok": False, "error": str(e)[:200]}


@app.get("/security")
def security_page(request: Request):
    """Secret rotation status page."""
    return jinja2_templates.TemplateResponse(request, "security.html", {"active_page": "security"})


@app.get("/council")
def council_page(request: Request):
    """Obsidian Circle — multi-model deliberation interface."""
    return jinja2_templates.TemplateResponse(request, "council.html", {"active_page": "council"})


if __name__ == "__main__":
    import uvicorn
    # Load env
    env_file = FLEET_DIR / "axon" / ".env"
    if env_file.exists():
        for line in env_file.read_text().splitlines():
            if "=" in line and not line.startswith("#"):
                k, v = line.split("=", 1)
                v = v.strip().strip('"').strip("'")
                os.environ.setdefault(k.strip(), v)

    port = int(os.environ.get("SOMA_PORT", 8082))
    print(f"[soma] Starting on port {port}")
    uvicorn.run(app, host="0.0.0.0", port=port, log_level="info")

