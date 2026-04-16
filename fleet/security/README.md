# ✦ Security

> "Keys rotated, patterns scrubbed, secrets never committed."

The **security** module manages the secret lifecycle and PII protection layer for Lumina Constellation — rotation, audit trails, key generation, and content filtering.

## What it does

- **Secret rotation** (9 secrets): auto-rotates random hex keys, Gitea tokens, and LiteLLM keys; sends Matrix alerts for manual-only secrets
- **Rotation audit**: tracks age, last rotation date, and status (ok/warn/expired) per secret
- **PII gate**: regex-based content filter that strips personal data before LLM ingestion
- **Myelin key generation**: `generate_litellm_keys.py` creates all 9 consumer virtual keys (MY.1-9) with budgets and metadata
- **Daily timer**: `secret-rotation-check.timer` fires at 08:17 — auto-rotates expired secrets, sends warnings on approaching ones

## Key files

| File | Purpose |
|------|---------|
| `rotation.py` | Secret rotation engine — auto-rotate, rollback on health fail, Prometheus export |
| `secrets_registry.yaml` | 9 secrets with rotation method, max_age_days, restart_commands |
| `generate_litellm_keys.py` | Creates MY.1-9 LiteLLM virtual keys with budgets and Spectra metadata |
| `pii-patterns.yaml` | Regex patterns for PII detection (email, phone, SSN, address) |
| `lumina-secrets` | CLI: `lumina-secrets check|list|rotate|run` |

## Talks to

- **LiteLLM** — generates and manages virtual keys via `/key/generate`
- **Infisical** — reads current secret values, writes rotated values
- **Sentinel** — `sentinel_check()` returns health-check-compatible result
- **Nexus** — sends rotation alerts for manual-rotation secrets

## Configuration

```bash
INFISICAL_URL=http://your-infisical-host:8080
INFISICAL_CLIENT_ID=...
INFISICAL_CLIENT_SECRET=your-client-secret
GITEA_URL=http://your-gitea-host:3000
LITELLM_URL=http://your-litellm-host:4000
LITELLM_MASTER_KEY=your-master-key
```

Secret values are stored in Infisical — never committed to Git.

---

Part of [Lumina Constellation](../../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
