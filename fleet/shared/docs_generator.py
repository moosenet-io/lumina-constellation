#!/usr/bin/env python3
"""
Docs Generator — auto-generates reference documentation from code.
Run on Gitea push to lumina-terminus or lumina-fleet to keep docs fresh.

Outputs:
  docs/reference/mcp-tools.md      — from server.py tool docstrings
  docs/reference/api-endpoints.md  — from gateway.py FastAPI routes
  docs/reference/agent-yaml-format.md — from agent_loader.py schema
"""

import ast
import json
import os
import sys
import subprocess
from pathlib import Path
from datetime import datetime

DOCS_DIR = Path('/opt/lumina-fleet/docs/reference')
MCP_SERVER = Path('/opt/ai-mcp/server.py')
GATEWAY = Path('/opt/lumina-fleet/gateway/gateway.py')
AGENTS_DIR = Path('/opt/lumina-fleet/agents')

DOCS_DIR.mkdir(parents=True, exist_ok=True)


def _extract_tool_docstrings(server_py_path: Path) -> list:
    """Parse server.py and extract all @mcp.tool() decorated functions with docstrings."""
    tools = []
    try:
        source = server_py_path.read_text()
        tree = ast.parse(source)
    except Exception as e:
        return [{'error': str(e)}]

    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            # Check if decorated with @mcp.tool()
            for decorator in node.decorator_list:
                if (isinstance(decorator, ast.Call) and
                        hasattr(decorator.func, 'attr') and
                        decorator.func.attr == 'tool'):
                    # Extract docstring
                    docstring = ast.get_docstring(node) or ''
                    # Extract args
                    args = []
                    for arg in node.args.args:
                        if arg.arg != 'self':
                            args.append(arg.arg)
                    tools.append({
                        'name': node.name,
                        'docstring': docstring,
                        'args': args,
                        'line': node.lineno,
                    })
    return tools


def generate_mcp_tools_doc() -> str:
    """Generate docs/reference/mcp-tools.md from server.py."""
    lines = [
        '# MCP Tools Reference',
        f'',
        f'*Auto-generated from `/opt/ai-mcp/server.py` on {datetime.now().strftime("%Y-%m-%d")}.*',
        f'',
        f'Terminus (CT214) provides these MCP tools to IronClaw. Total: see count below.',
        f'',
    ]

    # Group tools by module
    module_tools = {}
    tools = _extract_tool_docstrings(MCP_SERVER) if MCP_SERVER.exists() else []

    # Group by prefix (e.g. nexus_*, engram_*, etc.)
    for tool in tools:
        name = tool['name']
        prefix = name.split('_')[0] if '_' in name else 'other'
        module_tools.setdefault(prefix, []).append(tool)

    lines.append(f'**Total tools: {len(tools)}** across {len(module_tools)} modules.')
    lines.append('')

    for module, module_tool_list in sorted(module_tools.items()):
        lines.append(f'## {module.title()} ({len(module_tool_list)} tools)')
        lines.append('')
        for tool in sorted(module_tool_list, key=lambda x: x['name']):
            lines.append(f'### `{tool["name"]}({", ".join(tool["args"])})`')
            if tool['docstring']:
                # First line of docstring
                first_line = tool['docstring'].split('\n')[0].strip()
                lines.append(f'{first_line}')
                # Full docstring
                if '\n' in tool['docstring']:
                    lines.append('')
                    lines.append('```')
                    lines.append(tool['docstring'].strip())
                    lines.append('```')
            lines.append('')

    return '\n'.join(lines)


def generate_api_endpoints_doc() -> str:
    """Generate docs/reference/api-endpoints.md from gateway.py."""
    lines = [
        '# Gateway API Reference',
        '',
        f'*Auto-generated from `/opt/lumina-fleet/gateway/gateway.py` on {datetime.now().strftime("%Y-%m-%d")}.*',
        '',
        'All endpoints require `X-API-Key: {DASHBOARD_API_KEY}` header.',
        'Base URL: `http://YOUR_FLEET_SERVER_IP:8080` (CT310 gateway)',
        '',
    ]

    if not GATEWAY.exists():
        lines.append('*Gateway file not found.*')
        return '\n'.join(lines)

    try:
        source = GATEWAY.read_text()
        tree = ast.parse(source)
    except Exception:
        lines.append('*Could not parse gateway.py*')
        return '\n'.join(lines)

    endpoints = []
    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            for decorator in node.decorator_list:
                if isinstance(decorator, ast.Call):
                    func = decorator.func
                    if (hasattr(func, 'attr') and
                            func.attr in ('get', 'post', 'put', 'delete', 'patch')):
                        path_arg = ''
                        if decorator.args:
                            arg = decorator.args[0]
                            if isinstance(arg, ast.Constant):
                                path_arg = arg.value
                        docstring = ast.get_docstring(node) or ''
                        endpoints.append({
                            'method': func.attr.upper(),
                            'path': path_arg,
                            'function': node.name,
                            'docstring': docstring.split('\n')[0].strip() if docstring else '',
                        })

    lines.append(f'**Total endpoints: {len(endpoints)}**')
    lines.append('')
    lines.append('| Method | Path | Description |')
    lines.append('|--------|------|-------------|')
    for ep in sorted(endpoints, key=lambda x: x['path']):
        lines.append(f'| `{ep["method"]}` | `{ep["path"]}` | {ep["docstring"]} |')

    lines.append('')
    lines.append('## Authentication')
    lines.append('')
    lines.append('Set `DASHBOARD_API_KEY` in your `.env` or `axon/.env`.')
    lines.append('')
    lines.append('```bash')
    lines.append('curl http://YOUR_FLEET_SERVER_IP:8080/api/health \\')
    lines.append('  -H "X-API-Key: your-key"')
    lines.append('```')

    return '\n'.join(lines)


def generate_agent_yaml_doc() -> str:
    """Generate docs/reference/agent-yaml-format.md from schema."""
    lines = [
        '# .agent.yaml Format Reference',
        '',
        f'*Auto-generated on {datetime.now().strftime("%Y-%m-%d")}.*',
        '',
        'Every Lumina agent is defined by a `.agent.yaml` file in `/opt/lumina-fleet/agents/`.',
        'This is the single source of truth for agent identity, routing, tools, and deployment.',
        '',
        '## Full schema',
        '',
        '```yaml',
        '# /opt/lumina-fleet/agents/{name}.agent.yaml',
        '',
        'name: string           # Internal codename (e.g. "vigil"). Stable — never changes.',
        'display_name: string   # User-visible name (e.g. "Vigil"). Can be changed in Soma.',
        'description: string    # One-line description of this agent\'s role.',
        'personality: string    # Voice/tone descriptor used in system_prompt.',
        'emoji: string          # Single emoji for UI display.',
        '',
        'system_prompt: |       # Full system prompt (multi-line)',
        '  You are {display_name}...',
        '',
        'routes:                # Ordered list — first enabled route is used',
        '  - type: openrouter   # openrouter | oauth | litellm | ollama',
        '    model: anthropic/claude-sonnet-4-6',
        '    enabled: true',
        '  - type: litellm',
        '    model: claude-sonnet-4-6',
        '    enabled: true',
        '  - type: ollama',
        '    model: qwen2.5:7b',
        '    endpoint: http://YOUR_GPU_HOST_IP:11434',
        '    enabled: true',
        '',
        'tools:                 # MCP tool categories this agent uses',
        '  - nexus',
        '  - engram',
        '  - google_calendar',
        '',
        'refractor_categories:  # Refractor filter categories to enable',
        '  - nexus',
        '  - engram',
        '  - google',
        '',
        'engram:',
        '  namespace: agents/lumina    # Engram key prefix for personal memory',
        '  shared_namespaces:',
        '    - household               # Additional namespaces this agent reads',
        '',
        'channels:',
        '  - type: matrix',
        '    room: "!lumina:matrix.moosenet.local"',
        '  - type: http',
        '    port: 3001',
        '',
        'container: CT305       # Where this agent runs (CT number)',
        'runtime: ironclaw      # ironclaw | subprocess | systemd',
        'auto_start: true       # Start automatically on container boot',
        '',
        '# Council agents only:',
        'council_seat: true',
        'parent_agent: wizard   # Parent that spawns this agent',
        'persona_file: /path/to/persona.md',
        '```',
        '',
        '## Loading in code',
        '',
        '```python',
        'from agent_loader import AgentLoader, display_name, get_agent',
        '',
        '# Get display name (falls back to constellation.yaml, then name.title())',
        'name = display_name("vigil")  # → "Vigil" (or custom name)',
        '',
        '# Get full agent object',
        'agent = get_agent("lumina")',
        'print(agent.primary_model)   # → "anthropic/claude-sonnet-4-6"',
        'print(agent.container)       # → "CT305"',
        '',
        '# List all agents',
        'from agent_loader import load_agents',
        'agents = load_agents()  # {name: Agent}',
        '```',
        '',
        '## Existing agents',
        '',
    ]

    # List existing .agent.yaml files
    if AGENTS_DIR.exists():
        for f in sorted(AGENTS_DIR.glob('*.agent.yaml')):
            lines.append(f'- `{f.name}`')
    else:
        lines.append('*No agents directory found.*')

    return '\n'.join(lines)


def generate_all():
    """Generate all reference documentation."""
    print('[docs_generator] Generating reference documentation...')

    # MCP tools
    mcp_doc = generate_mcp_tools_doc()
    mcp_path = DOCS_DIR / 'mcp-tools.md'
    mcp_path.write_text(mcp_doc)
    print(f'  Written: {mcp_path}')

    # API endpoints
    api_doc = generate_api_endpoints_doc()
    api_path = DOCS_DIR / 'api-endpoints.md'
    api_path.write_text(api_doc)
    print(f'  Written: {api_path}')

    # Agent YAML format
    agent_doc = generate_agent_yaml_doc()
    agent_path = DOCS_DIR / 'agent-yaml-format.md'
    agent_path.write_text(agent_doc)
    print(f'  Written: {agent_path}')

    print('[docs_generator] Done.')
    return {'files': 3}


if __name__ == '__main__':
    generate_all()
