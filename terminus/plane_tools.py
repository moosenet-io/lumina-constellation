import json
import os
import sys
import time as _time
import urllib.parse
import urllib.request
import urllib.error

PLANE_URL = os.environ.get("PLANE_BASE_URL", "http://YOUR_PLANE_IP").rstrip("/")
PLANE_API_KEY = os.environ.get("PLANE_TOKEN_LUMINA", "")
PLANE_WORKSPACE = os.environ.get("PLANE_WORKSPACE_SLUG", "moosenet")

# ── Rate limiter: 1 second minimum between Plane API calls ──
_plane_last_call = [0.0]
_PLANE_MIN_DELAY = 1.0  # seconds between calls


def _throttled_sleep():
    """Enforce minimum delay between Plane API calls."""
    now = _time.time()
    elapsed = now - _plane_last_call[0]
    if elapsed < _PLANE_MIN_DELAY:
        _time.sleep(_PLANE_MIN_DELAY - elapsed)
    _plane_last_call[0] = _time.time()


def _headers():
    return {
        "X-API-Key": PLANE_API_KEY,
        "Content-Type": "application/json",
        "Accept": "application/json",
    }


def _api(method, path, data=None, params=None):
    """Throttled Plane API call with 429 retry (up to 3 attempts)."""
    url = f"{PLANE_URL}/api/v1{path}"
    if params:
        url += "?" + urllib.parse.urlencode(params)
    body = json.dumps(data).encode() if data else None
    req = urllib.request.Request(url, data=body, headers=_headers(), method=method)
    for attempt in range(3):
        _throttled_sleep()
        try:
            with urllib.request.urlopen(req, timeout=30) as r:
                raw = r.read().decode("utf-8", errors="replace")
                return json.loads(raw) if raw.strip() else {}
        except urllib.error.HTTPError as e:
            if e.code == 429 and attempt < 2:
                wait = 30 * (attempt + 1)
                print(f"[plane_tools] 429 rate limit, waiting {wait}s (attempt {attempt + 1}/3)", file=sys.stderr)
                _time.sleep(wait)
            else:
                raw = e.read().decode("utf-8", errors="replace")
                raise RuntimeError(f"Plane API {method} {path} -> HTTP {e.code}: {raw}") from e
    return None


def _ws(path=""):
    return f"/workspaces/{PLANE_WORKSPACE}{path}"


def _proj(project_id, path=""):
    return _ws(f"/projects/{project_id}{path}")


def register_plane_tools(mcp):

    # ── Projects ──

    @mcp.tool()
    def plane_list_projects() -> dict:
        """List all projects in the MooseNet Plane workspace.
        Returns project names, identifiers, IDs, and member counts."""
        resp = _api("GET", _ws("/projects/"))
        results = resp.get("results", [])
        return {
            "count": len(results),
            "projects": [
                {
                    "id": p["id"],
                    "name": p["name"],
                    "identifier": p["identifier"],
                    "description": p.get("description", ""),
                    "total_members": p.get("total_members", 0),
                    "created_at": p.get("created_at", ""),
                }
                for p in results
            ],
        }

    @mcp.tool()
    def plane_get_project(project_id: str) -> dict:
        """Get detailed info about a specific Plane project.
        Args:
            project_id: UUID of the project
        """
        return _api("GET", _ws(f"/projects/{project_id}/"))

    @mcp.tool()
    def plane_create_project(
        name: str,
        identifier: str,
        description: str = "",
        network: int = 0,
    ) -> dict:
        """Create a new project in the MooseNet workspace.
        Args:
            name: Project name (e.g. "Infrastructure")
            identifier: Short code, 2-5 uppercase chars (e.g. "INFRA")
            description: Project description (optional)
            network: 0 = secret, 2 = public within workspace (default 0)
        """
        data = {"name": name, "identifier": identifier, "network": network}
        if description:
            data["description"] = description
        return _api("POST", _ws("/projects/"), data=data)

    @mcp.tool()
    def plane_update_project(
        project_id: str,
        name: str = "",
        description: str = "",
        network: int = -1,
    ) -> dict:
        """Update project settings. Only provided fields are changed.
        Args:
            project_id: UUID of the project
            name: New project name (optional)
            description: New description (optional)
            network: 0 = secret, 2 = public within workspace (optional, -1 = no change)
        """
        data = {}
        if name:
            data["name"] = name
        if description:
            data["description"] = description
        if network >= 0:
            data["network"] = network
        if not data:
            return {"error": "No fields provided to update"}
        return _api("PATCH", _ws(f"/projects/{project_id}/"), data=data)

    @mcp.tool()
    def plane_delete_project(project_id: str) -> dict:
        """Delete (archive) a project permanently. Use with caution.
        Args:
            project_id: UUID of the project
        """
        _api("DELETE", _ws(f"/projects/{project_id}/"))
        return {"deleted": True, "project_id": project_id}

    # ── Work Items (Issues) ──

    @mcp.tool()
    def plane_list_work_items(
        project_id: str,
        state: str = "",
        priority: str = "",
        assignee: str = "",
        label: str = "",
        limit: int = 50,
    ) -> dict:
        """List work items (issues) in a Plane project with optional filters.
        Args:
            project_id: UUID of the project
            state: Filter by state UUID (optional)
            priority: Filter by priority: urgent, high, medium, low, none (optional)
            assignee: Filter by assignee UUID (optional)
            label: Filter by label UUID (optional)
            limit: Max results to return (default 50)
        """
        params = {"per_page": limit}
        if state:
            params["state"] = state
        if priority:
            params["priority"] = priority
        if assignee:
            params["assignees"] = assignee
        if label:
            params["labels"] = label
        resp = _api("GET", _proj(project_id, "/work-items/"), params=params)
        results = resp.get("results", [])
        return {
            "count": len(results),
            "total": resp.get("total_results", len(results)),
            "work_items": [
                {
                    "id": w["id"],
                    "name": w.get("name", ""),
                    "state": w.get("state", ""),
                    "priority": w.get("priority", ""),
                    "assignees": w.get("assignees", []),
                    "labels": w.get("labels", []),
                    "created_at": w.get("created_at", ""),
                    "updated_at": w.get("updated_at", ""),
                    "sequence_id": w.get("sequence_id", ""),
                }
                for w in results
            ],
        }

    @mcp.tool()
    def plane_get_work_item(project_id: str, work_item_id: str) -> dict:
        """Get full details of a specific work item including description.
        Args:
            project_id: UUID of the project
            work_item_id: UUID of the work item
        """
        return _api("GET", _proj(project_id, f"/work-items/{work_item_id}/"))

    @mcp.tool()
    def plane_create_work_item(
        project_id: str,
        name: str,
        description_html: str = "",
        priority: str = "medium",
        state: str = "",
        assignees: str = "",
        labels: str = "",
    ) -> dict:
        """Create a new work item (issue) in a Plane project.
        Args:
            project_id: UUID of the project
            name: Title of the work item
            description_html: HTML description (optional)
            priority: urgent, high, medium, low, none (default medium)
            state: UUID of the state (optional, uses default)
            assignees: Comma-separated UUIDs of assignees (optional)
            labels: Comma-separated UUIDs of labels (optional)
        """
        data = {"name": name, "priority": priority}
        if description_html:
            data["description_html"] = description_html
        if state:
            data["state"] = state
        if assignees:
            data["assignees"] = [a.strip() for a in assignees.split(",") if a.strip()]
        if labels:
            data["labels"] = [l.strip() for l in labels.split(",") if l.strip()]
        return _api("POST", _proj(project_id, "/work-items/"), data=data)

    @mcp.tool()
    def plane_update_work_item(
        project_id: str,
        work_item_id: str,
        name: str = "",
        description_html: str = "",
        priority: str = "",
        state: str = "",
        assignees: str = "",
        labels: str = "",
    ) -> dict:
        """Update an existing work item. Only provided fields are changed.
        Args:
            project_id: UUID of the project
            work_item_id: UUID of the work item
            name: New title (optional)
            description_html: New HTML description (optional)
            priority: urgent, high, medium, low, none (optional)
            state: UUID of new state (optional)
            assignees: Comma-separated UUIDs of assignees (optional)
            labels: Comma-separated UUIDs of labels (optional)
        """
        data = {}
        if name:
            data["name"] = name
        if description_html:
            data["description_html"] = description_html
        if priority:
            data["priority"] = priority
        if state:
            data["state"] = state
        if assignees:
            data["assignees"] = [a.strip() for a in assignees.split(",") if a.strip()]
        if labels:
            data["labels"] = [l.strip() for l in labels.split(",") if l.strip()]
        if not data:
            return {"error": "No fields provided to update"}
        return _api("PATCH", _proj(project_id, f"/work-items/{work_item_id}/"), data=data)

    @mcp.tool()
    def plane_delete_work_item(project_id: str, work_item_id: str) -> dict:
        """Delete a work item permanently.
        Args:
            project_id: UUID of the project
            work_item_id: UUID of the work item
        """
        _api("DELETE", _proj(project_id, f"/work-items/{work_item_id}/"))
        return {"deleted": True, "work_item_id": work_item_id}

    # ── States ──

    @mcp.tool()
    def plane_list_states(project_id: str) -> dict:
        """List all workflow states for a project (e.g. Backlog, In Progress, Done).
        Args:
            project_id: UUID of the project
        """
        resp = _api("GET", _proj(project_id, "/states/"))
        results = resp.get("results", resp if isinstance(resp, list) else [])
        return {
            "count": len(results),
            "states": [
                {
                    "id": s["id"],
                    "name": s["name"],
                    "group": s.get("group", ""),
                    "color": s.get("color", ""),
                    "sequence": s.get("sequence", 0),
                }
                for s in results
            ],
        }

    # ── Labels ──

    @mcp.tool()
    def plane_list_labels(project_id: str) -> dict:
        """List all labels for a project.
        Args:
            project_id: UUID of the project
        """
        resp = _api("GET", _proj(project_id, "/labels/"))
        results = resp.get("results", resp if isinstance(resp, list) else [])
        return {
            "count": len(results),
            "labels": [
                {"id": l["id"], "name": l["name"], "color": l.get("color", "")}
                for l in results
            ],
        }

    @mcp.tool()
    def plane_create_label(project_id: str, name: str, color: str = "#95999f") -> dict:
        """Create a new label in a project.
        Args:
            project_id: UUID of the project
            name: Label name
            color: Hex color code (default gray)
        """
        return _api("POST", _proj(project_id, "/labels/"), data={"name": name, "color": color})

    # ── Members ──

    @mcp.tool()
    def plane_list_members() -> dict:
        """List all members in the MooseNet workspace with their roles."""
        resp = _api("GET", _ws("/members/"))
        results = resp.get("results", resp if isinstance(resp, list) else [])
        return {
            "count": len(results),
            "members": [
                {
                    "id": m.get("member", {}).get("id", m.get("id", "")),
                    "display_name": m.get("member", {}).get("display_name", m.get("display_name", "")),
                    "email": m.get("member", {}).get("email", m.get("email", "")),
                    "role": m.get("role", ""),
                }
                for m in results
            ],
        }

    # ── Cycles (Sprints) ──

    @mcp.tool()
    def plane_list_cycles(project_id: str) -> dict:
        """List all cycles (sprints) in a project.
        Args:
            project_id: UUID of the project
        """
        resp = _api("GET", _proj(project_id, "/cycles/"))
        results = resp.get("results", resp if isinstance(resp, list) else [])
        return {
            "count": len(results),
            "cycles": [
                {
                    "id": c["id"],
                    "name": c["name"],
                    "start_date": c.get("start_date", ""),
                    "end_date": c.get("end_date", ""),
                    "status": c.get("status", ""),
                }
                for c in results
            ],
        }

    @mcp.tool()
    def plane_create_cycle(
        project_id: str,
        name: str,
        start_date: str = "",
        end_date: str = "",
        description: str = "",
    ) -> dict:
        """Create a new cycle (sprint) in a project.
        Args:
            project_id: UUID of the project
            name: Cycle name (e.g. "Sprint 1", "Week of Apr 7")
            start_date: Start date YYYY-MM-DD (optional)
            end_date: End date YYYY-MM-DD (optional)
            description: Cycle description (optional)
        """
        data = {"name": name}
        if start_date:
            data["start_date"] = start_date
        if end_date:
            data["end_date"] = end_date
        if description:
            data["description"] = description
        return _api("POST", _proj(project_id, "/cycles/"), data=data)

    @mcp.tool()
    def plane_update_cycle(
        project_id: str,
        cycle_id: str,
        name: str = "",
        start_date: str = "",
        end_date: str = "",
        description: str = "",
    ) -> dict:
        """Update an existing cycle. Only provided fields are changed.
        Args:
            project_id: UUID of the project
            cycle_id: UUID of the cycle
            name: New name (optional)
            start_date: New start date YYYY-MM-DD (optional)
            end_date: New end date YYYY-MM-DD (optional)
            description: New description (optional)
        """
        data = {}
        if name:
            data["name"] = name
        if start_date:
            data["start_date"] = start_date
        if end_date:
            data["end_date"] = end_date
        if description:
            data["description"] = description
        if not data:
            return {"error": "No fields provided to update"}
        return _api("PATCH", _proj(project_id, f"/cycles/{cycle_id}/"), data=data)

    # ── Modules ──

    @mcp.tool()
    def plane_list_modules(project_id: str) -> dict:
        """List all modules in a project.
        Args:
            project_id: UUID of the project
        """
        resp = _api("GET", _proj(project_id, "/modules/"))
        results = resp.get("results", resp if isinstance(resp, list) else [])
        return {
            "count": len(results),
            "modules": [
                {
                    "id": m["id"],
                    "name": m["name"],
                    "description": m.get("description", ""),
                    "status": m.get("status", ""),
                    "start_date": m.get("start_date", ""),
                    "target_date": m.get("target_date", ""),
                }
                for m in results
            ],
        }

    @mcp.tool()
    def plane_create_module(
        project_id: str,
        name: str,
        description: str = "",
        start_date: str = "",
        target_date: str = "",
    ) -> dict:
        """Create a new module in a project.
        Args:
            project_id: UUID of the project
            name: Module name
            description: Module description (optional)
            start_date: Start date YYYY-MM-DD (optional)
            target_date: Target date YYYY-MM-DD (optional)
        """
        data = {"name": name}
        if description:
            data["description"] = description
        if start_date:
            data["start_date"] = start_date
        if target_date:
            data["target_date"] = target_date
        return _api("POST", _proj(project_id, "/modules/"), data=data)

    @mcp.tool()
    def plane_update_module(
        project_id: str,
        module_id: str,
        name: str = "",
        description: str = "",
        start_date: str = "",
        target_date: str = "",
        status: str = "",
    ) -> dict:
        """Update an existing module. Only provided fields are changed.
        Args:
            project_id: UUID of the project
            module_id: UUID of the module
            name: New name (optional)
            description: New description (optional)
            start_date: New start date YYYY-MM-DD (optional)
            target_date: New target date YYYY-MM-DD (optional)
            status: New status (optional)
        """
        data = {}
        if name:
            data["name"] = name
        if description:
            data["description"] = description
        if start_date:
            data["start_date"] = start_date
        if target_date:
            data["target_date"] = target_date
        if status:
            data["status"] = status
        if not data:
            return {"error": "No fields provided to update"}
        return _api("PATCH", _proj(project_id, f"/modules/{module_id}/"), data=data)

    # ── Work Item Assignment to Cycles/Modules ──

    @mcp.tool()
    def plane_add_work_items_to_cycle(
        project_id: str,
        cycle_id: str,
        work_item_ids: str,
    ) -> dict:
        """Add work items to a cycle (sprint).
        Args:
            project_id: UUID of the project
            cycle_id: UUID of the cycle
            work_item_ids: Comma-separated UUIDs of work items to add
        """
        ids = [i.strip() for i in work_item_ids.split(",") if i.strip()]
        return _api(
            "POST",
            _proj(project_id, f"/cycles/{cycle_id}/work-items/"),
            data={"work_items": ids},
        )

    @mcp.tool()
    def plane_add_work_items_to_module(
        project_id: str,
        module_id: str,
        work_item_ids: str,
    ) -> dict:
        """Add work items to a module.
        Args:
            project_id: UUID of the project
            module_id: UUID of the module
            work_item_ids: Comma-separated UUIDs of work items to add
        """
        ids = [i.strip() for i in work_item_ids.split(",") if i.strip()]
        return _api(
            "POST",
            _proj(project_id, f"/modules/{module_id}/work-items/"),
            data={"work_items": ids},
        )

    # ── Comments ──

    @mcp.tool()
    def plane_list_comments(project_id: str, work_item_id: str) -> dict:
        """List all comments on a work item.
        Args:
            project_id: UUID of the project
            work_item_id: UUID of the work item
        """
        resp = _api("GET", _proj(project_id, f"/work-items/{work_item_id}/comments/"))
        results = resp.get("results", resp if isinstance(resp, list) else [])
        return {
            "count": len(results),
            "comments": [
                {
                    "id": c["id"],
                    "comment_html": c.get("comment_html", ""),
                    "actor": c.get("actor_detail", {}).get("display_name", ""),
                    "created_at": c.get("created_at", ""),
                }
                for c in results
            ],
        }

    @mcp.tool()
    def plane_add_comment(project_id: str, work_item_id: str, comment_html: str) -> dict:
        """Add a comment to a work item.
        Args:
            project_id: UUID of the project
            work_item_id: UUID of the work item
            comment_html: HTML content of the comment
        """
        return _api(
            "POST",
            _proj(project_id, f"/work-items/{work_item_id}/comments/"),
            data={"comment_html": comment_html},
        )
