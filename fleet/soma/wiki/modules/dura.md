# Dura — Resilience and Operations

Dura handles backup, smoke testing, log aggregation, secret rotation, and data export for Lumina Constellation. It runs on a schedule and is the last line of defense against data loss or configuration drift.

**Deploys to:** <fleet-host> at `/opt/lumina-fleet/dura/`
**Trigger:** systemd timer — daily backup, weekly smoke test, hourly log aggregation
**Inference cost:** $0 (pure Python — all operations are deterministic)

## What Dura Does

### Backup
- Snapshots Engram memory database (`memory.db`)
- Exports Nexus inbox messages (JSON dump)
- Archives constellation.yaml and all agent configs
- Ships to configured backup destination (local path or S3-compatible)

### Smoke Tests
Runs a weekly end-to-end test of the full system:
1. Send a test message through Matrix
2. Verify Lumina responds
3. Verify Nexus delivers a work order to Axon
4. Verify Axon completes and responds
5. Verify all health checks pass

Reports pass/fail to Lumina via Nexus.

### Log Aggregation
Pulls the last 1000 lines of each agent's systemd journal and writes to:
`/opt/lumina-fleet/logs/{agent}/{date}.log`

Useful for debugging. Logs are rotated weekly.

### Secret Rotation
Triggers `fetch-mcp-secrets.sh` on <terminus-host> to refresh Infisical secrets without restarting the MCP server. Runs weekly at 03:00.

## Files

| File | Purpose |
|------|---------|
| `dura.py` | Main agent. Backup orchestrator, smoke test runner, log aggregator. |
| `dura.service` | systemd service. |
| `dura-backup.timer` | systemd timer for daily backups. |
| `dura-smoke.timer` | systemd timer for weekly smoke tests. |

## Backup Configuration

```yaml
# constellation.yaml — dura section
dura:
  backup_dest: /mnt/backup/lumina
  backup_s3_bucket: ""  # Leave empty for local-only
  backup_retention_days: 30
  smoke_test_schedule: "0 3 * * 0"  # Sunday 03:00
```

## MCP Tools (in Terminus)

| Tool | Description |
|------|-------------|
| `dura_backup()` | Trigger a manual backup |
| `dura_smoke_test()` | Run end-to-end smoke test |
| `dura_log_summary(agent, hours)` | Get recent log summary for an agent |

## Related

- [Architecture Overview](../architecture/constellation-overview.md)
- [Sentinel](sentinel.md) — complementary: Sentinel monitors health, Dura restores it
