---
name: health-check
description: Check infrastructure health across all MooseNet containers and services
version: 1.0
author: Peter Boose
license: MIT
agent: sentinel
container: CT310
schedule: "*/5 * * * *"
tags: [monitoring, health, infrastructure, alerts]
---

# Infrastructure Health Check

Check the health of all MooseNet containers and services. Alert only on failure — silence means healthy.

## Procedure

1. Check CT services: IronClaw (CT305), Terminus MCP (CT214), Matrix bridge (CT306), Postgres (CT300)
2. Check Docker services on CT310: Caddy, Actual Budget, Grocy, LubeLogger
3. Check Prometheus targets (up metric) via prometheus_query
4. Compute health score: (healthy/total) × 100
5. If any service down: send alert to Nexus (priority: urgent) and Matrix
6. Write HTML status page to /opt/lumina-fleet/sentinel/output/

## Inference de-bloat

All checks are Python HTTP calls. No LLM involved.
Alert message uses a template, not inference.

## Thresholds

- CPU > 90% for 5 min: warn
- Memory > 95%: warn
- Service down: alert immediately
- Disk > 90%: warn
