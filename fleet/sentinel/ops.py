#!/usr/bin/env python3
"""
Agent Ops — MooseNet Operational Monitoring Sub-Agent
Runs on CT310. Performs health checks, system snapshots, and logging
using Prometheus metrics + direct API calls + local Qwen for summarization.
Writes results to Gitea repos.

Usage:
    python3 ops.py plex-health
    python3 ops.py self-health
    python3 ops.py vm901-watchdog
    python3 ops.py gitea-health
    python3 ops.py system-snapshot
    python3 ops.py commute-tracker [morning|afternoon]
    python3 ops.py daily-log
    python3 ops.py reflection
    python3 ops.py tool-usage-log
    python3 ops.py memory-curation
"""

import json
import os
import sys
import base64
import urllib.request
import urllib.parse
import urllib.error
from datetime import datetime, timezone, timedelta

# Pulse — temporal awareness for duration-aware alerts (SP.C4)
sys.path.insert(0, '/opt/lumina-fleet/shared')
try:
    import pulse as _pulse
    _PULSE_OK = True
except ImportError:
    _PULSE_OK = False


def _since_str(marker: str) -> str:
    """Return ' (down for Xh Ym)' if pulse marker exists, else ''."""
    if not _PULSE_OK:
        return ''
    s = _pulse.since(marker)
    return f' (down for {s})' if s else ''

# ============================================================
# Config
# ============================================================

INFISICAL_AUTH = "/opt/briefing-agent/.infisical-auth"
LITELLM_URL = "http://YOUR_LITELLM_IP:4000"
LITELLM_MODEL = "Lumina Fast"
LITELLM_MODELS = [LITELLM_MODEL]
GITEA_URL = "http://YOUR_GITEA_IP:3000"
GITEA_REPO_OWNER = "moosenet"
GITEA_REPO = "agent-ops"
GITEA_MEMORY_REPO = "lumina-memory-repo"
GITEA_BRANCH = "main"

PROMETHEUS_URL = "http://YOUR_PROMETHEUS_IP:9090"
JELLYSEERR_URL = "http://YOUR_PVM_HOST_IP:5055"
OLLAMA_GPU_URL = "http://YOUR_GPU_HOST_IP:11434"
OLLAMA_CPU_URL = "http://YOUR_CPU_OLLAMA_IP:11434"
TOMTOM_BASE = "https://api.tomtom.com/routing/1/calculateRoute"

PT = timezone(timedelta(hours=-7))


# ============================================================
# Shared utilities
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
    for key in ["GITEA_TOKEN", "LITELLM_MASTER_KEY", "JELLYSEERR_API_KEY", "TOMTOM_API_KEY"]:
        secrets[key] = fetch_secret(token, auth, auth["SERVICES_PROJECT_ID"], key)
    return secrets


def _http_get(url, headers=None, timeout=10):
    hdrs = {"User-Agent": "MooseNet-Ops/2.0", "Accept": "application/json"}
    if headers:
        hdrs.update(headers)
    req = urllib.request.Request(url, headers=hdrs)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return json.loads(r.read().decode("utf-8", errors="replace"))
    except urllib.error.HTTPError as e:
        return {"error": f"HTTP {e.code}", "body": e.read().decode()[:200]}
    except Exception as e:
        return {"error": str(e)}


def _prometheus_query(query):
    url = f"{PROMETHEUS_URL}/api/v1/query?query={urllib.parse.quote(query)}"
    data = _http_get(url)
    if data.get("status") == "success":
        return data.get("data", {}).get("result", [])
    return []


def _prometheus_targets_by_role(*roles):
    results = {}
    all_targets = _prometheus_query("up")
    for t in all_targets:
        metric = t.get("metric", {})
        role = metric.get("role", "")
        name = metric.get("name", metric.get("instance", "?"))
        is_up = t.get("value", [None, "0"])[1] == "1"
        if role in roles or not roles:
            results[name] = {"role": role, "up": is_up, "node": metric.get("node", "?")}
    return results


def _format_with_llm(prompt, secrets, max_tokens=500):
    api_key = secrets.get("LITELLM_MASTER_KEY", "")
    if not api_key:
        return None
    payload = json.dumps({
        "model": LITELLM_MODEL,
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
        with urllib.request.urlopen(req, timeout=120) as r:
            resp = json.loads(r.read())
            return resp["choices"][0]["message"]["content"]
    except Exception as e:
        print(f"  WARN: LLM formatting failed: {e}")
        return None


def write_to_gitea(content, filepath, secrets, repo=None, message="ops update"):
    token = secrets.get("GITEA_TOKEN", "")
    if not token:
        print("  ERROR: No Gitea token")
        return False
    if repo is None:
        repo = GITEA_REPO
    headers = {"Authorization": f"token {token}", "Content-Type": "application/json"}
    url = f"{GITEA_URL}/api/v1/repos/{GITEA_REPO_OWNER}/{repo}/contents/{filepath}"
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
# Health Checks — Prometheus-Powered
# ============================================================

def op_plex_health(secrets):
    now = datetime.now(PT)
    checks = {}
    targets = _prometheus_targets_by_role("plex", "media-vpn")
    checks["prometheus_targets"] = targets
    jellyseerr_key = secrets.get("JELLYSEERR_API_KEY", "")
    if jellyseerr_key:
        data = _http_get(f"{JELLYSEERR_URL}/api/v1/status", headers={"X-Api-Key": jellyseerr_key})
        checks["jellyseerr"] = {"status": "healthy", "version": data.get("version", "?")} if "error" not in data else {"status": "down", "error": data.get("error", "?")}
    else:
        checks["jellyseerr"] = {"status": "no_key"}
    jellyseerr_ok = checks.get("jellyseerr", {}).get("status") == "healthy"
    status = "healthy" if jellyseerr_ok else "degraded"
    report = f"# Plex Health — {now.strftime('%Y-%m-%d %H:%M PT')}\n\n**Status:** {status}\n\n## Prometheus Targets\n"
    for name, info in targets.items():
        report += f"- {'✅' if info['up'] else '❌'} {name} ({info['role']})\n"
    if not targets:
        report += "- No plex-related targets found\n"
    report += f"\n## Jellyseerr\n- {'✅ v' + checks['jellyseerr'].get('version', '?') if jellyseerr_ok else '❌ ' + str(checks['jellyseerr'])}\n"
    return {"status": status, "report": report, "raw": checks}


def op_self_health(secrets):
    now = datetime.now(PT)
    critical_roles = ["ironclaw", "litellm", "mcp-hub", "tuwunel", "coredns", "prometheus", "gitea", "ollama-gpu", "ollama-cpu", "arcade-briefly-ops", "infisical"]
    targets = _prometheus_targets_by_role(*critical_roles)
    up_count = sum(1 for t in targets.values() if t["up"])
    total = len(targets)
    status = "healthy" if up_count == total and total > 0 else "degraded"
    report = f"# Self Health — {now.strftime('%Y-%m-%d %H:%M PT')}\n\n**Status:** {status} ({up_count}/{total} targets up)\n\n"
    for name, info in sorted(targets.items(), key=lambda x: (x[1]["up"], x[0])):
        report += f"- {'✅' if info['up'] else '❌'} {name} ({info['role']})\n"
    return {"status": status, "report": report, "raw": {"targets": {k: v for k, v in targets.items()}}}


def op_vm901_watchdog(secrets):
    now = datetime.now(PT)
    checks = {}
    targets = _prometheus_targets_by_role("ollama-gpu", "ollama-cpu")
    checks["prometheus"] = targets
    gpu_data = _http_get(f"{OLLAMA_GPU_URL}/api/tags")
    if "error" not in gpu_data:
        checks["gpu_ollama"] = {"status": "healthy", "models": [m.get("name", "?") for m in gpu_data.get("models", [])]}
    else:
        checks["gpu_ollama"] = {"status": "down", "error": gpu_data.get("error", "?")}
    cpu_data = _http_get(f"{OLLAMA_CPU_URL}/api/tags")
    if "error" not in cpu_data:
        checks["cpu_ollama"] = {"status": "healthy", "models": [m.get("name", "?") for m in cpu_data.get("models", [])]}
    else:
        checks["cpu_ollama"] = {"status": "down", "error": cpu_data.get("error", "?")}
    gpu_ok = checks["gpu_ollama"]["status"] == "healthy"
    status = "healthy" if gpu_ok else "degraded"
    report = f"# VM901 Watchdog — {now.strftime('%Y-%m-%d %H:%M PT')}\n\n**Status:** {status}\n\n"
    report += f"## GPU Ollama (VM901)\n- {'✅ Models: ' + ', '.join(checks['gpu_ollama']['models']) if gpu_ok else '❌ ' + checks['gpu_ollama'].get('error', '?')}\n"
    report += f"\n## CPU Ollama (CT110)\n- {'✅ Models: ' + ', '.join(checks['cpu_ollama']['models']) if checks['cpu_ollama']['status'] == 'healthy' else '❌ ' + checks['cpu_ollama'].get('error', '?')}\n"
    report += f"\n## Node Health\n"
    for name, info in targets.items():
        report += f"- {'✅' if info['up'] else '❌'} {name} (node_exporter)\n"
    return {"status": status, "report": report, "raw": checks}


def op_gitea_health(secrets):
    now = datetime.now(PT)
    targets = _prometheus_targets_by_role("gitea")
    data = _http_get(f"{GITEA_URL}/api/v1/version")
    api_ok = "error" not in data
    status = "healthy" if api_ok else "degraded"
    report = f"# Gitea Health — {now.strftime('%Y-%m-%d %H:%M PT')}\n\n**Status:** {status}\n"
    if api_ok:
        report += f"- Version: {data.get('version', '?')}\n"
    report += f"- API: {'✅' if api_ok else '❌'}\n"
    for name, info in targets.items():
        report += f"- node_exporter: {'✅' if info['up'] else '❌'}\n"
    return {"status": status, "report": report, "raw": {"api": data, "prometheus": targets}}


def op_system_snapshot(secrets):
    now = datetime.now(PT)
    all_targets = _prometheus_query("up")
    targets_list = []
    for t in all_targets:
        m = t.get("metric", {})
        targets_list.append({"name": m.get("name", m.get("instance", "?")), "role": m.get("role", m.get("job", "?")), "node": m.get("node", "?"), "up": t.get("value", [None, "0"])[1] == "1"})
    up_count = sum(1 for t in targets_list if t["up"])
    total = len(targets_list)
    alerts_data = _http_get(f"{PROMETHEUS_URL}/api/v1/alerts")
    firing = []
    if "error" not in alerts_data:
        firing = [a for a in alerts_data.get("data", {}).get("alerts", []) if a.get("state") == "firing"]
    report = f"# System Snapshot — {now.strftime('%Y-%m-%d %H:%M PT')}\n\n**Cluster:** {up_count}/{total} targets up\n\n"
    by_node = {}
    for t in targets_list:
        by_node.setdefault(t["node"], []).append(t)
    for node in sorted(by_node.keys()):
        report += f"## {node.upper()}\n"
        for t in sorted(by_node[node], key=lambda x: (x["up"], x["name"])):
            report += f"- {'✅' if t['up'] else '❌'} {t['name']} ({t['role']})\n"
        report += "\n"
    if firing:
        report += f"## Alerts ({len(firing)} firing)\n"
        for a in firing:
            report += f"- ⚠️ {a.get('labels', {}).get('alertname', '?')}\n"
    else:
        report += "**Alerts:** None firing\n"
    status = "healthy" if up_count == total else "degraded"
    return {"status": status, "report": report, "raw": {"targets": targets_list, "alerts": firing}}


def op_commute_tracker(secrets, direction="morning"):
    now = datetime.now(PT)
    api_key = secrets.get("TOMTOM_API_KEY", "")
    if not api_key:
        return {"status": "error", "report": "No TomTom API key", "raw": {}}
    if direction == "morning":
        origin, dest, label = os.environ.get("COMMUTE_ORIGIN_LATLON", ""), os.environ.get("COMMUTE_DEST_LATLON", ""), "home → work"
    else:
        origin, dest, label = os.environ.get("COMMUTE_DEST_LATLON", ""), os.environ.get("COMMUTE_ORIGIN_LATLON", ""), "work → home"
    data = _http_get(f"{TOMTOM_BASE}/{origin}:{dest}/json?key={api_key}&traffic=true&travelMode=car")
    if "error" in data:
        return {"status": "error", "report": f"TomTom error: {data['error']}", "raw": data}
    try:
        s = data["routes"][0]["summary"]
        travel_min, delay_min, dist_mi = round(s["travelTimeInSeconds"] / 60), round(s.get("trafficDelayInSeconds", 0) / 60), round(s["lengthInMeters"] / 1609.34, 1)
    except (KeyError, IndexError):
        return {"status": "error", "report": "Parse error", "raw": data}
    report = f"# Commute — {now.strftime('%Y-%m-%d %H:%M PT')}\n\n**{label}:** {travel_min} min ({dist_mi} mi)\n**Traffic delay:** {delay_min} min\n"
    return {"status": "tracked", "report": report, "raw": {"travel_min": travel_min, "delay_min": delay_min, "distance_mi": dist_mi, "direction": label}}


# ============================================================
# LLM-Powered Operations
# ============================================================

def op_daily_log(secrets):
    now = datetime.now(PT)
    date_str = now.strftime("%Y-%m-%d")
    token = secrets.get("GITEA_TOKEN", "")
    today_data = []
    for check_type in ["plex-health", "self-health", "vm901-watchdog", "gitea-health", "system-snapshot"]:
        try:
            url = f"{GITEA_URL}/api/v1/repos/{GITEA_REPO_OWNER}/{GITEA_REPO}/contents/checks/latest-{check_type}.md?ref={GITEA_BRANCH}"
            req = urllib.request.Request(url, headers={"Authorization": f"token {token}"})
            with urllib.request.urlopen(req, timeout=10) as r:
                content = base64.b64decode(json.loads(r.read())["content"]).decode()
                today_data.append(f"--- {check_type} ---\n{content[:500]}")
        except Exception:
            today_data.append(f"--- {check_type} ---\nNo data available")
    prompt = f"You are Lumina's operations logger. Write a concise daily log for {date_str}. Summarize health checks in 2-3 paragraphs. Note issues and trends. Markdown.\n\n{chr(10).join(today_data)}"
    summary = _format_with_llm(prompt, secrets, max_tokens=500)
    if not summary:
        summary = f"# Daily Log — {date_str}\n\nLLM unavailable. Raw data collected."
    return {"status": "logged", "report": summary, "raw": {"date": date_str, "checks_found": len(today_data)}}


def op_reflection(secrets):
    now = datetime.now(PT)
    date_str = now.strftime("%Y-%m-%d")
    token = secrets.get("GITEA_TOKEN", "")
    logs = []
    for days_ago in range(3):
        ds = (now - timedelta(days=days_ago)).strftime("%Y-%m-%d")
        try:
            url = f"{GITEA_URL}/api/v1/repos/{GITEA_REPO_OWNER}/{GITEA_MEMORY_REPO}/contents/logs/{ds}-daily.md?ref={GITEA_BRANCH}"
            req = urllib.request.Request(url, headers={"Authorization": f"token {token}"})
            with urllib.request.urlopen(req, timeout=10) as r:
                logs.append(f"--- {ds} ---\n{base64.b64decode(json.loads(r.read())['content']).decode()[:500]}")
        except Exception:
            pass
    if not logs:
        return {"status": "skipped", "report": "No recent daily logs to reflect on.", "raw": {}}
    prompt = f"You are Lumina's reflection engine. Review recent logs. Identify patterns, issues, improvements. 2-3 paragraphs. Markdown.\n\n{chr(10).join(logs)}"
    reflection = _format_with_llm(prompt, secrets, max_tokens=500)
    return {"status": "reflected", "report": reflection or f"# Reflection — {date_str}\n\nLLM unavailable.", "raw": {"date": date_str, "logs_reviewed": len(logs)}}


def op_tool_usage_log(secrets):
    date_str = datetime.now(PT).strftime("%Y-%m-%d")
    return {"status": "placeholder", "report": f"# Tool Usage — {date_str}\n\nPending IronClaw gateway API.\n", "raw": {"date": date_str}}


def op_memory_curation(secrets):
    now = datetime.now(PT)
    token = secrets.get("GITEA_TOKEN", "")
    try:
        url = f"{GITEA_URL}/api/v1/repos/{GITEA_REPO_OWNER}/{GITEA_MEMORY_REPO}/contents/?ref=main"
        req = urllib.request.Request(url, headers={"Authorization": f"token {token}"})
        with urllib.request.urlopen(req, timeout=10) as r:
            file_list = [f.get("name", "?") for f in json.loads(r.read())]
    except Exception as e:
        file_list = [f"Error: {e}"]
    report = f"# Memory Curation — {now.strftime('%Y-%m-%d')}\n\n**Files:** {len(file_list)}\n"
    for f in file_list:
        report += f"- {f}\n"
    return {"status": "curated", "report": report, "raw": {"files": file_list}}


# ============================================================
# Registry & Main
# ============================================================

def op_plane_gateway(secrets):
    """Check Plane gateway metrics from plane-helper.log (PG.6)."""
    now = datetime.now(PT)
    try:
        sys.path.insert(0, '/opt/lumina-fleet/sentinel')
        from plane_metrics import parse_log, sentinel_health, write_prom_file
        metrics = parse_log()
        health = sentinel_health(metrics)
        # Write Prometheus textfile for node_exporter
        write_prom_file(metrics)
        status = "healthy" if health["ok"] else "degraded"
        report = (
            f"# Plane Gateway — {now.strftime('%Y-%m-%d %H:%M PT')}\n\n"
            f"**Status:** {status}\n\n"
            f"| Metric | Value |\n|--------|-------|\n"
            f"| Requests (24h) | {metrics['total']} |\n"
            f"| Errors | {metrics['errors']} |\n"
            f"| Avg wait | {metrics['wait_avg_ms']}ms |\n"
            f"| Max wait | {metrics['wait_max_ms']}ms |\n"
        )
        if metrics.get("last_request_age_s") is not None:
            age_h = metrics["last_request_age_s"] // 3600
            age_m = (metrics["last_request_age_s"] % 3600) // 60
            report += f"| Last request | {int(age_h)}h {int(age_m)}m ago |\n"
        return {"status": status, "report": report, "raw": metrics}
    except Exception as e:
        return {
            "status": "unknown",
            "report": f"# Plane Gateway\n\nMetrics unavailable: {e}",
            "raw": {"error": str(e)},
        }


OPS = {
    "plex-health": {"fn": op_plex_health, "category": "checks", "needs_llm": False},
    "self-health": {"fn": op_self_health, "category": "checks", "needs_llm": False},
    "vm901-watchdog": {"fn": op_vm901_watchdog, "category": "checks", "needs_llm": False},
    "gitea-health": {"fn": op_gitea_health, "category": "checks", "needs_llm": False},
    "system-snapshot": {"fn": op_system_snapshot, "category": "checks", "needs_llm": False},
    "commute-tracker": {"fn": op_commute_tracker, "category": "checks", "needs_llm": False},
    "plane-gateway": {"fn": op_plane_gateway, "category": "checks", "needs_llm": False},
    "daily-log": {"fn": op_daily_log, "category": "logs", "needs_llm": True},
    "reflection": {"fn": op_reflection, "category": "logs", "needs_llm": True},
    "tool-usage-log": {"fn": op_tool_usage_log, "category": "logs", "needs_llm": False},
    "memory-curation": {"fn": op_memory_curation, "category": "logs", "needs_llm": False},
}


def run_op(op_name, extra_args=None):
    now = datetime.now(PT)
    date_str, time_str = now.strftime("%Y-%m-%d"), now.strftime("%H%M")
    if op_name not in OPS:
        print(f"Unknown operation: {op_name}\nAvailable: {', '.join(OPS.keys())}")
        sys.exit(1)
    op = OPS[op_name]
    print(f"[agent-ops] Running {op_name}...")
    secrets = load_secrets()
    result = op["fn"](secrets, extra_args[0]) if op_name == "commute-tracker" and extra_args else op["fn"](secrets)
    status, report, raw = result.get("status", "unknown"), result.get("report", ""), result.get("raw", {})
    category = op["category"]
    target_repo = GITEA_MEMORY_REPO if category == "logs" else GITEA_REPO
    write_to_gitea(report, f"{category}/{date_str}-{time_str}-{op_name}.md", secrets, repo=target_repo, message=f"{op_name} {date_str} {time_str}")
    write_to_gitea(report, f"{category}/latest-{op_name}.md", secrets, repo=target_repo, message=f"Latest {op_name}")
    write_to_gitea(json.dumps(raw, indent=2), f"{category}/{date_str}-{time_str}-{op_name}-raw.json", secrets, repo=target_repo, message=f"{op_name} raw {date_str}")
    print(f"[agent-ops] {op_name} complete — status: {status}")


if __name__ == "__main__":
    if len(sys.argv) < 2:
        print(f"Usage: python3 ops.py <operation> [args...]\nOperations: {', '.join(OPS.keys())}")
        sys.exit(1)
    run_op(sys.argv[1], sys.argv[2:] if len(sys.argv) > 2 else None)
