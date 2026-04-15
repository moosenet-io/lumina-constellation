# ✦ Spectra

> "Lumina's eyes on the web."

**Spectra** is Lumina's browser automation module — a sandboxed Playwright instance that can navigate, screenshot, extract content, record sessions, and hand off to a human when it encounters something it can't handle alone.

## What it does

- **Headless browsing** with full Playwright capabilities (navigate, click, type, fill forms, execute JS)
- **Content extraction**: 10-stage sanitization pipeline strips prompt injection, hidden elements, scripts, and zero-width chars before any content reaches an LLM
- **Session recording**: rrweb captures DOM mutations for replay in Soma's `/spectra/recordings`
- **Visual diff**: Pillow + imagehash compares screenshots before/after changes — used in the Soma feedback loop
- **Human-in-the-loop**: detects login forms and CAPTCHAs, notifies Peter via Matrix, waits for reply
- **5-layer security**: container sandbox, iptables network isolation, Chromium sandbox, content sanitization, access control with per-consumer rate limits and daily budgets

## Key files

| File | Purpose |
|------|---------|
| `spectra_service.py` | FastAPI service (port 8084) — 21 REST endpoints, session management, audit logging |
| `spectra_worker.py` | Playwright async worker — browser contexts, navigation, screenshots, rrweb injection |
| `spectra_sanitizer.py` | 10-stage content sanitization pipeline |
| `spectra_config.yaml` | Consumer key table (MY.1–MY.9), rate limits, budgets, URL allowlists |
| `docker-compose.yml` | Three containers: `spectra` (internet), `spectra-internal` (LAN-only), `spectra-novnc` (Live View) |
| `entrypoint.sh` | iptables setup, TLS cert generation, Xvfb/x11vnc start, gosu user switch |

## Talks to

- **Terminus** (`spectra_tools.py`) — 21 MCP tools expose all browser capabilities to IronClaw
- **Engram** (`spectra_store.py`) — stores extracted content with `source='spectra'` namespace
- **Soma** — Live View iframe (`/spectra`), recordings (`/spectra/recordings`), config (`/spectra/config`)
- **Vigil** — internal screenshots of Soma and Prometheus dashboards
- **Sentinel** — 4 health checks, Prometheus metrics
- **Synapse** — HITL notifications routed through Synapse to Matrix

## Configuration

```bash
SPECTRA_URL=http://your-fleet-host:8084
SPECTRA_INTERNAL_URL=http://your-fleet-host:8085
LAN_RANGES=10.0.0.0/8 172.16.0.0/12 YOUR_LAN/16 169.254.0.0/16  # blocked by iptables
COREDNS_IP=your-dns-server   # optional: restrict DNS to internal resolver
```

Consumer keys (MY.1–MY.9) are LiteLLM virtual keys defined in `spectra_config.yaml`. Spectra `spectra_enabled` metadata controls browser access per consumer.

---

Part of [Lumina Constellation](../../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
