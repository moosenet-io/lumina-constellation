# ✦ Plugins

> "Drop it in. It loads itself."

The plugin system for extending Terminus with custom tool modules — no server.py edits required.

## How it works

`terminus/plugin_loader.py` auto-discovers `.py` files in `terminus/plugins/` at startup. Each plugin registers MCP tools by calling a `register_*_tools(mcp)` function.

## Included plugins

| Plugin | What it does |
|--------|-------------|
| `synapse_tools.py` | Synapse control via SSH to fleet host |
| `pulse_tools.py` | Temporal awareness (now, short, mark, since, timer_elapsed) |
| `vector_onboard_plugin.py` | Vector agent onboarding helpers |
| `news_plugin.py` | NewsAPI news fetching |
| `example_plugin.py` | Starter template for new plugins |

## Writing a plugin

```python
# terminus/plugins/my_plugin.py
def register_my_tools(mcp):
    @mcp.tool()
    def my_tool(param: str) -> str:
        """Does the thing."""
        return f"Result: {param}"
```

Drop the file in `terminus/plugins/`. Terminus auto-loads it on next restart. No server.py edit needed.

---

Part of [Lumina Constellation](../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
