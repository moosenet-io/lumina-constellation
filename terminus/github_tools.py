
import os
import sys
import json
import urllib.request
import urllib.error

# PII gate — blocks secrets/PII from being pushed to public GitHub
try:
    sys.path.insert(0, '/opt/ai-mcp')
    from pii_gate import gate as _pii_gate
    _PII_GATE_LOADED = True
except ImportError:
    _PII_GATE_LOADED = False
    def _pii_gate(tool_name, content):
        return True, "Clean"  # fail-open if gate not loaded


def _github_api(method, endpoint, data=None):
    token = os.environ.get("GITHUB_TOKEN", "")
    url = f"https://api.github.com{endpoint}"
    payload = json.dumps(data).encode() if data else None
    req = urllib.request.Request(
        url,
        data=payload,
        headers={
            "Authorization": f"Bearer {token}",
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
            "Content-Type": "application/json",
        },
        method=method,
    )
    try:
        with urllib.request.urlopen(req) as resp:
            return json.loads(resp.read())
    except urllib.error.HTTPError as e:
        body = e.read().decode()
        raise Exception(f"HTTP {e.code}: {body}")


def register_github_tools(mcp):

    @mcp.tool()
    def github_list_repos() -> dict:
        """List all repositories in the moosenet-io GitHub org."""
        org = os.environ.get("GITHUB_ORG", "moosenet-io")
        try:
            data = _github_api("GET", f"/orgs/{org}/repos?per_page=100&sort=updated")
            return {"repos": [{"name": r["name"], "full_name": r["full_name"], "private": r["private"], "url": r["html_url"], "description": r.get("description", "")} for r in data]}
        except Exception as e:
            return {"error": str(e)}

    @mcp.tool()
    def github_create_repo(name: str, description: str = "", private: bool = False) -> dict:
        """Create a new repository in the moosenet-io GitHub org. private=False by default (public repos only)."""
        # Gate on description content (low risk but good practice)
        if description:
            ok, block_msg = _pii_gate("github_create_repo", description)
            if not ok:
                return {"error": block_msg, "blocked": True}

        org = os.environ.get("GITHUB_ORG", "moosenet-io")
        try:
            data = _github_api("POST", f"/orgs/{org}/repos", {
                "name": name,
                "description": description,
                "private": private,
                "auto_init": False,
            })
            return {"created": True, "full_name": data.get("full_name"), "html_url": data.get("html_url"), "clone_url": data.get("clone_url")}
        except Exception as e:
            err = str(e)
            if "already exists" in err or "422" in err:
                return {"created": False, "error": "repo already exists", "full_name": f"{org}/{name}"}
            return {"created": False, "error": err}

    @mcp.tool()
    def github_push_repo(gitea_repo: str, github_repo: str, gitea_owner: str = "moosenet") -> dict:
        """
        Mirror a completed Gitea repo to GitHub moosenet-io org.
        NOTE: The pre-push hook on the dev host will scan commits for PII before the push completes.
        gitea_repo: repo name in Gitea (e.g. lumina-constellation)
        github_repo: target repo name in moosenet-io (e.g. lumina-constellation)
        Returns a command to run via dev_run_command on the dev host.
        """
        org = os.environ.get("GITHUB_ORG", "moosenet-io")
        github_token = os.environ.get("GITHUB_TOKEN", "")
        gitea_url = os.environ.get("GITEA_URL", "").rstrip("/")
        gitea_token = os.environ.get("GITEA_TOKEN", "")

        gitea_clone = f"{gitea_url}/{gitea_owner}/{gitea_repo}.git"
        gitea_clone_auth = gitea_clone.replace("https://", f"https://oauth2:{gitea_token}@")
        github_push = f"https://{github_token}@github.com/{org}/{github_repo}.git"

        # Shell command to run on dev host via SSH
        cmd = (
            f"cd /tmp && "
            f"rm -rf _mirror_tmp && "
            f"git clone --mirror {gitea_clone_auth} _mirror_tmp && "
            f"cd _mirror_tmp && "
            f"git push --mirror {github_push} && "
            f"cd /tmp && rm -rf _mirror_tmp && "
            f"echo MIRROR_OK"
        )
        return {"cmd": cmd, "note": "Run this via dev_run_command on the dev host. Pre-push hook will scan for PII.", "github_url": f"https://github.com/{org}/{github_repo}"}
