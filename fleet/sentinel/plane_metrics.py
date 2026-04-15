#!/usr/bin/env python3
"""
plane_metrics.py — Prometheus metrics exporter for Plane gateway (PG.6)
fleet/sentinel/plane_metrics.py

Reads /tmp/plane-helper.log and exposes metrics for Prometheus node_exporter
textfile collector, or returns structured data for Sentinel health checks.

Log format (one line per request):
  2026-04-15T10:00:00 GET /workspaces/moosenet/projects/ 200 wait=150ms

Metrics exposed:
  lumina_plane_requests_total        — total requests in log window
  lumina_plane_requests_errors_total — 4xx/5xx count
  lumina_plane_wait_avg_ms           — average wait time (ms)
  lumina_plane_wait_max_ms           — max wait time (ms)
  lumina_plane_last_request_age_s    — seconds since last request

Usage:
  python3 plane_metrics.py                   — Print Prometheus text format
  python3 plane_metrics.py json              — Print JSON dict
  python3 plane_metrics.py sentinel          — Print sentinel health result

Prometheus textfile mode:
  Install as cron: */2 * * * * python3 /opt/lumina-fleet/sentinel/plane_metrics.py > /var/lib/node_exporter/textfile_collector/plane_gateway.prom
"""

import json
import os
import re
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

LOG_FILE = Path(os.environ.get("PLANE_HELPER_LOG", "/tmp/plane-helper.log"))
PROM_OUTPUT = Path(os.environ.get("PLANE_PROM_OUTPUT", "/var/lib/node_exporter/textfile_collector/plane_gateway.prom"))

# Parse log lines from the last N seconds (default: last 24 hours)
WINDOW_SECONDS = int(os.environ.get("PLANE_METRICS_WINDOW", str(24 * 3600)))

# Pattern: 2026-04-15T10:00:00 GET /path 200 wait=150ms
_LINE_RE = re.compile(
    r'^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})\s+(\w+)\s+(\S+)\s+(\d+)\s+wait=(\d+)ms'
)


def parse_log(window_seconds: int = WINDOW_SECONDS) -> dict:
    """
    Parse plane-helper.log and return metrics dict.
    Only includes lines within the last window_seconds.
    """
    if not LOG_FILE.exists():
        return {
            "total": 0, "errors": 0, "wait_avg_ms": 0, "wait_max_ms": 0,
            "last_request_age_s": None, "log_exists": False,
        }

    now = time.time()
    cutoff = now - window_seconds

    total = 0
    errors = 0
    wait_times = []
    last_ts = None

    try:
        lines = LOG_FILE.read_text().splitlines()
    except Exception:
        return {
            "total": 0, "errors": 0, "wait_avg_ms": 0, "wait_max_ms": 0,
            "last_request_age_s": None, "log_exists": True,
        }

    for line in lines:
        m = _LINE_RE.match(line.strip())
        if not m:
            continue

        ts_str, method, path, status_str, wait_str = m.groups()
        try:
            ts = datetime.strptime(ts_str, "%Y-%m-%dT%H:%M:%S").replace(tzinfo=timezone.utc).timestamp()
        except ValueError:
            continue

        if ts < cutoff:
            continue

        status = int(status_str)
        wait_ms = int(wait_str)

        total += 1
        if status >= 400:
            errors += 1
        wait_times.append(wait_ms)
        if last_ts is None or ts > last_ts:
            last_ts = ts

    wait_avg = round(sum(wait_times) / len(wait_times)) if wait_times else 0
    wait_max = max(wait_times) if wait_times else 0
    last_age = round(now - last_ts) if last_ts else None

    return {
        "total": total,
        "errors": errors,
        "wait_avg_ms": wait_avg,
        "wait_max_ms": wait_max,
        "last_request_age_s": last_age,
        "log_exists": True,
        "window_seconds": window_seconds,
    }


def to_prometheus(metrics: dict) -> str:
    """Format metrics as Prometheus text format."""
    lines = [
        "# HELP lumina_plane_requests_total Total Plane API requests in window",
        "# TYPE lumina_plane_requests_total gauge",
        f'lumina_plane_requests_total {metrics["total"]}',
        "",
        "# HELP lumina_plane_requests_errors_total Plane API 4xx/5xx responses in window",
        "# TYPE lumina_plane_requests_errors_total gauge",
        f'lumina_plane_requests_errors_total {metrics["errors"]}',
        "",
        "# HELP lumina_plane_wait_avg_ms Average rate-limiter wait time (ms)",
        "# TYPE lumina_plane_wait_avg_ms gauge",
        f'lumina_plane_wait_avg_ms {metrics["wait_avg_ms"]}',
        "",
        "# HELP lumina_plane_wait_max_ms Max rate-limiter wait time (ms) in window",
        "# TYPE lumina_plane_wait_max_ms gauge",
        f'lumina_plane_wait_max_ms {metrics["wait_max_ms"]}',
        "",
    ]
    if metrics.get("last_request_age_s") is not None:
        lines += [
            "# HELP lumina_plane_last_request_age_s Seconds since last Plane API request",
            "# TYPE lumina_plane_last_request_age_s gauge",
            f'lumina_plane_last_request_age_s {metrics["last_request_age_s"]}',
            "",
        ]
    return "\n".join(lines)


def sentinel_health(metrics: dict) -> dict:
    """
    Return Sentinel-compatible health check result.
    Called by Sentinel health_checks integration.
    """
    if not metrics["log_exists"]:
        return {
            "ok": True,
            "status": "ok",
            "value": "no activity",
            "message": "plane-helper.log not found — no Plane calls made yet",
        }

    total = metrics["total"]
    errors = metrics["errors"]
    error_rate = errors / total if total > 0 else 0.0
    avg_wait = metrics["wait_avg_ms"]

    if error_rate > 0.20:
        return {
            "ok": False,
            "status": "critical",
            "value": f"{errors}/{total} errors",
            "message": f"Plane API error rate {error_rate:.0%} — {errors} errors in last 24h",
        }

    if avg_wait > 5000:
        return {
            "ok": True,
            "status": "warn",
            "value": f"avg wait {avg_wait}ms",
            "message": f"Plane API rate limiter backing off — avg wait {avg_wait}ms (normal: <3000ms)",
        }

    return {
        "ok": True,
        "status": "ok",
        "value": f"{total} reqs, {avg_wait}ms avg wait",
        "message": f"Plane gateway healthy — {total} requests, {errors} errors, {avg_wait}ms avg wait",
    }


def write_prom_file(metrics: dict):
    """Write Prometheus textfile for node_exporter scraping."""
    try:
        PROM_OUTPUT.parent.mkdir(parents=True, exist_ok=True)
        PROM_OUTPUT.write_text(to_prometheus(metrics))
    except Exception as e:
        print(f"[plane_metrics] Could not write {PROM_OUTPUT}: {e}", file=sys.stderr)


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else "prometheus"
    metrics = parse_log()

    if cmd == "json":
        print(json.dumps(metrics, indent=2))
    elif cmd == "sentinel":
        print(json.dumps(sentinel_health(metrics), indent=2))
    elif cmd == "write":
        # Write textfile for node_exporter and print path
        write_prom_file(metrics)
        print(f"Written to {PROM_OUTPUT}")
    else:
        # Default: print Prometheus text format to stdout
        print(to_prometheus(metrics), end="")
