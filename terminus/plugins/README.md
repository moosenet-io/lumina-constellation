# Terminus Plugins

> Drop-in Python tool extensions for the MCP hub. No server.py changes required.

## What's here

The `plugins/` directory contains Python tool extensions that are auto-discovered at startup. Any `.py` file here that exports `register_plugin(mcp)` or a `TOOLS` list is automatically loaded.

## Key files

| File | Purpose |
|------|---------|
| `example_plugin.py` | Example plugin with `constellation_version` tool |
| `news_plugin.py` | News tools (migrated from server.py direct registration) |
| `README.md` | This file |

## How to add a plugin

1. Create a `.py` file in `/opt/ai-mcp/plugins/` on CT214 (<terminus-ip>)
2. Export `register_plugin(mcp)` function **or** a `TOOLS` list of tool functions
3. Restart the MCP server: `systemctl restart mcp-server`
4. Verify: `constellation_version` or your new tool appears in IronClaw

## Plugin format — Option A: register_plugin

```python
def register_plugin(mcp):
    @mcp.tool()
    def my_tool(param: str) -> dict:
        """Tool description for IronClaw."""
        return {'result': param}
```

## Plugin format — Option B: TOOLS list

```python
def my_tool(param: str) -> dict:
    """Tool description."""
    return {'result': param}

TOOLS = [my_tool]
```

## Adding Refractor keywords

To make your tool discoverable by the keyword router (Refractor on CT305), add keywords to `/usr/local/bin/llm-proxy.py` on CT305 in the KEYWORD_CATEGORIES dict.

## How it's deployed

Plugins directory: `/opt/ai-mcp/plugins/` on CT214 (<terminus-ip>)
Loaded by: `plugin_loader.py` → called from `server.py` at startup
MCP server: `systemctl status mcp-server` on CT214

## Configuration

No dedicated config file. Plugins inherit the Terminus `.env` secrets (injected at server start via `stdio.sh`). If a plugin needs a new secret, add it to `.env` and Infisical (CT221, workspace: moosenet-services, env: prod).

## History / Lineage

The plugin loader was introduced in session 10 to allow new tool modules to be added without modifying `server.py`. Before that, every new tool required a direct edit to the main server file. The `example_plugin.py` and `news_plugin.py` were the first two modules migrated to the plugin format as a proof-of-concept.

## Credits

- FastMCP plugin discovery pattern — [jlowin/fastmcp](https://github.com/jlowin/fastmcp) (MIT, Jeremy Lowin)

## Development

1. Write your plugin locally
2. `scp plugin.py root@<pvm-host-ip>:/tmp/plugin.py`
3. `ssh root@<pvm-host-ip> "pct push 214 /tmp/plugin.py /opt/ai-mcp/plugins/plugin.py"`
4. `ssh root@<pvm-host-ip> "pct exec 214 -- systemctl restart mcp-server"`
