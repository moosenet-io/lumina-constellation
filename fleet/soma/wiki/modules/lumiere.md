# Lumiere

> Partner household agent — coordinates shared household context.

## What it does

Lumière is the partner household agent, running on <partner-host>. It coordinates shared household context (grocery lists, meal plans, travel, budget, vehicle) while keeping personal data private between agents.

## Deploys to

<partner-host> at `/root/.ironclaw/` (separate container per household member)

## MCP Tools

See `terminus/lumiere_tools.py` for available tools.

## Status

✅ Running on <partner-host>. Shares household context with Lumina via Nexus.
