#!/usr/bin/env python3
"""Vector CLI entry point."""
import argparse, yaml, os, sys, logging
from pathlib import Path

logging.basicConfig(level=logging.INFO, format='%(asctime)s [vector] %(levelname)s %(message)s')

def load_config(config_path='./vector.yaml'):
    path = Path(config_path)
    if not path.exists():
        example = Path('/opt/lumina-fleet/vector/config/vector.yaml.example')
        if example.exists():
            print(f'No vector.yaml found. Copy from: {example}')
        return {}
    with open(path) as f:
        return yaml.safe_load(f) or {}

def build_backends(config):
    mode = config.get('mode', 'standalone')
    sys.path.insert(0, '/opt/lumina-fleet/vector')
    sys.path.insert(0, '/opt/lumina-fleet/vector/backends')

    if mode == 'standalone':
        from backends.standalone.sqlite_state import SQLiteState
        from backends.standalone.stdout_bus import StdoutMessageBus
        from backends.standalone.local_memory import LocalMemoryStore
        from backends.standalone.local_cost import LocalCostGate
        sc = config.get('standalone', {})
        state = SQLiteState(sc.get('state_db', './vector-state.db'))
        bus = StdoutMessageBus(sc.get('log_file', './vector.log'), sc.get('interactive', False))
        memory = LocalMemoryStore(sc.get('memory_dir', './memory'), sc.get('conventions', './conventions.md'))
        cost = LocalCostGate(sc.get('max_cost_per_run', 5.00))
    elif mode == 'integrated':
        # Integrated backends (Phase 3 — not yet built, fall back to standalone)
        print('[vector] Integrated backends not yet built — falling back to standalone')
        return build_backends({**config, 'mode': 'standalone'})
    else:
        raise ValueError(f'Unknown mode: {mode}')
    return state, bus, memory, cost

def cmd_init(args):
    """Initialize a new Vector project with template files."""
    from pathlib import Path
    import os

    project_dir = Path(f'./vector-projects/{args.project}')
    project_dir.mkdir(parents=True, exist_ok=True)

    templates = {
        'CONTEXT.md': f"""# {args.project} — Vector Project Context

## Project description
[Describe what this project does]

## Key files
[List the main files to be modified]

## Architecture notes
[Any architectural constraints or decisions]
""",
        'guardrails.md': f"""# {args.project} — Project Guardrails

- Never modify files outside the project directory without explicit instruction
- Run tests before committing changes
- Keep functions under 50 lines

""",
        'patterns.md': f"""# {args.project} — Coding Patterns

## Naming conventions
[Project-specific naming rules]

## Code style
[Style requirements]

""",
        'activity.md': f"""# {args.project} — Activity Log

## Sessions
<!-- Vector logs activity here automatically -->

""",
    }

    for filename, content in templates.items():
        path = project_dir / filename
        if not path.exists():
            path.write_text(content)
            print(f'Created: {path}')
        else:
            print(f'Exists:  {path}')

    # Write vector.yaml for this project
    config_path = project_dir / 'vector.yaml'
    if not config_path.exists():
        config_path.write_text(f"""mode: {args.mode}
project: {args.project}

llm:
  model: claude-sonnet
  base_url: ${{LITELLM_URL}}
  api_key: ${{LITELLM_MASTER_KEY}}

cost:
  max_per_task: 2.00
  max_per_day: 10.00

delegation:
  enabled: true
  scaffold_model: Lumina Fast
  primary_model: claude-sonnet

git:
  branch_prefix: vector/{args.project}
""")
        print(f'Created: {config_path}')

    print(f'\nProject initialized: vector-projects/{args.project}/')
    print(f'Edit vector-projects/{args.project}/CONTEXT.md to describe your project.')
    print(f'Run: python3 vector.py run --config vector-projects/{args.project}/vector.yaml "your task"')

    # Integrated mode: also create Plane project items if configured
    if args.mode == 'integrated':
        plane_token = os.environ.get('PLANE_API_TOKEN', '')
        if plane_token:
            print('\n[integrated] Creating Plane project tracking...')
            # Just note it — full Plane integration requires the backends
        print('[integrated] Use `vector run` with the integrated backend to track in Plane.')


def main():
    parser = argparse.ArgumentParser(description='Vector autonomous dev loop')
    sub = parser.add_subparsers(dest='cmd')

    p = sub.add_parser('run')
    p.add_argument('--task', required=True, help='Development task to execute')
    p.add_argument('--repo', default='.', help='Repository path')
    p.add_argument('--config', default='./vector.yaml', help='Config file')
    p.add_argument('--model', default='', help='Override LLM model')
    p.add_argument('--interactive', action='store_true')

    p = sub.add_parser('status')
    p.add_argument('--config', default='./vector.yaml')

    p = sub.add_parser('cost')
    p.add_argument('--config', default='./vector.yaml')

    init_parser = sub.add_parser('init', help='Initialize a new Vector project')
    init_parser.add_argument('project', help='Project name')
    init_parser.add_argument('--mode', default='standalone', choices=['standalone', 'integrated'])

    args = parser.parse_args()
    if not args.cmd:
        parser.print_help(); return

    config = load_config(args.config if hasattr(args, 'config') else './vector.yaml')

    if args.cmd == 'run':
        state, bus, memory, cost = build_backends(config)
        from core.loop import VectorLoop, LoopConfig
        llm_cfg = config.get('llm', {})
        loop_config = LoopConfig(
            max_iterations=config.get('standalone', {}).get('max_iterations', 10),
            max_cost=config.get('standalone', {}).get('max_cost_per_run', 5.00),
            llm_url=llm_cfg.get('endpoint', 'http://YOUR_LITELLM_IP:4000'),
            llm_key=os.environ.get(llm_cfg.get('api_key_env', 'LITELLM_MASTER_KEY'), ''),
            llm_model=args.model or llm_cfg.get('model', 'claude-sonnet-4-6'),
            repo_path=args.repo,
            interactive=args.interactive
        )
        loop = VectorLoop(state, bus, memory, cost, loop_config)
        result = loop.run({'task': args.task, 'repo': args.repo})
        print(f'\nResult: {result["status"]} — {result.get("tasks_completed", 0)} tasks completed, ${result.get("cost", 0):.2f} spent')

    elif args.cmd == 'status':
        state, bus, memory, cost = build_backends(config)
        tasks = state.list_tasks()
        print(f'Tasks: {len(tasks)} total')
        for t in tasks[-5:]:
            print(f'  {t.status}: {t.name[:60]}')

    elif args.cmd == 'cost':
        _, _, _, cost = build_backends(config)
        print(f'Remaining budget: ${cost.get_remaining():.2f}')

    elif args.cmd == 'init':
        cmd_init(args)

if __name__ == '__main__':
    main()
