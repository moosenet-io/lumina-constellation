#!/usr/bin/env python3
"""
skill_tracker.py — Track skill usage and update metadata.
Called by skill_loader when a skill is loaded and after task completion.

Python file operations only — no LLM cost.
"""
import os
import re
import json
from pathlib import Path
from datetime import datetime

SKILLS_DIR = Path(os.environ.get('SKILLS_DIR', '/opt/lumina-fleet/skills'))


def _read_skill(name: str) -> tuple[dict, str, Path]:
    """Read skill metadata and body. Returns (meta, body, path)."""
    skill_file = SKILLS_DIR / 'active' / name / 'SKILL.md'
    if not skill_file.exists():
        return {}, '', skill_file
    content = skill_file.read_text()
    m = re.match(r'^---\n(.*?)\n---\n(.*)', content, re.DOTALL)
    if not m:
        return {}, content, skill_file
    try:
        import yaml
        meta = yaml.safe_load(m.group(1)) or {}
        return meta, m.group(2).strip(), skill_file
    except Exception:
        return {}, content, skill_file


def _write_skill(skill_file: Path, meta: dict, body: str):
    """Write skill metadata back to SKILL.md."""
    import yaml
    content = f"---\n{yaml.dump(meta, default_flow_style=False)}---\n\n{body}"
    skill_file.write_text(content)


def record_success(skill_name: str, context: str = '') -> bool:
    """Record a successful skill execution. Updates usage_count and last_success."""
    meta, body, skill_file = _read_skill(skill_name)
    if not skill_file.exists():
        return False
    
    meta['usage_count'] = meta.get('usage_count', 0) + 1
    meta['last_success'] = datetime.now().isoformat()
    
    _write_skill(skill_file, meta, body)
    return True


def record_failure(skill_name: str, failure_desc: str, context: str = '') -> bool:
    """Record a skill execution failure. Appends to pitfalls section."""
    meta, body, skill_file = _read_skill(skill_name)
    if not skill_file.exists():
        return False
    
    meta['failure_count'] = meta.get('failure_count', 0) + 1
    meta['last_failure'] = datetime.now().isoformat()
    
    # Append to pitfalls section in body
    pitfall_entry = f"\n- [{datetime.now().strftime('%Y-%m-%d')}] {failure_desc}"
    if '## Pitfalls' in body:
        body = body.replace('## Pitfalls', f'## Pitfalls{pitfall_entry}', 1)
    else:
        body = body + f"\n\n## Pitfalls{pitfall_entry}"
    
    _write_skill(skill_file, meta, body)
    return True


def get_usage_stats(skill_name: str) -> dict:
    """Get usage statistics for a skill."""
    meta, _, skill_file = _read_skill(skill_name)
    if not skill_file.exists():
        return {'error': f'Skill {skill_name!r} not found'}
    return {
        'name': skill_name,
        'usage_count': meta.get('usage_count', 0),
        'last_success': meta.get('last_success'),
        'failure_count': meta.get('failure_count', 0),
        'last_failure': meta.get('last_failure'),
    }


if __name__ == '__main__':
    import argparse
    parser = argparse.ArgumentParser()
    parser.add_argument('--success', metavar='SKILL', help='Record success for skill')
    parser.add_argument('--failure', metavar='SKILL', help='Record failure for skill')
    parser.add_argument('--desc', default='', help='Failure description')
    parser.add_argument('--stats', metavar='SKILL', help='Get usage stats for skill')
    args = parser.parse_args()
    
    if args.success:
        record_success(args.success)
        print(f"Recorded success for {args.success}")
    elif args.failure:
        record_failure(args.failure, args.desc)
        print(f"Recorded failure for {args.failure}")
    elif args.stats:
        print(json.dumps(get_usage_stats(args.stats), indent=2))
