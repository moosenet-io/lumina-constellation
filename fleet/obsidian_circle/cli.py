#!/usr/bin/env python3
"""
cli.py — Obsidian Circle command-line interface. (OC.9)

Callable as a standalone script or via the lumina-council entry point.

Usage:
  python3 cli.py 'question' [options]
  python3 cli.py --list-presets
  python3 cli.py --list-personas

Options:
  --circle, -c PRESET    Circle preset (default: quick)
  --prism                Prism mode — same model, different personas
  --budget, -b FLOAT     Max spend in USD (default: 0.10)
  --json                 Output raw JSON (for scripted use)
  --no-resume            Skip checkpoint resume (fresh deliberation)
  --list-presets         Print all available circle presets
  --list-personas        Print all available personas

Examples:
  python3 cli.py 'Should we add Redis caching to Nexus?'
  python3 cli.py 'Evaluate the security posture of our key rotation system' --circle security --budget 0.50
  python3 cli.py 'Design the Synapse event pipeline' --circle architecture --budget 1.00
  python3 cli.py 'Rate this design decision' --circle full --prism --json
"""

import argparse
import json
import os
import sys
from pathlib import Path

# Fleet path resolution — works both from dev-host and deployed fleet-host
_FLEET_DIR = Path(os.environ.get('FLEET_DIR', str(Path(__file__).parent.parent.parent)))
sys.path.insert(0, str(_FLEET_DIR / 'fleet'))

try:
    from obsidian_circle import convene, list_presets, list_personas
    from obsidian_circle.output import format_for_operator
except ImportError as e:
    print(f'Error: could not import obsidian_circle — {e}', file=sys.stderr)
    print(f'Make sure FLEET_DIR is set and fleet/obsidian_circle/ exists.', file=sys.stderr)
    sys.exit(1)

# ── Display helpers ────────────────────────────────────────────────────────────

_ACTION_LABELS = {
    'auto_act':           'AUTO-ACT',
    'ask_operator':       'CONFIRM NEEDED',
    'surface_deliberation': 'DELIBERATE',
}


def _bar(width: int = 60, char: str = '═') -> str:
    return char * width


def _print_result(result: dict, json_output: bool = False, width: int = 60):
    if json_output:
        # Remove non-serializable keys defensively
        print(json.dumps(result, indent=2, default=str))
        return

    circle = result.get('circle', '?').upper()
    confidence = result.get('confidence', 0.0)
    action = result.get('action', '?')
    action_label = _ACTION_LABELS.get(action, action.upper())
    cost = result.get('cost_usd', 0.0)
    elapsed = result.get('elapsed_s', 0)
    member_count = result.get('member_count', 0)
    resumed = result.get('resumed', False)

    print()
    print(_bar(width))
    print(f'  OBSIDIAN CIRCLE — {circle} DELIBERATION{" (resumed)" if resumed else ""}')
    print(_bar(width))
    print(f'  Confidence: {confidence:.0%}  |  Action: {action_label}')
    print(f'  Members: {member_count}  |  Cost: ${cost:.4f}  |  Time: {elapsed}s')
    print(_bar(width, '─'))

    synthesis = result.get('synthesis', '')
    if synthesis:
        print('\n  SYNTHESIS\n')
        # Word-wrap synthesis to width
        words = synthesis.split()
        line, lines = [], []
        for w in words:
            if sum(len(x) + 1 for x in line) + len(w) > width - 4:
                lines.append('  ' + ' '.join(line))
                line = [w]
            else:
                line.append(w)
        if line:
            lines.append('  ' + ' '.join(line))
        print('\n'.join(lines))

    positions = result.get('positions', [])
    valid_positions = [p for p in positions if not p.get('error') == 'budget_exhausted']

    if valid_positions:
        print()
        print(_bar(width, '─'))
        print('  MEMBER POSITIONS\n')
        for p in valid_positions:
            if p.get('error') and p['error'] != 'budget_exhausted':
                print(f'  [{p.get("member_id", "?")}] ERROR: {p.get("position", "")[:100]}')
                print()
                continue
            persona = p.get('persona', p.get('member_id', '?'))
            conf = p.get('confidence', 0.0)
            position_text = p.get('position', '')
            # Truncate long positions
            if len(position_text) > 250:
                position_text = position_text[:247] + '...'
            print(f'  [{persona} — {conf:.0%}]')
            for chunk in [position_text[i:i+width-4] for i in range(0, len(position_text), width-4)]:
                print(f'  {chunk}')
            print()

    if result.get('action') == 'surface_deliberation':
        print('  ⚠  Confidence below 50% — surface full deliberation to operator.')
        print()

    print(_bar(width))
    print()


def _print_presets(presets: list):
    print(f'\nAvailable circle presets ({len(presets)}):\n')
    print(f'  {"Name":<20} {"Members":>7}  Description')
    print(f'  {"─" * 20} {"─" * 7}  {"─" * 30}')
    for p in presets:
        members = len(p.get('members', []))
        desc = p.get('description', '')
        name = p['name']
        flag = ' *' if p.get('custom') else ''
        print(f'  {name:<20} {members:>7}  {desc}{flag}')
    print()
    print('  * = custom preset (stored in constellation.yaml)')
    print()


def _print_personas(personas: list):
    print(f'\nAvailable personas ({len(personas)}):\n')
    print(f'  {"ID":<20} {"Name":<20} Description')
    print(f'  {"─" * 20} {"─" * 20} {"─" * 30}')
    for p in personas:
        flag = ' *' if p.get('custom') else ''
        print(f'  {p["id"]:<20} {p["name"]:<20} {p.get("description", "")}{flag}')
    print()


# ── Main ───────────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(
        prog='lumina-council',
        description='Obsidian Circle — multi-model deliberation CLI',
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__.split('Examples:')[1] if 'Examples:' in __doc__ else '',
    )

    parser.add_argument('question', nargs='?', help='Question or problem to deliberate on')
    parser.add_argument('--circle', '-c', default='quick',
                        help='Circle preset: quick/architecture/security/cost/research/full (default: quick)')
    parser.add_argument('--prism', action='store_true',
                        help='Prism mode: one model, multiple personas')
    parser.add_argument('--budget', '-b', type=float, default=0.10,
                        help='Max spend in USD (default: 0.10)')
    parser.add_argument('--json', action='store_true', dest='json_output',
                        help='Output raw JSON (for scripted use)')
    parser.add_argument('--no-resume', action='store_true',
                        help='Disable checkpoint resume — start fresh')
    parser.add_argument('--list-presets', action='store_true',
                        help='List all available circle presets')
    parser.add_argument('--list-personas', action='store_true',
                        help='List all available personas')
    parser.add_argument('--schema', type=str,
                        help='JSON output schema (inline JSON string or path to .json file)')

    args = parser.parse_args()

    if args.list_presets:
        _print_presets(list_presets())
        return

    if args.list_personas:
        _print_personas(list_personas())
        return

    if not args.question:
        parser.print_help()
        sys.exit(1)

    # Parse optional output schema
    output_schema = None
    if args.schema:
        schema_src = args.schema.strip()
        if schema_src.startswith('{'):
            try:
                output_schema = json.loads(schema_src)
            except json.JSONDecodeError as e:
                print(f'Error: invalid JSON schema: {e}', file=sys.stderr)
                sys.exit(1)
        else:
            schema_path = Path(schema_src)
            if not schema_path.exists():
                print(f'Error: schema file not found: {schema_path}', file=sys.stderr)
                sys.exit(1)
            output_schema = json.loads(schema_path.read_text())

    mode = 'prism' if args.prism else 'multi'
    resume = not args.no_resume

    if not args.json_output:
        print(f'Convening Obsidian Circle ({args.circle}, {mode} mode, budget ${args.budget:.2f})...')
        if resume:
            print('Checkpoint resume: enabled (--no-resume to disable)')

    try:
        result = convene(
            question=args.question,
            circle=args.circle,
            budget=args.budget,
            mode=mode,
            output_schema=output_schema,
            resume=resume,
        )
    except RuntimeError as e:
        print(f'Error: {e}', file=sys.stderr)
        if 'LITELLM_URL' in str(e):
            print('Set LITELLM_URL environment variable to your LiteLLM proxy endpoint.', file=sys.stderr)
        sys.exit(1)
    except Exception as e:
        print(f'Unexpected error: {e}', file=sys.stderr)
        sys.exit(1)

    _print_result(result, json_output=args.json_output)

    # Exit code: 0 if auto_act or ask_operator, 2 if surface_deliberation
    if result.get('action') == 'surface_deliberation':
        sys.exit(2)


if __name__ == '__main__':
    main()
