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
IRONCLAW_URL = os.environ.get("IRONCLAW_URL", "http://192.168.0.217:3001")
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
        litellm_url = os.environ.get("LITELLM_URL", "http://192.168.0.215:4000")
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
@app.get("/api/logs")
def recent_logs(source: str = "CT310", lines: int = 50, x_soma_key: str = Header(default="")):
    _auth(x_soma_key)
    try:
        ct_map = {"CT305": "305", "CT310": "310", "CT300": "300", "CT214": "214"}
        ct_id = ct_map.get(source, "310")
        result = subprocess.run(
            ["ssh", "root@192.168.0.104", f"pct exec {ct_id} -- journalctl -n {lines} --no-pager --output=short 2>/dev/null"],
            capture_output=True, text=True, timeout=15
        )
        return {"source": source, "lines": result.stdout.splitlines()[-lines:]}
    except Exception as e:
        return {"error": str(e)[:100]}

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

# ── Config read/write ─────────────────────────────────────────────────────────
@app.get("/api/config/{module}")
def get_module_config(module: str, x_soma_key: str = Header(default="")):
    _auth(x_soma_key)
    config_paths = {
        "myelin": FLEET_DIR / "myelin" / "myelin_config.yaml",
        "constellation": CONSTELLATION_YAML,
        "axon": FLEET_DIR / "axon" / ".env",  # Read-only, filtered
    }
    path = config_paths.get(module)
    if not path or not path.exists():
        return {"error": f"No config found for module: {module}"}
    try:
        if path.suffix == ".yaml":
            import yaml
            with open(path) as f:
                return yaml.safe_load(f) or {}
        else:
            # .env — return keys only, not values
            keys = []
            for line in path.read_text().splitlines():
                if "=" in line and not line.startswith("#"):
                    keys.append(line.split("=")[0].strip())
            return {"module": module, "configured_keys": keys}
    except Exception as e:
        return {"error": str(e)}

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
    gitea_url = 'http://192.168.0.223:3000'
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
    checks.append(_ping('LiteLLM', 'http://192.168.0.215:4000/health'))
    checks.append(_ping('Terminus MCP', 'http://192.168.0.226:8080/health'))

    try:
        s = socket.create_connection(('192.168.0.104', 5432), timeout=3)
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
        ["ssh", "root@192.168.0.104",
         "pct exec 305 -- /usr/local/bin/ironclaw --version 2>&1"],
        capture_output=True, text=True, timeout=6
    )
    ver = r.stdout.strip() or r.stderr.strip()
    return {"value": ver or "unknown", "ok": bool(ver)}


def _check_active_agents():
    agents = {}
    r = subprocess.run(
        ["ssh", "root@192.168.0.104", "pct exec 310 -- systemctl is-active axon.service 2>/dev/null"],
        capture_output=True, text=True, timeout=6
    )
    agents["axon"] = r.stdout.strip() == "active"
    r2 = subprocess.run(
        ["ssh", "root@192.168.0.104", "pct exec 310 -- systemctl is-active sentinel-health.timer 2>/dev/null"],
        capture_output=True, text=True, timeout=6
    )
    agents["sentinel"] = r2.stdout.strip() == "active"
    r3 = subprocess.run(
        ["ssh", "root@192.168.0.104", "pct exec 310 -- pgrep -f briefing.py 2>/dev/null"],
        capture_output=True, text=True, timeout=6
    )
    agents["vigil"] = bool(r3.stdout.strip())
    active_count = sum(1 for v in agents.values() if v)
    return {"value": agents, "ok": active_count > 0, "active_count": active_count}


def _check_nexus_messages():
    try:
        import psycopg2
        db_host = os.environ.get("INBOX_DB_HOST", "192.168.0.108")
        db_user = os.environ.get("INBOX_DB_USER", "lumina")
        db_pass = os.environ.get("INBOX_DB_PASS", "")
        conn = psycopg2.connect(
            host=db_host, dbname="lumina_inbox",
            user=db_user, password=db_pass, connect_timeout=3
        )
        cur = conn.cursor()
        cur.execute("SELECT COUNT(*) FROM inbox_messages WHERE status = 'pending'")
        count = cur.fetchone()[0]
        conn.close()
        return {"value": count, "ok": True}
    except Exception as e:
        return {"value": None, "ok": False, "error": str(e)[:80]}


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
    litellm_url = os.environ.get("LITELLM_URL", "http://192.168.0.215:4000")
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
        ["ssh", "root@192.168.0.104",
         "pct exec 306 -- systemctl is-active matrix-bridge.service 2>/dev/null"],
        capture_output=True, text=True, timeout=6
    )
    active = r.stdout.strip() == "active"
    return {"value": "running" if active else "stopped", "ok": active}


def _check_refractor_categories():
    """Count actual keyword categories: lines matching '    "word": {' at 4-space indent."""
    r = subprocess.run(
        ["ssh", "root@192.168.0.104",
         r'pct exec 305 -- grep -cP "^    \"[a-z_]+\": \{" /usr/local/bin/llm-proxy.py 2>/dev/null'],
        capture_output=True, text=True, timeout=6
    )
    count_str = r.stdout.strip()
    try:
        count = int(count_str)
    except ValueError:
        count = 0
    return {"value": count, "ok": count > 10}  # expect 32+


def _check_plane_projects():
    plane_url = os.environ.get("PLANE_URL", "http://192.168.0.232")
    plane_token = os.environ.get("PLANE_API_TOKEN", "")
    req = _urlreq_status.Request(
        f"{plane_url}/api/v1/workspaces/moosenet/projects/",
        headers={"X-API-Key": plane_token}
    )
    with _urlreq_status.urlopen(req, timeout=5) as r:
        data = json.load(r)
    count = data.get("count", len(data.get("results", []))) if isinstance(data, dict) else len(data)
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

    failed = sum(1 for c in checks.values() if not c.get("ok", False))
    overall = "ok" if failed == 0 else ("degraded" if failed <= 2 else "critical")
    result = {"status": overall, "checks": checks, "timestamp": datetime.now(timezone.utc).isoformat()}

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
def root_page(request: Request):
    status_data = get_status()
    return jinja2_templates.TemplateResponse(
        request, "status.html",
        {"status": status_data, "active_page": "status"}
    )

@app.get("/status")
def status_page(request: Request):
    """Render status dashboard with live data via Jinja2."""
    status_data = get_status()
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
    return jinja2_templates.TemplateResponse(request, "plugins.html", {"active_page": "plugins"})

@app.get("/sessions")
def sessions_page(request: Request):
    return jinja2_templates.TemplateResponse(request, "sessions.html", {"active_page": "sessions"})

@app.get("/cron")
def cron_page(request: Request):
    return jinja2_templates.TemplateResponse(request, "cron.html", {"active_page": "cron"})

@app.get("/logs")
def logs_page(request: Request):
    return jinja2_templates.TemplateResponse(request, "logs.html", {"active_page": "logs"})


@app.get("/api/sessions")
def list_sessions(limit: int = 20, x_soma_key: str = Header(default="")):
    _auth(x_soma_key)
    try:
        db_path = Path("/root/.ironclaw/ironclaw.db")
        if db_path.exists():
            import sqlite3
            conn = sqlite3.connect(str(db_path)); conn.row_factory = sqlite3.Row
            try:
                rows = conn.execute("SELECT id, channel, created_at FROM sessions ORDER BY created_at DESC LIMIT ?", (limit,)).fetchall()
                sessions = [dict(r) for r in rows]
            except Exception: sessions = []
            conn.close()
            return {"count": len(sessions), "sessions": sessions}
        return {"count": 0, "sessions": [], "note": "IronClaw DB not on CT310"}
    except Exception as e:
        return {"count": 0, "error": str(e)[:100]}


@app.get("/api/timers")
def list_timers(x_soma_key: str = Header(default="")):
    _auth(x_soma_key)
    try:
        result = subprocess.run(["systemctl", "list-timers", "--all", "--no-pager"], capture_output=True, text=True, timeout=10)
        timers = []
        for line in result.stdout.splitlines():
            if ".timer" in line and "UNIT" not in line and "timers listed" not in line:
                name = next((p for p in line.split() if p.endswith(".timer")), None)
                if name: timers.append({"name": name, "raw": line[:120].strip()})
        return {"count": len(timers), "timers": timers}
    except Exception as e:
        return {"count": 0, "timers": [], "error": str(e)[:100]}


@app.post("/api/timers/{timer_name}/trigger")
def trigger_timer(timer_name: str, x_soma_key: str = Header(default="")):
    _auth(x_soma_key)
    safe = timer_name.replace(".timer",".service").replace("/","").replace("..","")
    if not safe.endswith(".service"): safe += ".service"
    try:
        result = subprocess.run(["systemctl", "start", safe], capture_output=True, text=True, timeout=15)
        return {"triggered": result.returncode == 0, "service": safe}
    except Exception as e:
        return {"triggered": False, "error": str(e)[:100]}


@app.get("/status/grid", response_class=HTMLResponse)
def status_grid(request: Request):
    """Return just the grid cards HTML fragment for HTMX polling."""
    status_data = get_status()
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
def list_skills(x_soma_key: str = Header(default="")):
    """List all active and proposed skills from CT310 filesystem."""
    _auth(x_soma_key)
    skills_base = FLEET_DIR / "skills"
    result = []
    for status_label in ["active", "proposed", "archived"]:
        sdir = skills_base / status_label
        if not sdir.exists():
            continue
        for skill_path in sorted(sdir.iterdir()):
            if not skill_path.is_dir():
                continue
            fm = _parse_skill_frontmatter(skill_path)
            if fm is None:
                continue
            stats = _get_skill_stats(skill_path)
            result.append({
                "name": fm.get("name", skill_path.name),
                "slug": skill_path.name,
                "description": fm.get("description", ""),
                "version": fm.get("version", ""),
                "agent": fm.get("agent", ""),
                "tags": fm.get("tags", []),
                "status": status_label,
                "usage_count": stats.get("usage_count", 0),
                "last_success": stats.get("last_success"),
            })
    return {"count": len(result), "skills": result}


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
    """List plugins from CT214 /opt/ai-mcp/plugins/ via SSH."""
    _auth(x_soma_key)
    try:
        # CT214 is on PVM (192.168.0.103)
        r = subprocess.run(
            ["ssh", "root@192.168.0.103",
             "pct exec 214 -- find /opt/ai-mcp/plugins/ -maxdepth 1 -name '*.py' -not -name '__*' -printf '%f %s\\n' 2>/dev/null"],
            capture_output=True, text=True, timeout=10
        )
        plugins = []
        for line in r.stdout.strip().splitlines():
            parts = line.split()
            if len(parts) < 2:
                continue
            fname = parts[0]
            try:
                size = int(parts[1])
            except ValueError:
                size = 0
            slug = fname[:-3]

            r2 = subprocess.run(
                ["ssh", "root@192.168.0.103",
                 f"pct exec 214 -- grep -c '@mcp.tool' /opt/ai-mcp/plugins/{fname} 2>/dev/null || echo 0"],
                capture_output=True, text=True, timeout=5
            )
            try:
                tool_count = int(r2.stdout.strip())
            except ValueError:
                tool_count = 0

            r3 = subprocess.run(
                ["ssh", "root@192.168.0.103",
                 f"pct exec 214 -- grep -c '{slug}' /opt/ai-mcp/server.py 2>/dev/null || echo 0"],
                capture_output=True, text=True, timeout=5
            )
            try:
                enabled = int(r3.stdout.strip()) > 0
            except ValueError:
                enabled = False

            plugins.append({
                "name": slug,
                "filename": fname,
                "size": size,
                "tool_count": tool_count,
                "enabled": enabled,
            })
        return {"count": len(plugins), "plugins": plugins}
    except Exception as e:
        return {"count": 0, "plugins": [], "error": str(e)[:120]}



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

