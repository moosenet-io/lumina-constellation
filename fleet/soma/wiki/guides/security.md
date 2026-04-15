# Security Guide

## Overview

Lumina Constellation uses a layered security model across three areas:

1. **Secret Rotation** — managed credentials with age tracking and auto-rotation
2. **PII Gate** — three-layer filter to prevent personal data from reaching external LLMs
3. **Dev/Prod Isolation** — Gitea (private) vs GitHub (public) for code, pre-commit hooks for enforcement

---

## Secret Rotation System

### How it works

All managed secrets are defined in `fleet/security/secrets_registry.yaml`. The rotation engine (`rotation.py`) checks each secret's age against its `max_age_days` and either auto-rotates it or sends a Matrix alert for manual rotation.

**Status thresholds:**
- `ok` — age < 80% of max_age_days
- `warn` — age ≥ 80% of max_age_days
- `expired` — age ≥ max_age_days

**Rotation methods:**
| Method | What it does |
|--------|-------------|
| `random_hex_32` | Generates `secrets.token_hex(32)`, updates Infisical, restarts services |
| `gitea_api` | Deletes old Gitea token, creates new one, updates Infisical, restarts terminus-mcp |
| `manual` | Sends a Nexus alert to Lumina with step-by-step instructions |

### Daily timer

`secret-rotation-check.timer` runs at 08:17 AM daily on the fleet host. It calls `rotation.py run` which rotates expired secrets and sends warnings for those approaching expiry.

Check timer status: `systemctl status secret-rotation-check.timer`

### Soma Security tab

The Soma admin panel at `/security` shows all secrets with their age, status badge (green/amber/red), last rotation date, and action buttons for auto-rotatable secrets.

### CLI

```bash
lumina-secrets list              # Show all secrets with status table
lumina-secrets check             # Exit 0=ok, 1=warn, 2=critical (for monitoring)
lumina-secrets rotate SECRET_NAME  # Force-rotate a specific secret
lumina-secrets run               # Rotate all expired secrets
lumina-secrets run --dry-run     # Preview without making changes
```

### Adding a new secret to the registry

1. Add an entry to `fleet/security/secrets_registry.yaml`:
```yaml
- name: MY_NEW_SECRET
  description: What this secret does and which service uses it
  method: random_hex_32   # or gitea_api, manual
  max_age_days: 90
  services: [soma, axon]
  infisical_project: services
  restart_commands:
    - "systemctl restart soma"
```

2. Add the secret to Infisical at `YOUR_INFISICAL_HOST:8080` (workspace: moosenet-services, env: prod).

3. Add a stub line to the relevant `.env` file so `fetch-mcp-secrets.sh` knows to pull it:
```
MY_NEW_SECRET=PLACEHOLDER
```

4. Deploy: `push fleet/security/secrets_registry.yaml /opt/lumina-fleet/security/secrets_registry.yaml`

5. Run initial check: `lumina-secrets check`

---

## PII Gate

The PII gate prevents personal information from reaching external LLM providers (OpenRouter, cloud models). It operates at three layers:

### Layer 1 — Pre-send filter (Refractor / LiteLLM)
Before any request leaves the LiteLLM proxy, the payload is scanned for PII patterns. Matches are replaced with `[REDACTED]` or the request is blocked entirely depending on the configured action.

### Layer 2 — Engram namespace isolation
Memory stored in Engram uses namespaced keys (`agents/vigil/...`, `personal/...`). Namespaces tagged `pii` or `personal` are never included in LLM context windows by the engram retrieval functions.

### Layer 3 — Persona boundaries
Agent prompts (in `.agent.yaml` persona fields and IronClaw LUMINA.md) are written to avoid requesting or repeating personal details. The operator's name and role are referenced symbolically.

### Configuring PII patterns
PII patterns are regex-based and live in the LiteLLM config on the LiteLLM proxy host:
```
/srv/litellm/config.yaml → pii_masking: patterns: [...]
```

Common patterns already configured: email addresses, phone numbers, home addresses, SSNs.

---

## Dev/Prod Workflow

### Gitea (private, internal)
- URL: `YOUR_GITEA_HOST:3000` (git.moosenet.online)
- All work-in-progress and infrastructure code lives here
- Contains secrets in `.env` files (never committed — `.gitignore` enforced)
- Primary remote: `gitea` alias in all repos

### GitHub (public mirror)
- URL: `github.com/moosenet-io/lumina-constellation`
- Mirrors Gitea after scrubbing via `deploy/publish-to-github.sh`
- Script strips: `.env` files, internal IPs, tokens, any file matching `secrets_*`
- Run manually: `bash ~/lumina-constellation/deploy/publish-to-github.sh`

### Pre-commit hook

The pre-commit hook in `lumina-constellation` blocks commits that contain:
- IP addresses matching `192.168.0.\d+` (internal network)
- Token patterns: `token_`, `api_key=`, `password=` in non-`.env` files
- File patterns: `.env`, `*.pem`, `*_private_key*`

To bypass (legitimate use — internal IPs in config files): `git commit --no-verify`

**When to use `--no-verify`**: Only when committing infrastructure config files that legitimately contain internal IPs or service addresses. Never use it to bypass a hook catching actual secrets.

---

## Infisical Secret Management

All production secrets are stored in Infisical at `YOUR_INFISICAL_HOST:8080`.

- **Workspace:** `moosenet-services`
- **Environment:** `prod`
- **Auth:** Machine Identity with Universal Auth (credentials in `/opt/briefing-agent/.infisical-auth`)

### Fetching secrets to a container

Terminus host: `/opt/ai-mcp/fetch-mcp-secrets.sh`

For other containers, the pattern is:
```bash
infisical export --env prod --path / > /opt/lumina-fleet/axon/.env
```

### Rotation state file

Rotation state is written to `/opt/lumina-fleet/security/rotation_state.json`. This tracks last rotation dates per secret. If this file is lost, all secrets will appear as "unknown" status and the next `rotation.py run` will re-rotate auto-rotatable ones.

---

## Incident Response

If a secret is suspected compromised:

1. **Immediate**: Run `lumina-secrets rotate SECRET_NAME` for auto-rotatable secrets
2. **Manual secrets**: Follow instructions at `lumina-secrets list` then look at the Instructions button in Soma → Security
3. **After rotation**: Check service health in Soma → Status
4. **Audit**: Check Engram activity journal for recent unusual tool calls: `sqlite3 /opt/lumina-fleet/engram/engram.db "SELECT * FROM activity_journal ORDER BY created_at DESC LIMIT 20"`
