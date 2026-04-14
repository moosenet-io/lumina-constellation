# OpenClaw Migration Report

**Date:** 2026-04-13 05:32
**Source:** `deploy/test-fixtures/openclaw`
**Mode:** DRY RUN (no changes made)

## Migrated

| Category | Item | Notes |
|----------|------|-------|
| personality | SOUL.md | → /home/coder/.ironclaw/LUMINA.md (dry-run) |
| user-profile | USER.md | → /home/coder/.ironclaw/USER.md (dry-run) |
| memory | 2 facts | would import to Engram (dry-run) |
| skills | morning-routine | → /opt/lumina-fleet/skills/active/morning-routine (dry-run) |
| credentials | 3 API keys | printed to console — add to Infisical manually |

## Skipped

_Nothing skipped._

## Warnings

- Found 3 API keys in OpenClaw .env: ANTHROPIC_API_KEY, OPENAI_API_KEY, SOME_SERVICE_TOKEN. These are printed to console only — NOT written to any file. Add them to Infisical manually.

## Next Steps

1. Review this report and verify migrated files
2. If memory facts were extracted, run the Engram batch import
3. Add API keys to Infisical (see credential section above)
4. Run `deploy/ironclaw-setup.sh` to seed IronClaw vault from env vars
5. Start IronClaw and verify it loads your personality and user profile

## What was NOT migrated

- OpenClaw gateway process state (stateless — IronClaw restarts cleanly)
- OpenClaw conversation history (use --include-history to opt in, then import manually)
- OpenClaw plugin code (re-install as Lumina skills or MCP plugins)
