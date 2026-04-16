#!/usr/bin/env python3
"""
prometheus_exporter.py — Prometheus /metrics endpoint for Lumina Constellation.

Pure Python, no prometheus_client dependency — just formats the text exposition format.
Runs as sentinel_metrics.service on fleet-host, port 9100.

Metrics exposed:
  lumina_service_up{service,container}
  lumina_inference_cost_daily_usd
  lumina_nexus_unacked_count
  lumina_axon_last_poll_seconds
  lumina_engram_fact_count
  lumina_litellm_response_ms
  lumina_check_last_run_timestamp
"""
import json
import os
import time
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
from datetime import datetime, timezone

HEALTH_JSON = Path('/opt/lumina-fleet/sentinel/output/health.json')
METRICS_PORT = int(os.environ.get('SENTINEL_METRICS_PORT', '9100'))


def _load_health() -> dict:
    try:
        if HEALTH_JSON.exists():
            return json.loads(HEALTH_JSON.read_text())
    except Exception:
        pass
    return {}


def _metric_line(name: str, labels: dict, value, help_text: str = '', metric_type: str = 'gauge') -> list[str]:
    """Format a single Prometheus metric."""
    lines = []
    if help_text:
        lines.append(f'# HELP {name} {help_text}')
    lines.append(f'# TYPE {name} {metric_type}')
    label_str = ','.join(f'{k}="{v}"' for k, v in labels.items())
    lines.append(f'{name}{{{label_str}}} {value}')
    return lines


def generate_metrics() -> str:
    """Generate Prometheus text format metrics from latest health check results."""
    health = _load_health()
    checks = health.get('checks', {})
    lines = []

    # Service up/down gauge
    SERVICE_MAP = {
        'ironclaw': ('ironclaw', 'ironclaw-host'),
        'terminus': ('terminus', 'terminus-host'),
        'litellm': ('litellm', 'litellm-host'),
        'postgres': ('postgres', 'postgres-host'),
        'matrix': ('matrix', 'matrix-host'),
        'docker': ('docker_fleet_host', 'fleet-host'),
        'plane': ('plane', 'plane-host'),
        'ollama_gpu': ('ollama_gpu', 'local GPU host'),
        'ollama_cpu': ('ollama_cpu', 'ollama-cpu-host'),
        'soma': ('soma', 'fleet-host'),
        'gitea': ('gitea', 'gitea-host'),
    }

    lines.append('# HELP lumina_service_up Whether a service is reachable (1=up, 0=down)')
    lines.append('# TYPE lumina_service_up gauge')
    for check_key, (service, container) in SERVICE_MAP.items():
        check = checks.get(check_key, {})
        status = check.get('status', 'unknown')
        up = 1 if status == 'ok' else 0
        lines.append(f'lumina_service_up{{service="{service}",container="{container}"}} {up}')

    # LLM cost
    cost_check = checks.get('llm_cost', {})
    cost_val = float(cost_check.get('value', 0) or 0)
    lines.extend(_metric_line('lumina_inference_cost_daily_usd', {},
                              f'{cost_val:.4f}', 'Today inference spend in USD'))

    # Nexus unacked
    nexus_check = checks.get('nexus_age', {})
    nexus_count = int(nexus_check.get('value', 0) or 0)
    lines.extend(_metric_line('lumina_nexus_unacked_count', {},
                              nexus_count, 'Unacknowledged Nexus inbox messages'))

    # Axon DB status (1=ok, 0=down, -1=unknown)
    axon_check = checks.get('axon_db', {})
    axon_status = 1 if axon_check.get('status') == 'ok' else (0 if axon_check.get('status') == 'critical' else -1)
    lines.extend(_metric_line('lumina_axon_db_status', {},
                              axon_status, 'Axon DB connection status (1=ok, 0=critical, -1=unknown)'))

    # Engram facts
    engram_check = checks.get('engram', {})
    fact_count = int(engram_check.get('value', 0) or 0)
    lines.extend(_metric_line('lumina_engram_fact_count', {},
                              fact_count, 'Total facts in Engram knowledge base'))

    # LiteLLM latency
    litellm_check = checks.get('litellm', {})
    litellm_ms = int(litellm_check.get('value', 0) or 0)
    lines.extend(_metric_line('lumina_litellm_response_ms', {},
                              litellm_ms, 'LiteLLM /health response latency in ms'))

    # local GPU status
    gpu_check = checks.get('ollama_gpu', {})
    gpu_up = 1 if gpu_check.get('status') == 'ok' else 0
    lines.extend(_metric_line('lumina_ollama_gpu_up', {},
                              gpu_up, 'local Ollama GPU availability (1=up, 0=down)'))

    # Overall status
    overall_map = {'ok': 0, 'warn': 1, 'critical': 2}
    overall = overall_map.get(health.get('overall', 'unknown'), -1)
    lines.extend(_metric_line('lumina_overall_health', {},
                              overall, 'Overall system health (0=ok, 1=warn, 2=critical, -1=unknown)'))

    # Check counts
    lines.extend(_metric_line('lumina_checks_total', {},
                              health.get('total', 0), 'Total checks run'))
    lines.extend(_metric_line('lumina_checks_critical', {},
                              health.get('critical', 0), 'Checks in critical state'))
    lines.extend(_metric_line('lumina_checks_warn', {},
                              health.get('warn', 0), 'Checks in warn state'))

    # Last check timestamp
    ts = health.get('timestamp', '')
    if ts:
        try:
            dt = datetime.fromisoformat(ts.replace('Z', '+00:00'))
            unix_ts = int(dt.timestamp())
            lines.extend(_metric_line('lumina_last_check_timestamp', {},
                                      unix_ts, 'Unix timestamp of last health check run'))
        except Exception:
            pass

    return '\n'.join(lines) + '\n'


class MetricsHandler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == '/metrics':
            metrics = generate_metrics()
            body = metrics.encode('utf-8')
            self.send_response(200)
            self.send_header('Content-Type', 'text/plain; version=0.0.4; charset=utf-8')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        elif self.path == '/health':
            body = b'{"status": "ok", "service": "sentinel-metrics"}'
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        else:
            self.send_response(404)
            self.end_headers()

    def log_message(self, format, *args):
        pass  # Suppress HTTP access logs


def main():
    print(f'[sentinel-metrics] Starting Prometheus exporter on :{METRICS_PORT}')
    print(f'[sentinel-metrics] Metrics from: {HEALTH_JSON}')
    server = HTTPServer(('0.0.0.0', METRICS_PORT), MetricsHandler)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print('[sentinel-metrics] Stopped')


if __name__ == '__main__':
    main()
