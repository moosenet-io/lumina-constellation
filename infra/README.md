# ✦ Infrastructure

> The foundation of the constellation.

**Infrastructure** contains the configuration management and automation playbooks for distributed deployments.

## What it does

- Manages server-level configuration using Ansible playbooks.
- Automates the setup of remote inference nodes and secondary hosts.
- Defines consistent environment states across different deployment targets.
- Handles system-level dependencies (drivers, libraries, networking).
- Coordinates cross-node security and SSH access patterns.

## Key files

| File | Purpose |
|------|---------|
| `ansible/` | Roles and playbooks for host configuration |
| `ansible/group_vars/` | Environment-specific configuration variables |

## Talks to

- **[Deployment](../deploy/)** — Provides the base environment for Docker-led deployments.
- **[Dura](../fleet/dura/)** — Integrates with backup and secret rotation workflows.
- **[Sentinel](../fleet/sentinel/)** — Feeds metrics into the monitoring stack.

## Configuration

Managed via Ansible inventory and group variables. Requires SSH access to target nodes.

---

Part of [Lumina Constellation](../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
