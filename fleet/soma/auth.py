"""
auth.py — Soma authentication module (SP.2)
fleet/soma/auth.py

Login flow: username+password → bcrypt verify → JWT cookie (HttpOnly, 24h)
Backward compat: X-Soma-Key header still accepted (API clients)

Usage in main.py:
    from auth import SomaAuth, require_auth, optional_auth
    soma_auth = SomaAuth()

    @app.get("/api/data")
    async def data(user=Depends(require_auth)):
        return {"user": user["username"]}

    # Or simple header check (existing endpoints):
    @app.get("/api/data")
    async def data(creds=Depends(optional_auth)):
        if not creds:
            raise HTTPException(401)
"""

import hashlib
import hmac
import json
import os
import secrets
import time
from pathlib import Path
from typing import Optional

try:
    import bcrypt
    _BCRYPT = True
except ImportError:
    _BCRYPT = False

from fastapi import Cookie, Depends, Header, HTTPException, Request, Response
from fastapi.responses import RedirectResponse

# ── Config ──────────────────────────────────────────────────────────────────

FLEET_DIR   = Path(os.environ.get("FLEET_DIR", "/opt/lumina-fleet"))
AUTH_DB     = FLEET_DIR / "soma" / "auth.db"
JWT_SECRET  = os.environ.get("SOMA_JWT_SECRET", os.environ.get("SOMA_SECRET_KEY", "soma-dev-key"))
JWT_TTL     = 86400          # 24 hours
LOCKOUT_TTL = 30             # seconds after 5 failed attempts
MAX_FAILS   = 5

# Legacy header key for API backward compatibility
SOMA_KEY    = os.environ.get("SOMA_SECRET_KEY", "soma-dev-key")


# ── Simple JWT (no external deps) ────────────────────────────────────────────

def _b64url(data: bytes) -> str:
    import base64
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()

def _jwt_sign(payload: dict) -> str:
    header  = _b64url(json.dumps({"alg": "HS256", "typ": "JWT"}).encode())
    body    = _b64url(json.dumps(payload).encode())
    sig_input = f"{header}.{body}".encode()
    sig     = hmac.new(JWT_SECRET.encode(), sig_input, hashlib.sha256).digest()
    return f"{header}.{body}.{_b64url(sig)}"

def _jwt_verify(token: str) -> Optional[dict]:
    try:
        parts = token.split(".")
        if len(parts) != 3:
            return None
        header, body, sig = parts
        sig_input = f"{header}.{body}".encode()
        expected  = hmac.new(JWT_SECRET.encode(), sig_input, hashlib.sha256).digest()
        if not hmac.compare_digest(_b64url(expected).encode(), sig.encode()):
            return None
        import base64
        payload = json.loads(base64.urlsafe_b64decode(body + "=="))
        if payload.get("exp", 0) < time.time():
            return None
        return payload
    except Exception:
        return None


# ── Auth DB (flat JSON file) ─────────────────────────────────────────────────

class SomaAuth:

    def __init__(self):
        self._db: Optional[dict] = None
        self._fail_counts: dict = {}   # username → (count, first_fail_ts)

    def _load(self) -> dict:
        if AUTH_DB.exists():
            try:
                return json.loads(AUTH_DB.read_text())
            except Exception:
                pass
        return {"users": {}, "version": 1}

    def _save(self, db: dict):
        AUTH_DB.parent.mkdir(parents=True, exist_ok=True)
        AUTH_DB.write_text(json.dumps(db, indent=2))

    def _db_get(self) -> dict:
        return self._load()

    def is_configured(self) -> bool:
        db = self._db_get()
        return bool(db.get("users"))

    def create_user(self, username: str, password: str, role: str = "admin",
                    display_name: str = "") -> bool:
        """Create a new user. Returns False if username already exists."""
        if len(password) < 8:
            raise ValueError("Password must be at least 8 characters")
        db = self._db_get()
        if username in db["users"]:
            return False
        if _BCRYPT:
            pw_hash = bcrypt.hashpw(password.encode(), bcrypt.gensalt()).decode()
        else:
            # Fallback: PBKDF2 (no external deps)
            import hashlib
            salt = secrets.token_hex(16)
            pw_hash = "pbkdf2:" + salt + ":" + hashlib.pbkdf2_hmac(
                "sha256", password.encode(), salt.encode(), 260000
            ).hex()
        db["users"][username] = {
            "password_hash": pw_hash,
            "role": role,
            "display_name": display_name or username,
            "created_at": time.time(),
        }
        self._save(db)
        return True

    def verify_password(self, username: str, password: str) -> Optional[dict]:
        """Verify credentials. Returns user record or None."""
        db = self._db_get()
        user = db.get("users", {}).get(username)
        if not user:
            return None
        pw_hash = user["password_hash"]
        ok = False
        if _BCRYPT and not pw_hash.startswith("pbkdf2:"):
            ok = bcrypt.checkpw(password.encode(), pw_hash.encode())
        else:
            # PBKDF2 fallback
            import hashlib
            parts = pw_hash.split(":")
            if len(parts) == 3:
                _, salt, stored = parts
                candidate = hashlib.pbkdf2_hmac(
                    "sha256", password.encode(), salt.encode(), 260000
                ).hex()
                ok = hmac.compare_digest(stored, candidate)
        if ok:
            self._fail_counts.pop(username, None)
            return {"username": username, **user}
        return None

    def is_locked_out(self, username: str) -> bool:
        entry = self._fail_counts.get(username)
        if not entry:
            return False
        count, first_ts = entry
        if time.time() - first_ts > LOCKOUT_TTL:
            del self._fail_counts[username]
            return False
        return count >= MAX_FAILS

    def record_fail(self, username: str):
        entry = self._fail_counts.get(username)
        if not entry or time.time() - entry[1] > LOCKOUT_TTL:
            self._fail_counts[username] = (1, time.time())
        else:
            self._fail_counts[username] = (entry[0] + 1, entry[1])

    def issue_token(self, user: dict) -> str:
        payload = {
            "sub": user["username"],
            "role": user.get("role", "admin"),
            "display_name": user.get("display_name", user["username"]),
            "exp": time.time() + JWT_TTL,
            "iat": time.time(),
        }
        return _jwt_sign(payload)

    def get_users(self) -> list:
        db = self._db_get()
        return [
            {"username": u, "role": d["role"], "display_name": d["display_name"],
             "created_at": d["created_at"]}
            for u, d in db.get("users", {}).items()
        ]

    def delete_user(self, username: str) -> bool:
        db = self._db_get()
        if username not in db.get("users", {}):
            return False
        del db["users"][username]
        self._save(db)
        return True

    def set_role(self, username: str, role: str) -> bool:
        db = self._db_get()
        if username not in db.get("users", {}):
            return False
        db["users"][username]["role"] = role
        self._save(db)
        return True


# ── Singleton ────────────────────────────────────────────────────────────────

_soma_auth = SomaAuth()


# ── FastAPI Dependencies ─────────────────────────────────────────────────────

def _extract_user(
    soma_session: str = Cookie(default=""),
    x_soma_key: str = Header(default=""),
) -> Optional[dict]:
    """Try JWT cookie first, then legacy X-Soma-Key header."""
    # JWT cookie
    if soma_session:
        payload = _jwt_verify(soma_session)
        if payload:
            return {"username": payload["sub"], "role": payload.get("role", "admin"),
                    "display_name": payload.get("display_name", payload["sub"]),
                    "auth_method": "jwt"}
    # Legacy API key header (backward compat for existing API clients)
    if x_soma_key and x_soma_key == SOMA_KEY:
        return {"username": "admin", "role": "admin", "display_name": "Admin",
                "auth_method": "header"}
    return None


def require_auth(
    soma_session: str = Cookie(default=""),
    x_soma_key: str = Header(default=""),
) -> dict:
    """Dependency: require authenticated user or raise 401."""
    user = _extract_user(soma_session, x_soma_key)
    if not user:
        raise HTTPException(status_code=401, detail="Authentication required")
    return user


def optional_auth(
    soma_session: str = Cookie(default=""),
    x_soma_key: str = Header(default=""),
) -> Optional[dict]:
    """Dependency: return user if authenticated, None otherwise."""
    return _extract_user(soma_session, x_soma_key)


# ── Login route helpers ───────────────────────────────────────────────────────

def add_auth_routes(app, templates):
    """Register /login, /logout, /api/auth/* routes and redirect middleware."""
    from fastapi import Form
    from fastapi.responses import HTMLResponse

    @app.get("/login")
    async def login_page(request: Request, error: str = ""):
        if not _soma_auth.is_configured():
            return RedirectResponse("/setup", status_code=302)
        return templates.TemplateResponse("login.html", {"request": request, "error": error})

    @app.post("/login")
    async def login_submit(
        request: Request,
        response: Response,
        username: str = Form(...),
        password: str = Form(...),
    ):
        if not _soma_auth.is_configured():
            return RedirectResponse("/setup", status_code=302)

        if _soma_auth.is_locked_out(username):
            return templates.TemplateResponse(
                "login.html",
                {"request": request, "error": f"Account locked. Try again in {LOCKOUT_TTL}s."},
                status_code=429,
            )

        user = _soma_auth.verify_password(username, password)
        if not user:
            _soma_auth.record_fail(username)
            return templates.TemplateResponse(
                "login.html",
                {"request": request, "error": "Invalid credentials"},
                status_code=401,
            )

        token = _soma_auth.issue_token(user)
        role  = user.get("role", "admin")
        dest  = {"admin": "/status", "agent": "/agent-dashboard",
                 "member": "/home", "guest": "/chat"}.get(role, "/status")

        resp = RedirectResponse(dest, status_code=302)
        resp.set_cookie(
            "soma_session", token,
            httponly=True, samesite="lax",
            max_age=JWT_TTL, path="/",
        )
        return resp

    @app.post("/api/auth/logout")
    async def logout(response: Response):
        resp = RedirectResponse("/login", status_code=302)
        resp.delete_cookie("soma_session", path="/")
        return resp

    @app.get("/api/auth/me")
    async def me(user: dict = Depends(require_auth)):
        return {k: v for k, v in user.items() if k != "password_hash"}

    @app.get("/api/auth/users")
    async def list_users(user: dict = Depends(require_auth)):
        if user.get("role") != "admin":
            raise HTTPException(403, "Admin only")
        return {"users": _soma_auth.get_users()}

    @app.post("/api/auth/users")
    async def create_user(
        request: Request,
        user: dict = Depends(require_auth),
    ):
        if user.get("role") != "admin":
            raise HTTPException(403, "Admin only")
        data = await request.json()
        try:
            ok = _soma_auth.create_user(
                data["username"], data["password"],
                data.get("role", "member"), data.get("display_name", "")
            )
            return {"created": ok}
        except ValueError as e:
            raise HTTPException(400, str(e))

    @app.delete("/api/auth/users/{username}")
    async def delete_user(username: str, user: dict = Depends(require_auth)):
        if user.get("role") != "admin":
            raise HTTPException(403, "Admin only")
        return {"deleted": _soma_auth.delete_user(username)}

    # First-run: create initial admin account
    @app.post("/api/auth/setup")
    async def setup_admin(request: Request):
        """Only works when no users exist yet."""
        if _soma_auth.is_configured():
            raise HTTPException(403, "Already configured")
        data = await request.json()
        try:
            _soma_auth.create_user(
                data["username"], data["password"], "admin",
                data.get("display_name", "")
            )
            return {"created": True}
        except ValueError as e:
            raise HTTPException(400, str(e))
