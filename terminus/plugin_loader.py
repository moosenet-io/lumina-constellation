# /opt/ai-mcp/plugin_loader.py
"""
Plugin loader for Terminus MCP server.
Auto-discovers Python files in /opt/ai-mcp/plugins/ and registers their tools.

Convention: each plugin file must export a list called TOOLS containing
FastMCP tool functions (decorated with @mcp.tool() or returning callables).

Alternatively, plugins may export register_plugin(mcp) function.
"""
import os
import sys
import importlib.util
from pathlib import Path

PLUGINS_DIR = Path(os.environ.get('PLUGINS_DIR', '/opt/ai-mcp/plugins'))


def discover(mcp) -> dict:
    """Scan plugins/ directory and register all tools found.

    Returns dict: {plugin_name: {'status': 'ok'|'error', 'tools': [...], 'error': str}}
    """
    results = {}

    if not PLUGINS_DIR.exists():
        PLUGINS_DIR.mkdir(parents=True, exist_ok=True)
        return results

    for plugin_file in sorted(PLUGINS_DIR.glob('*.py')):
        if plugin_file.name.startswith('_'):
            continue  # skip __init__.py etc.

        plugin_name = plugin_file.stem
        try:
            spec = importlib.util.spec_from_file_location(
                f'plugins.{plugin_name}', plugin_file
            )
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)

            # Convention 1: register_plugin(mcp) function
            if hasattr(module, 'register_plugin'):
                module.register_plugin(mcp)
                results[plugin_name] = {'status': 'ok', 'method': 'register_plugin'}

            # Convention 2: TOOLS list of tool functions
            elif hasattr(module, 'TOOLS'):
                registered = []
                for tool_fn in module.TOOLS:
                    mcp.tool()(tool_fn)
                    registered.append(tool_fn.__name__)
                results[plugin_name] = {'status': 'ok', 'method': 'TOOLS', 'tools': registered}

            else:
                results[plugin_name] = {'status': 'skip', 'reason': 'no register_plugin or TOOLS export'}

        except Exception as e:
            results[plugin_name] = {'status': 'error', 'error': str(e)[:200]}

    return results


def list_plugins() -> list[dict]:
    """List all plugin files in the plugins directory."""
    plugins = []
    if not PLUGINS_DIR.exists():
        return plugins
    for f in sorted(PLUGINS_DIR.glob('*.py')):
        if not f.name.startswith('_'):
            plugins.append({'name': f.stem, 'file': f.name, 'size': f.stat().st_size})
    return plugins
