#!/usr/bin/env python3
"""
plane_helper.py — Shared Plane CE API library
MooseNet · Document 28 implementation

Zero-dep rate-limited client for Plane CE.
Serializes all requests via filesystem lock so multiple agents
on the same machine can't race each other into 429s.

Usage (CLI):
  python3 plane_helper.py GET /workspaces/moosenet/projects/
  python3 plane_helper.py POST /workspaces/moosenet/projects/{pid}/issues/ '{"name":"..."}'
  python3 plane_helper.py create-issue {project_id} 'title' --priority high
  python3 plane_helper.py list-issues {project_id}
  python3 plane_helper.py mark-done {project_id} {issue_id}
  python3 plane_helper.py batch-create {project_id} /path/to/issues.json
  python3 plane_helper.py stats

Usage (Python):
  from plane_helper import PlaneClient
  plane = PlaneClient()
  projects = plane.get('/workspaces/moosenet/projects/')
  issue = plane.create_issue(project_id, 'Fix the thing', priority='high')
"""

import fcntl
import json
import logging
import os
import sys
import time
from pathlib import Path
from typing import Any, Optional

try:
    import requests
except ImportError:
    print("Error: 'requests' not installed. Run: pip install requests", file=sys.stderr)
    sys.exit(1)

# ── Config ──────────────────────────────────────────────────────────────────

LOCK_FILE   = "/tmp/plane-helper.lock"
LAST_FILE   = "/tmp/plane-helper.last"
CACHE_FILE  = "/tmp/plane-helper-cache.json"
LOG_FILE    = "/tmp/plane-helper.log"
ENV_FILE    = Path(__file__).parent / ".env"
LOG_MAXLINES = 1000

def _load_env():
    """Load .env from helper dir if present."""
    if ENV_FILE.exists():
        for line in ENV_FILE.read_text().splitlines():
            line = line.strip()
            if line and not line.startswith('#') and '=' in line:
                k, _, v = line.partition('=')
                os.environ.setdefault(k.strip(), v.strip())

_load_env()

BASE_URL     = os.environ.get("PLANE_BASE_URL", "http://192.168.0.232/api/v1")
TOKEN        = os.environ.get("PLANE_API_TOKEN", "")
WORKSPACE    = os.environ.get("PLANE_WORKSPACE", "moosenet")
PLANE_RPM    = int(os.environ.get("PLANE_RPM", "60"))
RATE_SHARE   = int(os.environ.get("PLANE_RATE_SHARE", "3"))
CACHE_TTL    = 5  # seconds for GET cache

# Effective min interval per machine
MIN_INTERVAL = 60.0 / (PLANE_RPM / RATE_SHARE)  # e.g. 60/(60/3) = 3.0s

# ── Logging ─────────────────────────────────────────────────────────────────

logging.basicConfig(level=logging.WARNING)
_log = logging.getLogger("plane_helper")

def _write_log(method: str, path: str, status: int, wait_ms: int):
    """Append a line to the rolling log (max LOG_MAXLINES)."""
    try:
        ts = time.strftime("%Y-%m-%dT%H:%M:%S")
        line = f"{ts} {method} {path} {status} wait={wait_ms}ms\n"
        # Rolling log
        existing = []
        try:
            existing = open(LOG_FILE).readlines()
        except FileNotFoundError:
            pass
        if len(existing) >= LOG_MAXLINES:
            existing = existing[-(LOG_MAXLINES - 1):]
        existing.append(line)
        open(LOG_FILE, "w").writelines(existing)
    except Exception:
        pass

# ── Rate Limiter ─────────────────────────────────────────────────────────────

class RateLimiter:
    """Filesystem-based rate limiter. Works across processes on same machine."""

    def acquire(self):
        """Block until it's safe to make a request."""
        lock_fd = open(LOCK_FILE, "w")
        fcntl.flock(lock_fd, fcntl.LOCK_EX)
        try:
            wait_ms = self._wait_if_needed()
            return lock_fd, wait_ms
        except Exception:
            fcntl.flock(lock_fd, fcntl.LOCK_UN)
            lock_fd.close()
            raise

    def release(self, lock_fd):
        try:
            Path(LAST_FILE).write_text(str(time.time()))
        finally:
            fcntl.flock(lock_fd, fcntl.LOCK_UN)
            lock_fd.close()

    def _wait_if_needed(self) -> int:
        """Sleep if necessary. Returns ms waited."""
        try:
            last = float(Path(LAST_FILE).read_text())
        except (FileNotFoundError, ValueError):
            return 0
        elapsed = time.time() - last
        if elapsed < MIN_INTERVAL:
            delay = MIN_INTERVAL - elapsed
            time.sleep(delay)
            return int(delay * 1000)
        return 0


_rate_limiter = RateLimiter()

# ── GET Cache ────────────────────────────────────────────────────────────────

def _cache_get(key: str) -> Optional[Any]:
    try:
        data = json.loads(Path(CACHE_FILE).read_text())
        entry = data.get(key)
        if entry and (time.time() - entry["ts"]) < CACHE_TTL:
            return entry["value"]
    except Exception:
        pass
    return None

def _cache_set(key: str, value: Any):
    try:
        try:
            data = json.loads(Path(CACHE_FILE).read_text())
        except Exception:
            data = {}
        data[key] = {"ts": time.time(), "value": value}
        Path(CACHE_FILE).write_text(json.dumps(data))
    except Exception:
        pass

# ── HTTP Core ────────────────────────────────────────────────────────────────

def _make_request(method: str, path: str, body: Optional[dict] = None,
                  retries: int = 3) -> dict:
    """Rate-limited HTTP request with retry. Returns response dict."""
    if not TOKEN:
        return {"ok": False, "error": "PLANE_API_TOKEN not set", "status_code": 0}

    url = BASE_URL.rstrip("/") + "/" + path.lstrip("/")
    headers = {
        "X-API-Key": TOKEN,
        "Content-Type": "application/json",
    }

    # GET cache
    if method.upper() == "GET":
        cached = _cache_get(path)
        if cached is not None:
            return {"ok": True, "data": cached, "cached": True, "status_code": 200}

    backoff = [2, 5, 15]

    for attempt in range(retries):
        lock_fd, wait_ms = _rate_limiter.acquire()
        try:
            resp = requests.request(
                method.upper(), url, headers=headers,
                json=body, timeout=30
            )
            status = resp.status_code
            _rate_limiter.release(lock_fd)
            lock_fd = None

            _write_log(method, path, status, wait_ms)

            if status == 401 or status == 403:
                return {"ok": False, "error": f"Auth failed ({status})",
                        "status_code": status}

            if status == 429:
                retry_after = int(resp.headers.get("Retry-After", backoff[min(attempt, 2)]))
                print(f"Rate limited. Waiting {retry_after}s...", file=sys.stderr)
                time.sleep(retry_after)
                continue

            if status >= 500:
                delay = backoff[min(attempt, 2)]
                print(f"Server error {status}. Retry in {delay}s...", file=sys.stderr)
                time.sleep(delay)
                continue

            try:
                data = resp.json()
            except Exception:
                data = resp.text

            if method.upper() == "GET" and status < 300:
                _cache_set(path, data)

            return {"ok": status < 300, "data": data, "status_code": status}

        except requests.exceptions.RequestException as e:
            if lock_fd:
                _rate_limiter.release(lock_fd)
                lock_fd = None
            delay = backoff[min(attempt, 2)]
            print(f"Network error: {e}. Retry in {delay}s...", file=sys.stderr)
            time.sleep(delay)

    return {"ok": False, "error": "Max retries exceeded", "status_code": 0}

# ── PlaneClient ──────────────────────────────────────────────────────────────

class PlaneClient:
    """Python API for Plane CE. Import this in agents."""

    def __init__(self, base_url: str = BASE_URL, token: str = TOKEN,
                 workspace: str = WORKSPACE, rate_share: int = RATE_SHARE):
        global BASE_URL, TOKEN, WORKSPACE, RATE_SHARE, MIN_INTERVAL
        BASE_URL = base_url
        TOKEN = token
        WORKSPACE = workspace
        RATE_SHARE = rate_share
        MIN_INTERVAL = 60.0 / (PLANE_RPM / RATE_SHARE)

    def get(self, path: str) -> Any:
        r = _make_request("GET", path)
        if r["ok"]:
            return r["data"]
        raise RuntimeError(r.get("error", f"HTTP {r['status_code']}"))

    def post(self, path: str, body: dict) -> Any:
        r = _make_request("POST", path, body)
        if r["ok"]:
            return r["data"]
        raise RuntimeError(f"POST {path} failed: {r.get('error')} {r.get('data', '')}")

    def patch(self, path: str, body: dict) -> Any:
        r = _make_request("PATCH", path, body)
        if r["ok"]:
            return r["data"]
        raise RuntimeError(f"PATCH {path} failed: {r.get('error')} {r.get('data', '')}")

    def delete(self, path: str) -> bool:
        r = _make_request("DELETE", path)
        return r["ok"]

    def list_issues(self, project_id: str, state: str = "all") -> list:
        path = f"/workspaces/{WORKSPACE}/projects/{project_id}/issues/"
        data = self.get(path)
        results = data.get("results", data) if isinstance(data, dict) else data
        if state == "all":
            return results
        # Filter client-side (state_group param unreliable in CE)
        return [i for i in results if i.get("state_detail", {}).get("group") == state]

    def get_states(self, project_id: str) -> list:
        return self.get(f"/workspaces/{WORKSPACE}/projects/{project_id}/states/")

    def get_state_id(self, project_id: str, name: str = "Backlog") -> Optional[str]:
        states = self.get_states(project_id)
        for s in states:
            if s.get("name", "").lower() == name.lower():
                return s["id"]
        return None

    def create_issue(self, project_id: str, name: str,
                     description: str = "", priority: str = "none",
                     state_id: Optional[str] = None,
                     label_ids: Optional[list] = None) -> dict:
        body: dict = {"name": name, "priority": priority}
        if description:
            body["description_html"] = f"<p>{description}</p>"
        if state_id:
            body["state"] = state_id
        if label_ids:
            body["label_ids"] = label_ids
        return self.post(
            f"/workspaces/{WORKSPACE}/projects/{project_id}/issues/", body
        )

    def update_issue(self, project_id: str, issue_id: str, **kwargs) -> dict:
        return self.patch(
            f"/workspaces/{WORKSPACE}/projects/{project_id}/issues/{issue_id}/",
            kwargs
        )

    def batch_create(self, project_id: str, issues: list,
                     verbose: bool = True) -> list:
        """Create multiple issues with throttling. Returns list of created issue dicts."""
        created = []
        for i, issue in enumerate(issues, 1):
            name = issue.get("name", issue) if isinstance(issue, dict) else str(issue)
            desc = issue.get("description", "") if isinstance(issue, dict) else ""
            prio = issue.get("priority", "none") if isinstance(issue, dict) else "none"
            sid  = issue.get("state_id") if isinstance(issue, dict) else None
            if verbose:
                print(f"  {i}/{len(issues)}: {name[:60]}", file=sys.stderr)
            result = self.create_issue(project_id, name, desc, prio, sid)
            created.append(result)
        return created

    def mark_done(self, project_id: str, issue_id: str) -> dict:
        done_id = self.get_state_id(project_id, "Done")
        if not done_id:
            raise RuntimeError("No 'Done' state found in project")
        return self.update_issue(project_id, issue_id, state=done_id)


# ── CLI ──────────────────────────────────────────────────────────────────────

def _stats():
    """Print usage stats from log file."""
    try:
        lines = Path(LOG_FILE).read_text().splitlines()
    except FileNotFoundError:
        print("No log data yet.")
        return

    total = len(lines)
    errors = sum(1 for l in lines if " 4" in l or " 5" in l or " 0 " in l)
    waits = []
    for l in lines:
        try:
            w = int(l.split("wait=")[1].replace("ms", ""))
            waits.append(w)
        except Exception:
            pass

    avg_wait = int(sum(waits) / len(waits)) if waits else 0
    print(f"Requests logged : {total}")
    print(f"Errors          : {errors}")
    print(f"Avg wait        : {avg_wait}ms")
    print(f"Max wait        : {max(waits, default=0)}ms")
    print(f"Rate share      : 1/{RATE_SHARE} of {PLANE_RPM} RPM = {60/(PLANE_RPM/RATE_SHARE):.1f}s min interval")


def _cli():
    import argparse

    if len(sys.argv) < 2:
        print(__doc__)
        sys.exit(1)

    cmd = sys.argv[1]

    if cmd == "stats":
        _stats()
        return

    plane = PlaneClient()

    if cmd.upper() in ("GET", "POST", "PATCH", "DELETE"):
        path = sys.argv[2] if len(sys.argv) > 2 else "/"
        body = json.loads(sys.argv[3]) if len(sys.argv) > 3 else None
        result = _make_request(cmd.upper(), path, body)
        print(json.dumps(result, indent=2))

    elif cmd == "list-issues":
        project_id = sys.argv[2]
        issues = plane.list_issues(project_id)
        for i in issues:
            print(f"{i['id']}  [{i.get('priority','none'):6}]  {i['name']}")

    elif cmd == "create-issue":
        parser = argparse.ArgumentParser()
        parser.add_argument("project_id")
        parser.add_argument("title")
        parser.add_argument("--priority", default="none")
        parser.add_argument("--description", default="")
        args = parser.parse_args(sys.argv[2:])
        result = plane.create_issue(
            args.project_id, args.title, args.description, args.priority
        )
        print(json.dumps(result, indent=2))

    elif cmd == "mark-done":
        project_id = sys.argv[2]
        issue_id = sys.argv[3]
        result = plane.mark_done(project_id, issue_id)
        print(json.dumps(result, indent=2))

    elif cmd == "batch-create":
        project_id = sys.argv[2]
        issues_file = sys.argv[3]
        issues = json.loads(Path(issues_file).read_text())
        results = plane.batch_create(project_id, issues)
        print(f"Created {len(results)} issues.")

    else:
        print(f"Unknown command: {cmd}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    _cli()
