# Relay

> Vehicle maintenance tracking via LubeLogger.

## What it does

Relay connects to LubeLogger to track vehicle service history, fuel logs, and maintenance schedules. Lumina can answer 'when is my oil change due?' and remind the operator before services are overdue.

## Deploys to

CT310 (YOUR_FLEET_SERVER_IP) at `/opt/lumina-fleet/relay/`

## MCP Tools

See `terminus/relay_tools.py` for available tools.

## Status

✅ Backend deployed (LubeLogger/Grocy running on CT310). Data entry needed — see Plane backlog.
