# ✦ Dura

> "Because 'it worked yesterday' isn't a backup strategy."

**Dura** is Lumina's operational resilience module — secret rotation with full audit trails, backup orchestration across local and NFS storage, smoke testing of every critical MCP tool, and a disaster recovery runbook.

## What it does

- **Secret rotation**: checks all 9 managed secrets against their `max_age_days`, auto-rotates where possible (random hex, Gitea API), sends Matrix alerts for manual rotation
- **Backup orchestration**: hourly SQLite backups, daily full backups to NFS; verifies backup integrity
- **Smoke tests**: calls every critical MCP tool with test inputs and verifies response schema
- **Audit trail**: rotation events, backup status, and smoke test results stored in `dura.db`
- **Prometheus metrics**: secret age and backup health exposed for Sentinel monitoring

## Key files

| File | Purpose |
|------|---------|
| `dura_backup.py` | Backup orchestration — SQLite, Postgres, configuration files |
| `dura_secret_rotation.py` | Secret age checking with Sentinel-compatible exit codes |
| `dura_smoke_test.py` | MCP tool smoke tests with test_fixtures.yaml |
| `test_fixtures.yaml` | Test inputs and expected response schemas per tool |

## Talks to

- **Terminus** (`dura_tools.py`) — MCP tools expose backup status and smoke test results
- **Nexus** — sends rotation alerts to Lumina for manual actions
- **Sentinel** — Prometheus metrics for secret age and backup freshness
- **Infisical** — reads and updates secret values during rotation

## Configuration

```bash
FLEET_DIR=/opt/lumina-fleet
GITEA_URL=http://your-gitea-host:3000
GITEA_TOKEN=...
INFISICAL_URL=http://your-infisical-host:8080
```

Rotation registry: `fleet/security/secrets_registry.yaml`. State file: `security/rotation_state.json`.

---

Part of [Lumina Constellation](../../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
