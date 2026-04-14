# Lumina Constellation

A 19-module AI operating system for personal homelab operators. Runs on Proxmox, coordinates via Matrix, uses Claude and local Qwen for inference.

## Quick start

```bash
git clone https://github.com/moosenet-io/lumina-deploy.git
cd lumina-deploy && cp .env.example .env
docker compose up -d
```

Open http://localhost:3000 → complete the Soma onboarding wizard.

## What's included

| Module | Role | Cost |
|--------|------|------|
| Nexus | Inter-agent inbox (PostgreSQL) | $0 |
| Vigil | Morning/afternoon briefings | $0 (Qwen) |
| Sentinel | Infrastructure health checks | $0 (Python) |
| Axon | Work queue manager | $0 (Qwen) |
| Seer | Web research | $0 (Qwen) |
| Vector | Autonomous dev loops | ~$0.20/loop |
| Cortex | Code intelligence | $0 (tree-sitter) |
| Engram | Semantic memory | $0 (sqlite-vec) |
| Myelin | Cost tracking | $0 (Python) |
| Dura | Backup + smoke tests | $0 (Python) |
| Soma | Admin panel + onboarding | $0 (FastAPI) |

**Daily cost: ~$0.08/day** for routine operations. Cloud inference reserved for reasoning tasks.

## Navigation

- [Getting started](getting-started/installation.md)
- [Module guides](modules/)
- [How-to guides](guides/)
- [Reference](reference/)
- [Troubleshooting](troubleshooting/common-issues.md)
