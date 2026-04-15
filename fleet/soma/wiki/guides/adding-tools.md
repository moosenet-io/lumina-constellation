# Adding MCP Tools to Terminus

Terminus is the MCP tool hub. Adding a new tool module follows a consistent pattern across all 20 existing modules.

**Where tools live:** <terminus-host> at `/opt/ai-mcp/`
**Monorepo location:** `terminus/`
**Framework:** FastMCP (jlowin/fastmcp)

## Overview

1. Write `yourmodule_tools.py` with a `register_yourmodule_tools(mcp)` function
2. Register in `server.py`
3. Add secrets to Infisical (if needed)
4. Add a Refractor keyword category (if needed)
5. Restart the MCP server on <terminus-host>
6. Test from <ironclaw-host>

## Step 1: Write the Tool Module

```python
# terminus/yourmodule_tools.py
"""
yourmodule_tools.py — MCP tools for [description].
Part of Lumina Terminus.
"""
from fastmcp import FastMCP


def register_yourmodule_tools(mcp: FastMCP):

    @mcp.tool()
    def yourmodule_action(param1: str, param2: int = 10) -> str:
        """
        Do something useful. (This docstring becomes the tool description in MCP.)

        Args:
            param1: Description of param1
            param2: Description of param2 (default: 10)

        Returns:
            JSON string with results
        """
        import json
        import os

        # Use env vars for secrets — never hardcode
        api_key = os.environ.get('YOUR_API_KEY')
        if not api_key:
            return json.dumps({"error": "YOUR_API_KEY not set"})

        # Do the work
        result = {"status": "ok", "data": param1}
        return json.dumps(result)

    @mcp.tool()
    def yourmodule_status() -> str:
        """Check the status of yourmodule."""
        import json
        return json.dumps({"status": "ok"})
```

### Tool Writing Rules

- Every tool returns a **string** (JSON is strongly preferred)
- Use `os.environ.get()` for secrets — never hardcode
- Docstrings become tool descriptions in MCP — write them clearly
- Keep each tool focused: one function, one purpose
- Handle errors gracefully and return them in the JSON response

## Step 2: Register in server.py

Add two lines to `terminus/server.py`:

```python
# At the top, with other imports:
from yourmodule_tools import register_yourmodule_tools

# In the registration block:
register_yourmodule_tools(mcp)
```

## Step 3: Add Secrets (if needed)

If your tool needs API keys or credentials:

**Add a stub to `.env` on <terminus-host>:**
```bash
# /opt/ai-mcp/.env
YOUR_API_KEY=
```

**Add the secret to Infisical:**
- Infisical URL: `http://YOUR_INFISICAL_IP:8080`
- Workspace: `moosenet-services`
- Environment: `prod`
- Key: `YOUR_API_KEY`

**Add to `fetch-mcp-secrets.sh`:**
```bash
# <terminus-host>: /opt/ai-mcp/fetch-mcp-secrets.sh
infisical run --env=prod -- env | grep YOUR_API_KEY >> /opt/ai-mcp/.env
```

## Step 4: Add a Refractor Category (if needed)

Refractor (the Smart Proxy on <ironclaw-host>) filters the 200+ tools to 17–28 per turn. If your tools should be available in specific contexts, add a keyword category to the Refractor config.

Edit `/usr/local/bin/llm-proxy.py` on <ironclaw-host>:

```python
CATEGORIES = {
    # ... existing categories ...
    "yourmodule": ["your", "keyword", "list"],
}

TOOL_CATEGORIES = {
    # ... existing mappings ...
    "yourmodule_action": "yourmodule",
    "yourmodule_status": "yourmodule",
}
```

## Step 5: Deploy and Restart

```bash
# Copy the new tool file to <terminus-host>
scp terminus/yourmodule_tools.py root@YOUR_TERMINUS_IP:/opt/ai-mcp/yourmodule_tools.py

# Update server.py on <terminus-host>
scp terminus/server.py root@YOUR_TERMINUS_IP:/opt/ai-mcp/server.py

# Restart MCP server
ssh root@YOUR_TERMINUS_IP "systemctl restart ai-mcp"
```

Or from <dev-host> via PVM:
```bash
ssh root@YOUR_PVM_HOST_IP "pct exec 214 -- systemctl restart ai-mcp"
```

## Step 6: Test

From <ironclaw-host>, test the new tools are discoverable:

```bash
ssh root@YOUR_IRONCLAW_IP "ironclaw mcp test moosenet 2>&1 | grep yourmodule"
```

Or trigger a tool call via Lumina in Matrix.

## Common Mistakes

| Mistake | Fix |
|---------|-----|
| Tool not returning string | Always `return json.dumps(result)` |
| Secret not in .env | Add stub line to .env before running fetch-secrets |
| Tools not appearing | Check server.py has both import and register call |
| 500 error on call | Check <terminus-host> logs: `journalctl -u ai-mcp -n 50` |
| Refractor hiding tools | Add keywords to the appropriate category |

## Existing Tool Modules

See the [Terminus README](https://github.com/moosenet-io/lumina-constellation/tree/main/terminus) for the full list of 20 existing modules.

## Related

- [Creating Skills](creating-skills.md) — Skills that call these tools
- [Architecture Overview](../architecture/constellation-overview.md) — Where Terminus fits
- FastMCP docs: [github.com/jlowin/fastmcp](https://github.com/jlowin/fastmcp)
