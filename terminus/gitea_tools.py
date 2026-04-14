import base64
import json
import os
import sys
import urllib.parse
import urllib.request
import urllib.error

# PII gate — blocks secrets/PII from being committed to external repos
try:
    sys.path.insert(0, '/opt/ai-mcp')
    from pii_gate import gate as _pii_gate
    _PII_GATE_LOADED = True
except ImportError:
    _PII_GATE_LOADED = False
    def _pii_gate(tool_name, content):
        return True, "Clean"  # fail-open if gate not loaded

GITEA_URL = os.environ.get("GITEA_URL", "https://git.moosenet.online").rstrip("/")
GITEA_TOKEN = os.environ.get("GITEA_TOKEN", "")
DEFAULT_OWNER = os.environ.get("GITEA_DEFAULT_REPO_OWNER", "")

def _headers():
    return {
        "Authorization": f"token {GITEA_TOKEN}",
        "Content-Type": "application/json",
        "Accept": "application/json",
    }

def _api(method, path, data=None, params=None):
    url = f"{GITEA_URL}/api/v1{path}"
    if params:
        url += "?" + urllib.parse.urlencode(params)
    body = json.dumps(data).encode() if data else None
    req = urllib.request.Request(url, data=body, headers=_headers(), method=method)
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            raw = r.read().decode("utf-8", errors="replace")
            return json.loads(raw) if raw.strip() else {}
    except urllib.error.HTTPError as e:
        raw = e.read().decode("utf-8", errors="replace")
        raise RuntimeError(f"Gitea API {method} {path} -> HTTP {e.code}: {raw}") from e

def _owner(owner):
    return owner or DEFAULT_OWNER

def register_gitea_tools(mcp):

    @mcp.tool()
    def gitea_list_directory(repo: str, path: str = "", branch: str = "main", owner: str = "") -> dict:
        """List contents of a directory in a Gitea repo."""
        o = _owner(owner)
        data = _api("GET", f"/repos/{o}/{repo}/contents/{path}", params={"ref": branch})
        if isinstance(data, list):
            return {"entries": [{"name": e["name"], "type": e["type"], "path": e["path"], "sha": e["sha"]} for e in data]}
        return {"entries": [{"name": data["name"], "type": "file", "path": data["path"], "sha": data["sha"]}]}

    @mcp.tool()
    def gitea_get_file(repo: str, path: str, branch: str = "main", owner: str = "") -> dict:
        """Read the contents of a file from a Gitea repo."""
        o = _owner(owner)
        data = _api("GET", f"/repos/{o}/{repo}/contents/{path}", params={"ref": branch})
        content = base64.b64decode(data["content"]).decode("utf-8")
        return {"path": data["path"], "sha": data["sha"], "content": content}

    @mcp.tool()
    def gitea_create_or_update_file(repo: str, path: str, content: str, message: str, branch: str = "main", owner: str = "") -> dict:
        """Create or update a file in a Gitea repo with a single commit."""
        # PII gate — block secrets/PII before they reach the remote repo
        ok, block_msg = _pii_gate("gitea_create_or_update_file", content)
        if not ok:
            return {"error": block_msg, "blocked": True}

        o = _owner(owner)
        encoded = base64.b64encode(content.encode("utf-8")).decode("ascii")
        sha = None
        try:
            existing = _api("GET", f"/repos/{o}/{repo}/contents/{path}", params={"ref": branch})
            sha = existing.get("sha")
        except RuntimeError as e:
            if "HTTP 404" not in str(e):
                raise
        payload = {"message": message, "content": encoded, "branch": branch}
        if sha:
            payload["sha"] = sha
        method = "PUT" if sha else "POST"
        data = _api(method, f"/repos/{o}/{repo}/contents/{path}", data=payload)
        return {"path": path, "branch": branch, "commit_sha": data.get("commit", {}).get("sha", ""), "action": "updated" if sha else "created"}

    @mcp.tool()
    def gitea_create_branch(repo: str, new_branch: str, from_branch: str = "main", owner: str = "") -> dict:
        """Create a new branch in a Gitea repo."""
        o = _owner(owner)
        data = _api("POST", f"/repos/{o}/{repo}/branches", data={"new_branch_name": new_branch, "old_branch_name": from_branch})
        return {"branch": data["name"], "from_ref": from_branch, "commit_sha": data.get("commit", {}).get("id", "")}

    @mcp.tool()
    def gitea_create_pull_request(repo: str, title: str, head: str, base: str = "main", body: str = "", owner: str = "") -> dict:
        """Open a pull request in a Gitea repo."""
        # Gate on PR body content
        if body:
            ok, block_msg = _pii_gate("gitea_create_pull_request", body)
            if not ok:
                return {"error": block_msg, "blocked": True}

        o = _owner(owner)
        data = _api("POST", f"/repos/{o}/{repo}/pulls", data={"title": title, "head": head, "base": base, "body": body})
        return {"number": data["number"], "url": data["html_url"], "title": data["title"], "state": data["state"]}

    @mcp.tool()
    def gitea_create_repo(name: str, owner: str = "", description: str = "", private: bool = True, auto_init: bool = True) -> dict:
        """Create a new Gitea repository under an org or user. owner defaults to GITEA_DEFAULT_REPO_OWNER. private=True, auto_init=True by default."""
        o = _owner(owner)
        payload = {"name": name, "description": description, "private": private, "auto_init": auto_init, "default_branch": "main"}
        for endpoint in [f"/orgs/{o}/repos", "/user/repos"]:
            try:
                data = _api("POST", endpoint, data=payload)
                return {"created": True, "full_name": data.get("full_name"), "html_url": data.get("html_url"), "private": data.get("private"), "id": data.get("id")}
            except Exception as e:
                err = str(e)
                if "already exists" in err:
                    return {"created": False, "error": "repo already exists", "full_name": f"{o}/{name}"}
                if "404" in err or "403" in err:
                    continue
                return {"created": False, "error": err}
        return {"created": False, "error": "all endpoints failed"}

    @mcp.tool()
    def gitea_list_repos(owner: str = "") -> dict:
        """List all repositories in an org or user account."""
        o = _owner(owner)
        try:
            data = _api("GET", f"/orgs/{o}/repos", params={"limit": 50})
        except RuntimeError:
            data = _api("GET", "/user/repos", params={"limit": 50})
        return {"repos": [{"name": r["name"], "full_name": r["full_name"], "private": r["private"], "updated_at": r["updated_at"]} for r in data]}

    @mcp.tool()
    def gitea_delete_file(repo: str, path: str, message: str, branch: str = "main", owner: str = "") -> dict:
        """Delete a file from a Gitea repo."""
        o = _owner(owner)
        existing = _api("GET", f"/repos/{o}/{repo}/contents/{path}", params={"ref": branch})
        sha = existing["sha"]
        data = _api("DELETE", f"/repos/{o}/{repo}/contents/{path}", data={"message": message, "sha": sha, "branch": branch})
        return {"path": path, "deleted": True, "commit_sha": data.get("commit", {}).get("sha", "")}

    @mcp.tool()
    def gitea_list_branches(repo: str, owner: str = "") -> dict:
        """List all branches in a Gitea repo."""
        o = _owner(owner)
        data = _api("GET", f"/repos/{o}/{repo}/branches", params={"limit": 50})
        return {"branches": [{"name": b["name"], "commit_sha": b.get("commit", {}).get("id", "")} for b in data]}

    @mcp.tool()
    def gitea_list_pull_requests(repo: str, state: str = "open", owner: str = "") -> dict:
        """List pull requests in a Gitea repo."""
        o = _owner(owner)
        data = _api("GET", f"/repos/{o}/{repo}/pulls", params={"state": state, "limit": 50})
        return {"pull_requests": [{"number": pr["number"], "title": pr["title"], "state": pr["state"], "head": pr["head"]["ref"], "base": pr["base"]["ref"], "user": pr.get("user", {}).get("login", "")} for pr in data]}
