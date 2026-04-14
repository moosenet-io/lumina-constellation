#!/usr/bin/env python3
"""
Vigil — MooseNet Briefing Sub-Agent v4
Runs on CT310. Modular section-based briefing system.
Each section is an independent gather + format pipeline.

Usage:
    python3 briefing.py morning
    python3 briefing.py afternoon
"""

import sys as _sys; _sys.path.insert(0, '/opt/lumina-fleet')
try: from naming import display_name as _dn, constellation_name as _cn
except: _dn = lambda x: x; _cn = lambda: 'Lumina'

import json
import os
import sys
import base64
import random
import urllib.request
import urllib.parse
import urllib.error
from datetime import datetime, timezone, timedelta
from concurrent.futures import ThreadPoolExecutor, as_completed

from briefing_dashboard import generate_dashboard

# ============================================================
# Config
# ============================================================

INFISICAL_AUTH = "/opt/briefing-agent/.infisical-auth"
LITELLM_URL = "http://YOUR_LITELLM_IP:4000"
LITELLM_MODEL = "Lumina Fast"
LITELLM_MODELS = [LITELLM_MODEL]
GITEA_URL = "http://YOUR_GITEA_IP:3000"
GITEA_REPO_OWNER = "moosenet"
GITEA_REPO = "lumina-fleet"
GITEA_BRANCH = "main"

# Plane CE
PLANE_URL = "http://YOUR_PLANE_IP"
PLANE_LM_PROJECT_ID = "4ef3f3ec-e7ef-4af3-b258-881565e629f9"

# Service endpoints
JELLYSEERR_URL = "http://YOUR_PVM_HOST_IP:5055"
PORTAINER_URL = "http://YOUR_PVM_HOST_IP:9000"
PROMETHEUS_URL = "http://YOUR_PROMETHEUS_IP:9090"

# Dashboard
DASHBOARD_URL = "http://YOUR_FLEET_SERVER_IP/briefing/"

PT = timezone(timedelta(hours=-7))
MAX_WORKERS = 10
LLM_TIMEOUT = 60
SECTION_MAX_INPUT = 600


# ============================================================
# Infisical / Secrets
# ============================================================

def load_infisical_auth():
    auth = {}
    with open(INFISICAL_AUTH) as f:
        for line in f:
            line = line.strip()
            if line and "=" in line and not line.startswith("#"):
                k, v = line.split("=", 1)
                auth[k.strip()] = v.strip()
    return auth


def get_infisical_token(auth):
    data = json.dumps({
        "clientId": auth["INFISICAL_CLIENT_ID"],
        "clientSecret": auth["INFISICAL_CLIENT_SECRET"],
    }).encode()
    req = urllib.request.Request(
        f"{auth['INFISICAL_URL']}/api/v1/auth/universal-auth/login",
        data=data, headers={"Content-Type": "application/json"}, method="POST",
    )
    with urllib.request.urlopen(req, timeout=10) as r:
        return json.loads(r.read())["accessToken"]


def fetch_secret(token, auth, project_id, key):
    url = (f"{auth['INFISICAL_URL']}/api/v3/secrets/raw/{key}"
           f"?workspaceId={project_id}&environment=prod&secretPath=/")
    req = urllib.request.Request(url, headers={"Authorization": f"Bearer {token}"})
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            return json.loads(r.read())["secret"]["secretValue"]
    except Exception:
        return ""


def load_secrets():
    auth = load_infisical_auth()
    token = get_infisical_token(auth)
    secrets = {}
    for key in ["NEWSAPI_KEY", "GNEWS_API_KEY", "TOMTOM_API_KEY", "LITELLM_MASTER_KEY",
                "GITEA_TOKEN", "JELLYSEERR_API_KEY", "PORTAINER_API_TOKEN",
                "GOOGLE_LUMINA_EMAIL", "GOOGLE_APP_PASSWORD"]:
        secrets[key] = fetch_secret(token, auth, auth["SERVICES_PROJECT_ID"], key)
    lumina_token = fetch_secret(token, auth, auth["IRONCLAW_PROJECT_ID"], "GITEA_TOKEN")
    if lumina_token:
        secrets["LUMINA_GITEA_TOKEN"] = lumina_token
    # Plane token — try env first, then Infisical
    plane_token = os.environ.get("PLANE_TOKEN_LUMINA", "")
    if not plane_token:
        plane_token = fetch_secret(token, auth, auth.get("IRONCLAW_PROJECT_ID", auth.get("SERVICES_PROJECT_ID", "")), "PLANE_TOKEN_LUMINA")
    if plane_token:
        secrets["PLANE_TOKEN_LUMINA"] = plane_token
    return secrets


# ============================================================
# HTTP helper
# ============================================================

def _http_get(url, headers=None, timeout=15):
    hdrs = {"User-Agent": "MooseNet-Briefing/4.0", "Accept": "application/json"}
    if headers:
        hdrs.update(headers)
    req = urllib.request.Request(url, headers=hdrs)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return json.loads(r.read().decode("utf-8", errors="replace"))
    except Exception as e:
        return {"error": str(e)}


# ============================================================
# LLM helper
# ============================================================

def _llm_call(prompt, secrets, max_tokens=200, model=None):
    api_key = secrets.get("LITELLM_MASTER_KEY", "")
    if not api_key:
        return None
    if model is None:
        model = LITELLM_MODELS[0]
    payload = json.dumps({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "max_tokens": max_tokens,
    }).encode()
    req = urllib.request.Request(
        f"{LITELLM_URL}/v1/chat/completions",
        data=payload,
        headers={"Content-Type": "application/json", "Authorization": f"Bearer {api_key}"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=LLM_TIMEOUT) as r:
            return json.loads(r.read())["choices"][0]["message"]["content"]
    except Exception:
        return None


# ============================================================
# GATHER FUNCTIONS (no LLM, parallelizable)
# Each returns raw data for its section
# ============================================================

def gather_news_topic(api_key, category, country="us", limit=5):
    if not api_key:
        return []
    url = (f"https://newsapi.org/v2/top-headlines?"
           f"country={country}&category={category}&pageSize={limit}&apiKey={api_key}")
    data = _http_get(url)
    return [
        {"title": a.get("title", "")[:120], "source": a.get("source", {}).get("name", "")}
        for a in data.get("articles", [])
        if a.get("title") and "[Removed]" not in a.get("title", "")
    ][:limit]


def gather_sports(api_key, teams):
    if not api_key:
        return []
    query = " OR ".join(f'"{t}"' for t in teams)
    url = (f"https://newsapi.org/v2/everything?"
           f"q={urllib.parse.quote(query)}&sortBy=publishedAt&pageSize=5"
           f"&language=en&apiKey={api_key}")
    data = _http_get(url)
    return [
        {"title": a.get("title", "")[:120], "source": a.get("source", {}).get("name", "")}
        for a in data.get("articles", [])
        if a.get("title") and "[Removed]" not in a.get("title", "")
    ][:5]


def gather_weather(location_name, location_query):
    url = f"https://wttr.in/{location_query}?format=j1"
    data = _http_get(url, timeout=10)
    if "error" not in data and "current_condition" in data:
        cc = data["current_condition"][0]
        # Also grab forecast for heat warning
        forecast_high = "?"
        try:
            forecast_high = data["weather"][0]["maxtempF"]
        except (KeyError, IndexError):
            pass
        return {
            "location": location_name,
            "temp_f": cc.get("temp_F", "?"),
            "feels_like_f": cc.get("FeelsLikeF", "?"),
            "condition": cc.get("weatherDesc", [{}])[0].get("value", "Unknown"),
            "humidity": cc.get("humidity", "?"),
            "wind_mph": cc.get("windspeedMiles", "?"),
            "forecast_high_f": forecast_high,
        }
    return {"location": location_name, "error": data.get("error", "Unknown")}


def gather_commute(api_key, origin_coords, dest_coords, label):
    if not api_key:
        return {"label": label, "error": "No TomTom API key"}
    url = (f"https://api.tomtom.com/routing/1/calculateRoute/"
           f"{origin_coords}:{dest_coords}/json"
           f"?key={api_key}&traffic=true&travelMode=car")
    data = _http_get(url)
    if "error" in data:
        return {"label": label, "error": data["error"]}
    try:
        summary = data["routes"][0]["summary"]
        return {
            "label": label,
            "travel_min": round(summary["travelTimeInSeconds"] / 60),
            "delay_min": round(summary.get("trafficDelayInSeconds", 0) / 60),
            "distance_mi": round(summary["lengthInMeters"] / 1609.34, 1),
        }
    except (KeyError, IndexError):
        return {"label": label, "error": "Parse error"}


def gather_crypto():
    url = ("https://api.coingecko.com/api/v3/simple/price"
           "?ids=bitcoin,ethereum,solana,dogecoin&vs_currencies=usd&include_24hr_change=true")
    data = _http_get(url)
    if "error" in data:
        return data
    results = {}
    for coin, info in data.items():
        results[coin] = {
            "price_usd": info.get("usd", "?"),
            "change_24h": round(info.get("usd_24h_change", 0), 2),
        }
    return results


def gather_jellyseerr_pending(api_key):
    """Fetch pending media requests from Jellyseerr."""
    if not api_key:
        return {"error": "No Jellyseerr key"}
    data = _http_get(f"{JELLYSEERR_URL}/api/v1/request?take=10&filter=pending",
                     headers={"X-Api-Key": api_key})
    if "error" in data:
        return data
    results = data.get("results", [])
    pending = []
    for r in results:
        media = r.get("media", {})
        pending.append({
            "title": media.get("mediaInfo", {}).get("title", r.get("type", "Unknown")),
            "type": r.get("type", "?"),
            "status": r.get("status", "?"),
            "requested_by": r.get("requestedBy", {}).get("displayName", "?"),
        })
    return {"count": len(pending), "requests": pending}


def gather_plex_health(portainer_token):
    """Quick Plex ecosystem check via Portainer."""
    if not portainer_token:
        return {"error": "No Portainer token"}
    import socket
    checks = {}
    for name, host, port in [("plex", "YOUR_PVM_HOST_IP", 32400),
                              ("jellyseerr", "YOUR_PVM_HOST_IP", 5055),
                              ("tautulli", "YOUR_PVM_HOST_IP", 8181)]:
        try:
            sock = socket.create_connection((host, port), timeout=5)
            sock.close()
            checks[name] = True
        except Exception:
            checks[name] = False
    return checks


def gather_cluster_health(portainer_token):
    """Cluster health via Prometheus + Portainer."""
    result = {}
    # Prometheus targets
    data = _http_get(f"{PROMETHEUS_URL}/api/v1/query?query=up")
    if "error" not in data and data.get("status") == "success":
        targets = data.get("data", {}).get("result", [])
        up = sum(1 for t in targets if t.get("value", [None, "0"])[1] == "1")
        result["prometheus"] = {"up": up, "total": len(targets)}
    else:
        result["prometheus"] = {"error": "unreachable"}
    # Prometheus alerts
    alerts_data = _http_get(f"{PROMETHEUS_URL}/api/v1/alerts")
    if "error" not in alerts_data:
        alerts = alerts_data.get("data", {}).get("alerts", [])
        firing = [a for a in alerts if a.get("state") == "firing"]
        result["alerts"] = len(firing)
    else:
        result["alerts"] = "?"
    return result


def gather_fun_fact():
    """Grab a random fun fact."""
    data = _http_get("https://uselessfacts.jsph.pl/api/v2/facts/random?language=en")
    if "error" not in data:
        return {"fact": data.get("text", "No fact available")}
    # Fallback facts
    facts = [
        "Honey never spoils. Archaeologists found 3000-year-old honey in Egyptian tombs that was still edible.",
        "Octopuses have three hearts and blue blood.",
        "A group of flamingos is called a 'flamboyance'.",
        "The inventor of the Pringles can is buried in one.",
        "Venus is the only planet that spins clockwise.",
    ]
    return {"fact": random.choice(facts)}


def gather_seti():
    """Check for SETI/space news from NASA."""
    data = _http_get("https://api.nasa.gov/planetary/apod?api_key=DEMO_KEY")
    result = {}
    if "error" not in data:
        result["apod_title"] = data.get("title", "?")
        result["apod_explanation"] = data.get("explanation", "")[:300]
    else:
        result["apod_error"] = data.get("error", "?")
    return result


def gather_stock_movers():
    """Get major index movement from a free source."""
    # Using CoinGecko for crypto (already gathered) and a simple market summary
    # For stocks, we use a free news approach since real-time stock APIs need paid keys
    result = {
        "note": "Real-time stock data requires premium API. Using news-based summary.",
    }
    return result


def gather_ansible_log():
    """Check for recent Ansible run logs. Reads from Gitea if available."""
    # Placeholder — needs Ansible log file access or MCP tool
    return {"note": "Ansible log integration pending. Connect via CT214 MCP tools."}


def gather_reflection(gitea_token):
    """Read Lumina's latest reflection from lumina-engram repo."""
    if not gitea_token:
        return {"error": "No Gitea token"}
    # Try lumina-engram first, fall back to lumina-memory-repo
    for repo in ["lumina-engram", "lumina-memory-repo"]:
        try:
            url = (f"{GITEA_URL}/api/v1/repos/moosenet/{repo}"
                   f"/contents/logs/latest-reflection.md?ref=main")
            req = urllib.request.Request(url, headers={"Authorization": f"token {gitea_token}"})
            with urllib.request.urlopen(req, timeout=10) as r:
                data = json.loads(r.read())
                content = base64.b64decode(data["content"]).decode()
                return {"reflection": content[:500]}
        except Exception:
            continue
    return {"reflection": "No recent reflection available."}


def gather_crucible():
    """Get current learning streak and top active tracks from Crucible (CT310)."""
    try:
        import subprocess
        streak_result = subprocess.run(
            ["python3", "/opt/lumina-fleet/crucible/crucible.py", "streak"],
            capture_output=True, text=True, timeout=10
        )
        tracks_result = subprocess.run(
            ["python3", "/opt/lumina-fleet/crucible/crucible.py", "tracks"],
            capture_output=True, text=True, timeout=10
        )
        streak = json.loads(streak_result.stdout) if streak_result.returncode == 0 else {}
        tracks_raw = json.loads(tracks_result.stdout) if tracks_result.returncode == 0 else []
        tracks = tracks_raw if isinstance(tracks_raw, list) else tracks_raw.get("tracks", [])
        return {
            "streak": streak.get("current_streak", 0),
            "sessions_total": streak.get("sessions_total", 0),
            "active_tracks": len(tracks),
            "top_tracks": [{"name": t["name"], "type": t["type"], "last_session": t.get("last_session", "never"),
                           "streak_days": t.get("streak_days", 0)} for t in tracks[:3]],
        }
    except Exception as e:
        return {"error": str(e)}


def gather_calendar():
    """Get today's Google Calendar events from ALL calendars: lumina own + the operator's personal (<operator-personal-email>) + Lumina Actions."""
    try:
        import caldav
        from datetime import date, datetime as dt
        email = os.environ.get('GOOGLE_LUMINA_EMAIL', '')
        password = os.environ.get('GOOGLE_APP_PASSWORD', '')
        if not email or not password:
            return {'events': [], 'note': 'Google credentials not set'}
        today_start = dt.combine(date.today(), dt.min.time())
        today_end = dt.combine(date.today(), dt.max.time())
        events = []
        seen_uids = set()
        # All calendar IDs to query
        peter_email = os.environ.get('GOOGLE_PETER_EMAIL', '<operator-personal-email>')
        lumina_cal_id = os.environ.get('GOOGLE_LUMINA_CALENDAR_ID', '')
        cal_ids = [email, peter_email]
        if lumina_cal_id:
            cal_ids.append(lumina_cal_id)
        queried = set()
        for cal_id in cal_ids:
            if cal_id in queried:
                continue
            queried.add(cal_id)
            try:
                cal_url = 'https://www.google.com/calendar/dav/' + cal_id + '/events'
                client = caldav.DAVClient(url=cal_url, username=email, password=password)
                cal = caldav.Calendar(client=client, url=cal_url)
                for evt in cal.date_search(start=today_start, end=today_end, expand=True):
                    comp = evt.icalendar_component
                    uid = str(comp.get('UID', ''))
                    if uid and uid in seen_uids:
                        continue
                    if uid:
                        seen_uids.add(uid)
                    dtstart = comp.get('DTSTART')
                    summary = str(comp.get('SUMMARY', ''))
                    if summary:
                        cal_name = cal_id.split('@')[0][:20]
                        events.append({
                            'title': summary[:80],
                            'start': str(dtstart.dt) if dtstart else '',
                            'calendar': cal_name,
                        })
            except Exception:
                pass
        events.sort(key=lambda x: x.get('start', ''))
        return {'date': date.today().isoformat(), 'count': len(events), 'events': events[:15]}
    except Exception as e:
        return {'events': [], 'error': str(e)[:100]}


def gather_travel_advisory():
    """Fetch travel advisories. Using a simple approach for now."""
    # State Dept RSS or similar — placeholder until proper API
    return {"note": "Travel advisory integration pending. Will connect to State Dept API + flight deal feeds."}


def gather_renewals(days_ahead=30):
    """Return documents expiring within the next N days from renewal_tracker.py."""
    import subprocess
    tracker = "/opt/lumina-fleet/relay/renewal_tracker.py"
    try:
        result = subprocess.run(
            ["python3", tracker, "list", "--days", str(days_ahead)],
            capture_output=True, text=True, timeout=10,
        )
        if result.returncode == 0:
            data = json.loads(result.stdout)
            return data
        return {"error": result.stderr.strip()[:200]}
    except Exception as e:
        return {"error": str(e)}



def gather_myelin_cost():
    """Get yesterday's inference cost summary from Myelin."""
    import subprocess
    usage_file = "/opt/lumina-fleet/myelin/output/usage.json"
    try:
        import os
        if os.path.exists(usage_file):
            import json as j
            with open(usage_file) as f:
                data = j.load(f)
            today = data.get("today", {})
            per_agent = data.get("per_agent", {})
            # Format compact summary
            total = today.get("cost_usd", 0)
            top_agents = sorted(per_agent.items(), key=lambda x: x[1].get("cost", 0), reverse=True)[:3]
            summary = {"total_cost_usd": total, "calls": today.get("calls", 0), "top_agents": {k: v for k, v in top_agents}}
            return summary
        return {"total_cost_usd": 0, "note": "No Myelin data yet — collecting soon"}
    except Exception as e:
        return {"error": str(e)[:100]}


def gather_plane_tasks(plane_token):
    """Fetch high-priority open tasks from the LM Plane project."""
    if not plane_token:
        return {"error": "No Plane token"}
    url = (f"{PLANE_URL}/api/v1/workspaces/moosenet/projects/{PLANE_LM_PROJECT_ID}"
           f"/issues/?per_page=50")
    data = _http_get(url, headers={"X-API-Key": plane_token})
    if "error" in data:
        return data
    results = data.get("results", [])
    # Filter: In Progress or Todo, with urgent/high priority
    priority_map = {0: "none", 1: "urgent", 2: "high", 3: "medium", 4: "low"}
    wanted_priorities = {1, 2}  # urgent, high
    # State names we want (In Progress or Todo)
    wanted_state_groups = {"unstarted", "started"}
    filtered = []
    for issue in results:
        priority = issue.get("priority", 0)
        state_detail = issue.get("state_detail", {})
        state_group = state_detail.get("group", "")
        if priority in wanted_priorities and state_group in wanted_state_groups:
            filtered.append({
                "id": issue.get("sequence_id", "?"),
                "name": issue.get("name", "")[:80],
                "priority": priority_map.get(priority, "?"),
                "state": state_detail.get("name", "?"),
            })
    # Sort: urgent first, then high; limit to 5
    filtered.sort(key=lambda x: x["priority"])
    filtered = filtered[:5]
    return {"count": len(filtered), "items": filtered}


# ============================================================
# FORMAT FUNCTION — generic section formatter
# ============================================================

def format_section(section_type, raw_data, secrets, date_str="", model=None):
    """Format a single briefing section with a small focused LLM call."""
    if isinstance(raw_data, list):
        compact = "\n".join(f"- {item.get('title', str(item))} ({item.get('source', '')})"
                           for item in raw_data[:5])
    elif isinstance(raw_data, dict):
        compact = json.dumps(raw_data, separators=(',', ':'))
    else:
        compact = str(raw_data)

    if len(compact) > SECTION_MAX_INPUT:
        compact = compact[:SECTION_MAX_INPUT] + "..."

    prompts = {
        "tech_news": f"You are Lumina. Write 2-3 punchy bullet points summarizing these tech headlines. Be witty and direct. No intro.\n\n{compact}",
        "business_news": f"You are Lumina. Write 2-3 punchy bullet points on these business headlines. Focus on what matters. No intro.\n\n{compact}",
        "general_news": f"You are Lumina. Write 2-3 quick bullet points on these headlines. Skip fluff. No intro.\n\n{compact}",
        "sports": f"You are Lumina. The operator follows their local sports teams, Warriors, Sharks, 49ers, Valkyries. Write 2-3 bullet points on these sports stories. Be fun. No intro.\n\n{compact}",
        "weather": f"You are Lumina. Write a 1-2 sentence weather summary from this data. Include both locations. No intro.\n\n{compact}",
        "commute": f"You are Lumina. Write a 1 sentence commute summary from this data. Include time and traffic note. No intro.\n\n{compact}",
        "crypto": f"You are Lumina. Write a 1-2 sentence crypto market summary. Use direction arrows (up/down). No intro.\n\n{compact}",
        "outfit": f"You are Lumina. Based on this weather, suggest what the user should wear/bring today in one sentence. Be practical and fun.\n\n{compact}",
        "jellyseerr": f"You are Lumina. Summarize these pending media requests in 1-2 sentences. Alert the operator to go approve them if any exist. No intro.\n\n{compact}",
        "plex_health": f"You are Lumina. Summarize this Plex health check in one sentence. Only mention issues. If all good, say so briefly. No intro.\n\n{compact}",
        "heat_warning": f"You are Lumina. The forecast high is in this data. If over 85°F, alert the operator about homelab power draw. If under 85, say 'No heat concerns today.' One sentence. No intro.\n\n{compact}",
        "cluster_health": f"You are Lumina. Summarize this cluster health data in 1-2 sentences. Mention target count and any alerts. No intro.\n\n{compact}",
        "fun_fact": f"You are Lumina. Present this fun fact in a quirky, delightful way. One sentence. Add a brief witty comment. No intro.\n\n{compact}",
        "seti": f"You are Lumina. the operator loves space. Present NASA's Astronomy Picture of the Day in 1-2 exciting sentences. No intro.\n\n{compact}",
        "stock_movers": f"You are Lumina. Note that real-time stock data is pending integration. Mention the crypto data covers market sentiment for now. One sentence. No intro.\n\n{compact}",
        "ansible_log": f"You are Lumina. Note that Ansible log integration is coming soon. One sentence. No intro.\n\n{compact}",
        "reflection": f"You are Lumina. Summarize this self-reflection in 1-2 sentences. Be introspective but light. No intro.\n\n{compact}",
        "travel_advisory": f"You are Lumina. Note that travel advisory integration is coming soon. One fun travel-related sentence. No intro.\n\n{compact}",
        "today_tasks": f"You are Lumina. Summarize these open high-priority Plane tasks in one line: '📋 Today's Focus: X open items — [item1], [item2], ...'. Be terse. No intro.\n\n{compact}",
        "inference_cost": f"You are Lumina. Summarize yesterday's inference cost in one line: '💡 Yesterday: $X.XX total ([agent]: $X, [agent]: $X) · [N] calls'. If no data, say '💡 Inference tracking starting up'. No intro.\n\n{compact}",
        "crucible": f"You are Lumina. Summarize the operator's learning streak in one line: '📚 Learning: Day N streak · [top track name] · X total sessions'. If no data, say 'No active learning tracks yet'. Be encouraging. No intro.\n\n{compact}",
        "calendar": f"You are Lumina. Summarize today's calendar events for the operator. List events as '📅 HH:MM - Event title' each on a new line. If no events, say '📅 Clear schedule today'. No intro.\n\n{compact}",
        "renewals": f"You are Lumina. Summarize upcoming document renewals for the operator. List each as '🔄 [Doc type] — expires [date] ([N] days)'. If none due soon, say '🔄 No renewals due in the next 30 days.' No intro.\n\n{compact}",
    }

    prompt = prompts.get(section_type, f"Summarize briefly:\n\n{compact}")
    result = _llm_call(prompt, secrets, max_tokens=200, model=model)
    return result if result else compact


def format_greeting(briefing_type, date_str, secrets):
    prompt = (f"You are Lumina, the operator's AI assistant. Write a one-line {briefing_type} greeting for {date_str}. "
              f"Be warm, quirky, upbeat. Just the greeting, nothing else.")
    return _llm_call(prompt, secrets, max_tokens=80) or f"Good {'morning' if briefing_type == 'morning' else 'afternoon'}, the operator!"


def format_signoff(briefing_type, secrets):
    prompt = (f"You are Lumina. Write a one-line {briefing_type} signoff for the operator. "
              f"Warm, direct, maybe a little cheeky. End with — Lumina. Just the signoff.")
    return _llm_call(prompt, secrets, max_tokens=60) or "— Lumina"


# ============================================================
# Gitea write
# ============================================================

def write_to_gitea(content, filepath, secrets, message="Briefing update"):
    token = secrets.get("LUMINA_GITEA_TOKEN", secrets.get("GITEA_TOKEN", ""))
    if not token:
        print("  ERROR: No Gitea token")
        return False
    headers = {"Authorization": f"token {token}", "Content-Type": "application/json"}
    url = f"{GITEA_URL}/api/v1/repos/{GITEA_REPO_OWNER}/{GITEA_REPO}/contents/{filepath}"
    sha = ""
    try:
        req = urllib.request.Request(f"{url}?ref={GITEA_BRANCH}", headers=headers)
        with urllib.request.urlopen(req, timeout=10) as r:
            sha = json.loads(r.read()).get("sha", "")
    except Exception:
        pass
    payload = {"message": message, "content": base64.b64encode(content.encode()).decode(), "branch": GITEA_BRANCH}
    if sha:
        payload["sha"] = sha
    method = "PUT" if sha else "POST"
    req = urllib.request.Request(url, data=json.dumps(payload).encode(), headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            print(f"  Wrote {filepath} ({method})")
            return True
    except urllib.error.HTTPError as e:
        raw = e.read().decode()[:200]
        if method == "PUT" and e.code == 422:
            payload.pop("sha", None)
            req2 = urllib.request.Request(url, data=json.dumps(payload).encode(), headers=headers, method="POST")
            try:
                with urllib.request.urlopen(req2, timeout=15) as r2:
                    print(f"  Wrote {filepath} (POST fallback)")
                    return True
            except Exception:
                pass
        print(f"  ERROR: Gitea write failed: {e.code} {raw}")
        return False


# ============================================================
# SECTION REGISTRY
# Each section defines: emoji, header, gather function, format type
# Morning and afternoon briefings pick from this registry
# ============================================================

MORNING_SECTIONS = [
    ("today_tasks",     "📋 Today's Focus"),
    ("calendar",         "📅 Calendar"),
    ("crucible",        "📚 Learning"),
    ("outfit",          "👔 What to Wear"),
    ("commute",         "🚗 Commute"),
    ("weather",         "🌤️ Weather"),
    ("heat_warning",    "🌡️ Heat Alert"),
    ("tech_news",       "🤖 Tech & AI"),
    ("business_news",   "💼 Business"),
    ("general_news",    "📰 Headlines"),
    ("sports",          "🏀 Sports"),
    ("crypto",          "💰 Crypto"),
    ("plex_health",     "🎬 Plex"),
    ("jellyseerr",      "📺 Media Requests"),
    ("cluster_health",  "🖥️ Cluster"),
    ("fun_fact",        "🎲 Fun Fact"),
    ("seti",            "🛸 Space"),
    ("reflection",      "🪞 Lumina's Reflection"),
    ("renewals",        "🔄 Renewals"),
    ("inference_cost",  "💡 Inference Cost"),
]

AFTERNOON_SECTIONS = [
    ("commute",         "🚗 Commute Home"),
    ("weather",         "🌤️ Evening Weather"),
    ("tech_news",       "🤖 Tech & AI"),
    ("business_news",   "💼 Business"),
    ("sports",          "🏀 Sports"),
    ("crypto",          "💰 Crypto"),
    ("jellyseerr",      "📺 Media Requests"),
    ("cluster_health",  "🖥️ Cluster"),
    ("fun_fact",        "🎲 Fun Fact"),
]


# ============================================================
# Main orchestrator
# ============================================================

def run_briefing(briefing_type):
    now = datetime.now(PT)
    date_str = now.strftime("%Y-%m-%d")
    date_display = now.strftime("%A, %B %d, %Y")
    time_str = now.strftime("%I:%M %p PT")

    print(f"[{_dn('vigil').lower()}] Starting {briefing_type} briefing for {date_str}")

    print("  Loading secrets...")
    secrets = load_secrets()
    api_key = secrets.get("NEWSAPI_KEY", "")
    tomtom_key = secrets.get("TOMTOM_API_KEY", "")
    jellyseerr_key = secrets.get("JELLYSEERR_API_KEY", "")
    portainer_token = secrets.get("PORTAINER_API_TOKEN", "")
    gitea_token = secrets.get("LUMINA_GITEA_TOKEN", secrets.get("GITEA_TOKEN", ""))
    plane_token = secrets.get("PLANE_TOKEN_LUMINA", "")

    # Inject Google credentials into environment for gather_calendar()
    if secrets.get("GOOGLE_LUMINA_EMAIL"):
        os.environ["GOOGLE_LUMINA_EMAIL"] = secrets["GOOGLE_LUMINA_EMAIL"]
    if secrets.get("GOOGLE_APP_PASSWORD"):
        os.environ["GOOGLE_APP_PASSWORD"] = secrets["GOOGLE_APP_PASSWORD"]

    # ── Phase 1: Gather data (parallel) ──
    print("  Gathering data...")
    raw_data = {}

    with ThreadPoolExecutor(max_workers=MAX_WORKERS) as pool:
        futures = {}

        # News topics
        for cat in ["technology", "business", "general"]:
            futures[pool.submit(gather_news_topic, api_key, cat)] = f"news_{cat}"

        # Sports
        teams = ["SF Giants", "Golden State Warriors", "San Jose Sharks", "SF 49ers", "Golden State Valkyries"]
        futures[pool.submit(gather_sports, api_key, teams)] = "sports"

        # Weather (both locations)
        futures[pool.submit(gather_weather, "San Francisco", "San+Francisco")] = "weather_sf"
        futures[pool.submit(gather_weather, "YOUR_WORK_CITY", "YOUR_WORK_CITY,STATE")] = "weather_fc"

        # Commute
        if briefing_type == "morning":
            futures[pool.submit(gather_commute, tomtom_key,
                               os.environ.get("COMMUTE_ORIGIN_LATLON", ""), os.environ.get("COMMUTE_DEST_LATLON", ""), "home → work")] = "commute"
        else:
            futures[pool.submit(gather_commute, tomtom_key,
                               os.environ.get("COMMUTE_DEST_LATLON", ""), os.environ.get("COMMUTE_ORIGIN_LATLON", ""), "work → home")] = "commute"

        # Crypto
        futures[pool.submit(gather_crypto)] = "crypto"

        # Jellyseerr
        futures[pool.submit(gather_jellyseerr_pending, jellyseerr_key)] = "jellyseerr"

        # Plex health
        futures[pool.submit(gather_plex_health, portainer_token)] = "plex_health"

        # Cluster health
        futures[pool.submit(gather_cluster_health, portainer_token)] = "cluster_health"

        # Fun fact
        futures[pool.submit(gather_fun_fact)] = "fun_fact"

        # SETI / Space
        futures[pool.submit(gather_seti)] = "seti"

        # Stock movers (placeholder)
        futures[pool.submit(gather_stock_movers)] = "stock_movers"

        # Lumina's reflection
        futures[pool.submit(gather_reflection, gitea_token)] = "reflection"

        # Today's Plane tasks (morning only)
        if briefing_type == "morning":
            futures[pool.submit(gather_plane_tasks, plane_token)] = "plane_tasks"

        # Crucible learning streak (morning only)
        if briefing_type == "morning":
            futures[pool.submit(gather_crucible)] = "crucible"

        # Google Calendar (morning only)
        if briefing_type == "morning":
            futures[pool.submit(gather_calendar)] = "calendar"

        # Document renewals (morning only)
        if briefing_type == "morning":
            futures[pool.submit(gather_renewals, 30)] = "renewals"

        # Myelin cost summary (morning only)
        if briefing_type == "morning":
            futures[pool.submit(gather_myelin_cost)] = "inference_cost"

        for future in as_completed(futures):
            key = futures[future]
            try:
                raw_data[key] = future.result()
            except Exception as e:
                raw_data[key] = {"error": str(e)}

    print(f"  Data gathered: {len(raw_data)} sources")

    # Write raw data
    print("  Writing raw data to Gitea...")
    write_to_gitea(json.dumps(raw_data, indent=2),
                   f"vigil/briefings/{date_str}-{briefing_type}-raw.json", secrets,
                   message=f"Raw {briefing_type} data {date_str}")

    # ── Phase 2: Format sections (parallel LLM) ──
    print("  Formatting sections...")

    weather_combined = {"sf": raw_data.get("weather_sf", {}), "fc": raw_data.get("weather_fc", {})}

    section_data_map = {
        "outfit":          weather_combined,
        "commute":         raw_data.get("commute", {}),
        "weather":         weather_combined,
        "heat_warning":    weather_combined,
        "tech_news":       raw_data.get("news_technology", []),
        "business_news":   raw_data.get("news_business", []),
        "general_news":    raw_data.get("news_general", []),
        "sports":          raw_data.get("sports", []),
        "crypto":          raw_data.get("crypto", {}),
        "jellyseerr":      raw_data.get("jellyseerr", {}),
        "plex_health":     raw_data.get("plex_health", {}),
        "cluster_health":  raw_data.get("cluster_health", {}),
        "fun_fact":        raw_data.get("fun_fact", {}),
        "seti":            raw_data.get("seti", {}),
        "stock_movers":    raw_data.get("stock_movers", {}),
        "reflection":      raw_data.get("reflection", {}),
        "today_tasks":     raw_data.get("plane_tasks", {}),
        "calendar":         raw_data.get("calendar", {}),
        "inference_cost": f"You are Lumina. Summarize yesterday's inference cost in one line: '💡 Yesterday: $X.XX total ([agent]: $X, [agent]: $X) · [N] calls'. If no data, say '💡 Inference tracking starting up'. No intro.\n\n{compact}",
        "crucible":        raw_data.get("crucible", {}),
        "renewals":        raw_data.get("renewals", {}),
        "inference_cost":  raw_data.get("inference_cost", {}),
    }

    sections = {}
    section_list = MORNING_SECTIONS if briefing_type == "morning" else AFTERNOON_SECTIONS

    with ThreadPoolExecutor(max_workers=MAX_WORKERS) as pool:
        futures = {}
        for section_key, header in section_list:
            data = section_data_map.get(section_key, {})
            model = LITELLM_MODELS[0]
            futures[pool.submit(format_section, section_key, data, secrets, date_str, model)] = section_key

        futures[pool.submit(format_greeting, briefing_type, date_display, secrets)] = "_greeting"
        futures[pool.submit(format_signoff, briefing_type, secrets)] = "_signoff"

        for future in as_completed(futures):
            key = futures[future]
            try:
                sections[key] = future.result()
            except Exception as e:
                sections[key] = f"(unavailable: {e})"

    print(f"  Sections formatted: {len(sections)}")

    # ── Phase 3: Assemble ──
    print("  Assembling briefing...")

    greeting = sections.pop("_greeting", f"Good {briefing_type}, the operator!")
    signoff = sections.pop("_signoff", "— Lumina")

    # ── Phase 3a: Generate HTML dashboard ──
    print("  Generating HTML dashboard...")
    all_sections = {"greeting": greeting}
    for section_key, _header in section_list:
        content = sections.get(section_key, "")
        if content and "(unavailable" not in content:
            all_sections[section_key] = content
    try:
        dashboard_path = generate_dashboard(all_sections, briefing_type)
        print(f"  Dashboard written: {dashboard_path}")
    except Exception as e:
        print(f"  WARNING: Dashboard generation failed: {e}")

    # ── Phase 3b: Assemble Markdown briefing ──
    parts = [f"# ✦ {greeting}\n**{date_display} | {time_str}**\n\n📊 **Dashboard:** {DASHBOARD_URL}\n\n---\n"]

    for section_key, header in section_list:
        content = sections.get(section_key, "")
        if content and "(unavailable" not in content and "pending" not in content.lower()[:20]:
            parts.append(f"## {header}\n{content}\n")

    parts.append(f"---\n\n{signoff}")
    briefing = "\n".join(parts)

    # Build one-line summary for Matrix notification
    summary_parts = []
    if all_sections.get("weather"):
        summary_parts.append(str(all_sections["weather"])[:60])
    if all_sections.get("today_tasks"):
        tasks_text = str(all_sections["today_tasks"])
        # Extract just the task count/summary
        if "📋" in tasks_text:
            summary_parts.append(tasks_text.split("\n")[0][:60])
        else:
            summary_parts.append(tasks_text[:60])
    one_line_summary = " · ".join(summary_parts) if summary_parts else f"{len(all_sections)-1} sections ready"

    briefing_emoji = "🌅" if briefing_type == "morning" else "🌆"
    matrix_message = f"{briefing_emoji} Briefing ready: {DASHBOARD_URL} — {one_line_summary}"

    # ── Phase 4: Write ──
    print("  Writing briefing to Gitea...")
    write_to_gitea(briefing, f"vigil/briefings/{date_str}-{briefing_type}.md", secrets,
                   message=f"{briefing_type.title()} briefing {date_str}")
    write_to_gitea(briefing, f"vigil/briefings/latest-{briefing_type}.md", secrets,
                   message=f"Latest {briefing_type} briefing")

    print(f"[{_dn('vigil').lower()}] {briefing_type} briefing complete")
    print(f"  Sections: {len(section_list)}")
    print(f"  Matrix message: {matrix_message}")
    print(f"  Dashboard: {DASHBOARD_URL}")


if __name__ == "__main__":
    if len(sys.argv) < 2 or sys.argv[1] not in ("morning", "afternoon"):
        print("Usage: python3 briefing.py morning|afternoon")
        sys.exit(1)
    run_briefing(sys.argv[1])
