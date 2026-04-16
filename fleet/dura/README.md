# ✦ Dura

> Because 'it worked yesterday' isn't a backup strategy.

**Dura** is the resilience and backup module that ensures the constellation's data is safe and secrets are rotated.

## What it does

- Automates encrypted backups of all constellation databases (Postgres, SQLite).
- Manages automated rotation of API keys and internal credentials.
- Performs "smoke tests" on backup integrity to ensure restorability.
- Coordinates cross-node synchronization for distributed deployments.
- Maintains the constellation's virtual key-keyring.

## Key files

| File | Purpose |
|------|---------|
| `dura_backup.py` | Orchestrates database and filesystem dumps |
| `dura_secret_rotation.py` | Rotates keys for Gitea, Plane, and cloud APIs |
| `dura_smoke_test.py` | Validates that recent backups are valid |
| `dura-secret-rotation.timer` | Systemd timer for scheduled maintenance |

## Talks to

- **[Plexus (Plane)](../nexus/)** — Backs up the project management database.
- **[Engram](../engram/)** — Ensures the shared brain is backed up.
- **[Sentinel](../sentinel/)** — Reports backup success/failure status.

## Configuration

Backup targets and schedules defined in `test_fixtures.yaml` (or production equivalent).

---

Part of [Lumina Constellation](../../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
