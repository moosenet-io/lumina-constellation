import subprocess
import json

# ============================================================
# Odyssey Tools — Trip Planning MCP Tools
# Runs on terminus-host, SSHes to fleet-host to execute odyssey.py.
# Handles destination research (via Seer), bucket list,
# loyalty points, card optimization, trip logging, deals.
# ============================================================

ODYSSEY_HOST = "root@YOUR_FLEET_SERVER_IP"
ODYSSEY_SCRIPT = "/usr/bin/python3 /opt/lumina-fleet/odyssey/odyssey.py"
ODYSSEY_ENV = "source /opt/lumina-fleet/axon/.env && "


def _ssh_exec(cmd, timeout=300):
    """Execute a command on fleet-host via SSH."""
    full_cmd = f"ssh -o ConnectTimeout=5 -o StrictHostKeyChecking=no {ODYSSEY_HOST} '{cmd}'"
    try:
        result = subprocess.run(
            full_cmd, shell=True, capture_output=True, text=True, timeout=timeout
        )
        return {"stdout": result.stdout.strip(), "stderr": result.stderr.strip(), "rc": result.returncode}
    except subprocess.TimeoutExpired:
        return {"error": "Odyssey command timed out", "rc": -1}
    except Exception as e:
        return {"error": str(e), "rc": -1}


def _run_odyssey(args_str: str, timeout: int = 120) -> dict:
    """Run odyssey.py with given args on fleet-host and parse JSON output."""
    cmd = f"{ODYSSEY_ENV}{ODYSSEY_SCRIPT} {args_str}"
    result = _ssh_exec(cmd, timeout=timeout)
    if result.get("error"):
        return {"status": "error", "error": result["error"]}
    if result["rc"] != 0:
        return {"status": "failed", "error": result.get("stderr", "Unknown error"), "output": result.get("stdout", "")}
    stdout = result.get("stdout", "")
    # Parse last JSON line
    for line in reversed(stdout.split('\n')):
        line = line.strip()
        if line.startswith('{') or line.startswith('['):
            try:
                return json.loads(line)
            except Exception:
                pass
    return {"status": "ok", "output": stdout}


def register_odyssey_tools(mcp):

    @mcp.tool()
    def odyssey_research(destination: str, dates: str = '', budget: str = '', travelers: int = 1) -> dict:
        """Trigger Seer deep research for a travel destination.
        Runs a standard-effort research sweep via Seer, stores results in Engram,
        and updates bucket list status to 'researched'.
        destination: e.g. 'Tokyo, Japan' or 'Patagonia, Chile'
        dates: optional travel window, e.g. 'March 2027' or 'spring'
        budget: optional budget hint, e.g. '$5000' or 'budget'
        travelers: number of people (default 1)
        Returns report_id and URL when done. Takes 2-5 minutes."""
        args = f'research --destination "{destination}"'
        if dates:
            args += f' --dates "{dates}"'
        if budget:
            args += f' --budget "{budget}"'
        if travelers > 1:
            args += f' --travelers {travelers}'
        return _run_odyssey(args, timeout=360)

    @mcp.tool()
    def odyssey_bucket_add(destination: str, priority: str = 'medium', season: str = '', budget: float = 0, notes: str = '') -> dict:
        """Add a destination to the operator's travel bucket list.
        Stores in Engram, regenerates HTML at /travel/bucket-list/.
        destination: full name, e.g. 'Kyoto, Japan'
        priority: 'urgent', 'high', 'medium', or 'low'
        season: best travel window, e.g. 'spring', 'Oct-Nov'
        budget: estimated trip budget in USD (0 = unknown)
        notes: any notes, e.g. 'cherry blossom season'"""
        args = f'bucket-add --destination "{destination}" --priority {priority}'
        if season:
            args += f' --season "{season}"'
        if budget > 0:
            args += f' --budget {budget}'
        if notes:
            args += f' --notes "{notes}"'
        return _run_odyssey(args)

    @mcp.tool()
    def odyssey_bucket_list(status_filter: str = '') -> dict:
        """Get the operator's travel bucket list from Engram.
        Returns all destinations sorted by priority.
        status_filter: optionally filter by status — 'dream', 'researched', 'planned', 'booked', 'completed'
        Leave empty to get all destinations."""
        args = 'bucket-list'
        if status_filter:
            args += f' --status {status_filter}'
        result = _run_odyssey(args)
        if isinstance(result, list):
            return {"destinations": result, "count": len(result)}
        return result

    @mcp.tool()
    def odyssey_update_points(program: str, balance: int, card_type: str = 'credit', tier: str = '', benefits: str = '') -> dict:
        """Store/update a loyalty program or credit card balance in Engram.
        program: e.g. 'Chase Sapphire Reserve', 'Delta SkyMiles', 'Marriott Bonvoy'
        balance: current point/mile balance as integer
        card_type: 'credit', 'airline', 'hotel', or 'misc' (default: credit)
        tier: optional elite tier, e.g. 'sapphire-reserve', 'gold', 'platinum'
        benefits: comma-separated benefits, e.g. '3x travel,lounge access,trip delay'"""
        args = f'update-points --program "{program}" --balance {balance} --type {card_type}'
        if tier:
            args += f' --tier "{tier}"'
        if benefits:
            args += f' --benefits "{benefits}"'
        return _run_odyssey(args)

    @mcp.tool()
    def odyssey_list_cards() -> dict:
        """List the operator's full card and loyalty program portfolio from Engram.
        Returns all cards sorted by balance (highest first) with type, tier, and benefits.
        Use this before odyssey_optimize to see what's in the portfolio."""
        result = _run_odyssey('list-cards')
        if isinstance(result, list):
            return {"cards": result, "count": len(result)}
        return result

    @mcp.tool()
    def odyssey_optimize(destination: str, spend_estimate: float = 5000) -> dict:
        """Ask Mr. Wizard (via LiteLLM) to recommend the best card/points strategy for a trip.
        Pulls the full card portfolio from Engram, then runs AI reasoning to recommend
        which card to use for flights, hotels, and dining, and whether to redeem points.
        destination: destination name, e.g. 'Tokyo, Japan'
        spend_estimate: estimated total trip spend in USD (default 5000)
        Returns AI recommendation. Takes up to 60 seconds."""
        args = f'optimize --destination "{destination}" --spend {spend_estimate}'
        return _run_odyssey(args, timeout=120)

    @mcp.tool()
    def odyssey_log_trip(destination: str, dates: str, highlights: str, rating: int = 5, cost: float = 0) -> dict:
        """Log a completed trip to the adventure log in Engram.
        Updates bucket list status to 'completed'. Generates updated HTML.
        destination: e.g. 'Tokyo, Japan'
        dates: trip dates, e.g. 'March 15-25, 2027'
        highlights: 1-2 sentence summary of the trip
        rating: 1-5 stars
        cost: actual total trip cost in USD"""
        args = f'log-trip --destination "{destination}" --dates "{dates}" --highlights "{highlights}" --rating {rating}'
        if cost > 0:
            args += f' --cost {cost}'
        return _run_odyssey(args)

    @mcp.tool()
    def odyssey_deals(destination_filter: str = '') -> dict:
        """Search Engram for stored travel deals.
        Returns deal entries tagged 'deal' in Engram knowledge base.
        destination_filter: optional partial destination name to filter results
        Leave empty to get all stored deals."""
        filter_clause = f"AND key LIKE '%{destination_filter.lower().replace(' ','-')}%'" if destination_filter else ""
        cmd = (
            f"{ODYSSEY_ENV}"
            f"python3 -c \""
            f"import sys, json, sqlite3; sys.path.insert(0,'/opt/lumina-fleet/engram'); import engram; "
            f"import os; db=os.environ.get('ENGRAM_DB_PATH','/opt/lumina-fleet/engram/engram.db'); "
            f"conn=sqlite3.connect(db); "
            f"rows=conn.execute(\\\"SELECT key,content FROM knowledge_base WHERE key LIKE 'travel/deals/%' {filter_clause} ORDER BY rowid DESC LIMIT 20\\\").fetchall(); "
            f"conn.close(); "
            f"deals=[json.loads(r[1]) if r[1].startswith('{{') else {{'key':r[0],'content':r[1]}} for r in rows]; "
            f"print(json.dumps(deals))\""
        )
        result = _ssh_exec(cmd)
        if result.get("rc") == 0:
            try:
                deals = json.loads(result["stdout"])
                return {"deals": deals, "count": len(deals), "filter": destination_filter or "all"}
            except Exception:
                return {"deals": [], "count": 0, "note": "No deals stored yet."}
        return {"status": "error", "error": result.get("stderr", "unknown")}
