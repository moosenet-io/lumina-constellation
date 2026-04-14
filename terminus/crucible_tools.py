import subprocess
import json

# ============================================================
# Crucible Tools — Learning & Skills Tracker (Module 11)
# MCP tools that SSH to CT310 and run crucible.py.
# All data stored in Engram (sqlite-vec). No external backend.
# ============================================================

CRUCIBLE_HOST = "root@YOUR_FLEET_SERVER_IP"
CRUCIBLE_SCRIPT = "/usr/bin/python3 /opt/lumina-fleet/crucible/crucible.py"
CRUCIBLE_ENV = "source /opt/lumina-fleet/axon/.env && export LITELLM_MASTER_KEY &&"


def _ssh_exec(cmd, timeout=60):
    """Execute a command on CT310 via SSH."""
    full_cmd = f"ssh -o ConnectTimeout=5 -o StrictHostKeyChecking=no {CRUCIBLE_HOST} '{cmd}'"
    try:
        result = subprocess.run(
            full_cmd, shell=True, capture_output=True, text=True, timeout=timeout
        )
        return {"stdout": result.stdout.strip(), "stderr": result.stderr.strip(), "rc": result.returncode}
    except subprocess.TimeoutExpired:
        return {"error": "Command timed out", "rc": -1}
    except Exception as e:
        return {"error": str(e), "rc": -1}


def _run_crucible(args: str, timeout: int = 30) -> dict:
    """Run crucible.py with given args, return parsed JSON output."""
    cmd = f"{CRUCIBLE_ENV} cd /opt/lumina-fleet/crucible && {CRUCIBLE_SCRIPT} {args} 2>&1"
    result = _ssh_exec(cmd, timeout=timeout)
    if result.get("error"):
        return {"error": result["error"]}
    if result["rc"] != 0:
        return {"error": result.get("stderr") or result.get("stdout") or "crucible.py failed"}
    try:
        return json.loads(result["stdout"])
    except json.JSONDecodeError:
        return {"raw": result["stdout"]}


def register_crucible_tools(mcp):

    @mcp.tool()
    def crucible_track_create(name: str, track_type: str, goal: str, target_date: str = "") -> dict:
        """Create a new learning track in Crucible.
        name: Human-readable name (e.g. 'Rust Programming', 'AWS SAA Cert').
        track_type: One of: book, course, cert, hobby, skill.
        goal: What completion looks like (e.g. 'Finish chapters 1-20').
        target_date: Optional ISO date string (YYYY-MM-DD).
        Returns the created track object with its slug for future log calls."""
        args = f"create --name {json.dumps(name)} --type {json.dumps(track_type)} --goal {json.dumps(goal)}"
        if target_date:
            args += f" --target-date {json.dumps(target_date)}"
        return _run_crucible(args)

    @mcp.tool()
    def crucible_log(track: str, progress: str, notes: str = "", duration_min: int = 0) -> dict:
        """Log a learning session for an existing track.
        track: The track slug (e.g. 'rust-programming', 'aws-saa-cert').
        progress: What was accomplished (e.g. 'Completed chapter 3: Variables').
        notes: Optional extra notes.
        duration_min: Time spent in minutes (0 if unknown).
        Updates streak, stores session in Engram. Returns streak count and milestone if hit."""
        args = f"log --track {json.dumps(track)} --progress {json.dumps(progress)}"
        if notes:
            args += f" --notes {json.dumps(notes)}"
        if duration_min:
            args += f" --duration {duration_min}"
        return _run_crucible(args)

    @mcp.tool()
    def crucible_status(track: str = "") -> dict:
        """Get status of one or all active learning tracks.
        track: Slug of a specific track (e.g. 'rust-programming'), or empty for all active tracks.
        Returns track data including sessions count, last session date, streak, and goal."""
        args = f"status"
        if track:
            args += f" --track {json.dumps(track)}"
        return _run_crucible(args)

    @mcp.tool()
    def crucible_streak() -> dict:
        """Get the overall learning streak across all tracks.
        Returns current_streak (consecutive days with any session),
        recent_active_days (last 30 days), and sessions_total."""
        return _run_crucible("streak")

    @mcp.tool()
    def crucible_tracks(type_filter: str = "", active_only: bool = True) -> dict:
        """List learning tracks, optionally filtered by type.
        type_filter: One of book, course, cert, hobby, skill — or empty for all types.
        active_only: If true (default), only returns active tracks.
        Returns list of track objects."""
        args = "tracks"
        if type_filter:
            args += f" --type {json.dumps(type_filter)}"
        if not active_only:
            args += " --all"
        result = _run_crucible(args)
        # list_tracks returns a list; wrap for consistency
        if isinstance(result, list):
            return {"tracks": result, "count": len(result)}
        return result

    @mcp.tool()
    def crucible_hobby(project: str, entry_type: str, date: str = "", location: str = "", notes: str = "") -> dict:
        """Log a hobby activity (FPV drone, photography, woodworking, etc.).
        project: Project name (e.g. 'FPV 5-inch build', 'Backyard rocket').
        entry_type: Type of activity (e.g. 'build', 'flight', 'test', 'repair', 'planning').
        date: ISO date string (YYYY-MM-DD), defaults to today.
        location: Where it happened (e.g. 'backyard', 'garage', 'local field').
        notes: What was done, results, observations.
        Stores in Engram under crucible/hobbies/."""
        from datetime import date as _date
        use_date = date or str(_date.today())
        args = f"hobby --project {json.dumps(project)} --type {json.dumps(entry_type)} --date {json.dumps(use_date)}"
        if location:
            args += f" --location {json.dumps(location)}"
        if notes:
            args += f" --notes {json.dumps(notes)}"
        return _run_crucible(args)

    @mcp.tool()
    def crucible_reading_add(title: str, priority: str = "normal", notes: str = "") -> dict:
        """Add an article, post, doc, or book to the reading queue.
        title: Title or URL of the item to read.
        priority: 'urgent', 'normal', or 'low'.
        notes: Why it's relevant or what to look for.
        Returns the slug for use with crucible_reading_done."""
        args = f"reading-add --title {json.dumps(title)}"
        if priority:
            args += f" --priority {json.dumps(priority)}"
        if notes:
            args += f" --notes {json.dumps(notes)}"
        return _run_crucible(args)

    @mcp.tool()
    def crucible_reading_list(status_filter: str = "unread") -> dict:
        """Get the reading queue.
        status_filter: 'unread' (default), 'read', or empty for all.
        Returns list of reading items sorted by priority then date added."""
        args = f"reading-list --status {json.dumps(status_filter)}"
        result = _run_crucible(args)
        if isinstance(result, list):
            return {"items": result, "count": len(result)}
        return result

    @mcp.tool()
    def crucible_reading_done(slug: str, notes: str = "") -> dict:
        """Mark a reading queue item as done.
        slug: The item slug returned when it was added (e.g. 'the-rust-programming-language').
        notes: Optional completion notes — key takeaways, thoughts.
        Journals the completion in Engram."""
        args = f"reading-done --slug {json.dumps(slug)}"
        if notes:
            args += f" --notes {json.dumps(notes)}"
        return _run_crucible(args)

    @mcp.tool()
    def crucible_dashboard() -> dict:
        """Regenerate the Crucible learning dashboard at http://YOUR_FLEET_SERVER_IP/learning/.
        Pulls current track data, streak, and reading queue from Engram.
        Returns path to the written HTML file.
        Call after logging sessions or adding tracks to refresh the page."""
        result = _run_crucible("dashboard", timeout=30)
        if isinstance(result, str):
            return {"status": "ok", "path": result, "url": "http://YOUR_FLEET_SERVER_IP/learning/"}
        if isinstance(result, dict) and "raw" in result:
            return {"status": "ok", "path": result["raw"], "url": "http://YOUR_FLEET_SERVER_IP/learning/"}
        return result
