#!/usr/bin/env python3
"""
soma_trajectory.py — Skill Evolution Trajectory Analyzer
Analyzes completed conversation logs for patterns with 5+ tool calls.
Extracts candidate skills in agentskills.io format.

Part of Lumina Constellation's Skill Evolution system (Session 12).
Runs as part of the 2 AM batch schedule on fleet-host.

Inference de-bloat: pattern detection is pure Python heuristics.
Only skill description generation uses Qwen local model.
"""
import os
import re
import json
import sys
from pathlib import Path
from datetime import datetime, timedelta
from collections import Counter

LOGS_DIR = Path(os.environ.get('IRONCLAW_LOGS_DIR', '/root/.ironclaw/logs'))
SKILLS_DIR = Path(os.environ.get('SKILLS_DIR', '/opt/lumina-fleet/skills'))
PROPOSED_DIR = SKILLS_DIR / 'proposed'
LITELLM_URL = os.environ.get('LITELLM_URL', 'http://YOUR_LITELLM_IP:4000')
LITELLM_KEY = os.environ.get('LITELLM_MASTER_KEY', '')

MIN_TOOL_CALLS = 5  # minimum tool calls to consider a trajectory "complex"
MIN_OCCURRENCES = 2  # minimum times a pattern must appear to become a skill candidate


def _find_log_files(hours_back: int = 24) -> list[Path]:
    """Find conversation log files from the last N hours."""
    cutoff = datetime.now() - timedelta(hours=hours_back)
    logs = []
    if not LOGS_DIR.exists():
        return logs
    for f in LOGS_DIR.rglob('*.json'):
        try:
            if f.stat().st_mtime > cutoff.timestamp():
                logs.append(f)
        except Exception:
            pass
    return logs


def _extract_tool_sequences(log_content: str) -> list[list[str]]:
    """Extract sequences of tool calls from a conversation log.
    Returns list of tool call sequences (each sequence = one conversation turn)."""
    sequences = []
    try:
        data = json.loads(log_content)
        turns = data if isinstance(data, list) else data.get('turns', [])
        
        current_sequence = []
        for turn in turns:
            # Handle both list and dict formats
            if isinstance(turn, dict):
                tool_calls = turn.get('tool_calls', [])
                if tool_calls:
                    for tc in tool_calls:
                        tool_name = tc.get('name', tc.get('function', {}).get('name', ''))
                        if tool_name:
                            current_sequence.append(tool_name)
                elif current_sequence:
                    if len(current_sequence) >= MIN_TOOL_CALLS:
                        sequences.append(current_sequence)
                    current_sequence = []
        
        if len(current_sequence) >= MIN_TOOL_CALLS:
            sequences.append(current_sequence)
    except Exception:
        # Try simple regex extraction as fallback
        tools = re.findall(r'"name":\s*"([a-z][a-z_]*)"', log_content)
        if len(tools) >= MIN_TOOL_CALLS:
            sequences.append(tools)
    
    return sequences


def _classify_sequence(tool_sequence: list[str]) -> str:
    """Classify a tool sequence into a skill domain using keyword heuristics."""
    sequence_str = ' '.join(tool_sequence)
    
    if any(t in sequence_str for t in ['calendar', 'briefing', 'news', 'commute', 'weather']):
        return 'daily-briefing'
    elif any(t in sequence_str for t in ['plane', 'work_item', 'project', 'issue']):
        return 'project-management'
    elif any(t in sequence_str for t in ['cortex', 'code', 'review', 'ast', 'git']):
        return 'code-review'
    elif any(t in sequence_str for t in ['engram', 'memory', 'store', 'query']):
        return 'memory-management'
    elif any(t in sequence_str for t in ['nexus', 'axon', 'message', 'inbox']):
        return 'task-delegation'
    elif any(t in sequence_str for t in ['deploy', 'ansible', 'systemd', 'docker']):
        return 'deployment'
    elif any(t in sequence_str for t in ['seer', 'research', 'search', 'web']):
        return 'research'
    else:
        return 'general-automation'


def _generate_skill_description(tool_sequence: list[str], domain: str) -> str:
    """Generate a skill description using local Qwen model via LiteLLM."""
    import urllib.request
    
    tool_list = ', '.join(tool_sequence[:10])
    prompt = f"""You are writing a skill description for an AI agent skill library.
Domain: {domain}
Tools used: {tool_list}

Write a single concise sentence (under 100 chars) describing what this skill does for the user.
Focus on the user benefit, not the technical implementation.
Example: "Generate a morning briefing with weather, calendar, and curated news"
Output ONLY the description sentence, nothing else."""

    try:
        data = json.dumps({
            'model': 'Lumina Fast',
            'messages': [{'role': 'user', 'content': prompt}],
            'max_tokens': 80
        }).encode()
        req = urllib.request.Request(
            f'{LITELLM_URL}/v1/chat/completions',
            data=data,
            headers={'Authorization': f'Bearer {LITELLM_KEY}', 'Content-Type': 'application/json'},
            method='POST'
        )
        with urllib.request.urlopen(req, timeout=20) as r:
            resp = json.load(r)
            return resp['choices'][0]['message']['content'].strip()
    except Exception:
        # Fallback to template description
        return f"Automate {domain.replace('-', ' ')} using {len(tool_sequence)} coordinated tool calls"


def analyze_trajectories(hours_back: int = 24, dry_run: bool = False) -> dict:
    """Main analysis function. Finds skill candidates from recent conversations.
    
    Returns dict: {candidates: [...], analyzed_logs: int, proposed: int}
    """
    log_files = _find_log_files(hours_back)
    all_sequences = []
    
    for log_file in log_files:
        try:
            content = log_file.read_text()
            sequences = _extract_tool_sequences(content)
            all_sequences.extend(sequences)
        except Exception:
            pass
    
    # Find recurring patterns (same domain appearing multiple times)
    domain_sequences = {}
    for seq in all_sequences:
        domain = _classify_sequence(seq)
        if domain not in domain_sequences:
            domain_sequences[domain] = []
        domain_sequences[domain].append(seq)
    
    candidates = []
    for domain, sequences in domain_sequences.items():
        if len(sequences) >= MIN_OCCURRENCES:
            # Use the longest sequence as the representative
            best_seq = max(sequences, key=len)
            description = _generate_skill_description(best_seq, domain)
            candidates.append({
                'name': domain,
                'description': description,
                'tool_sequence': best_seq[:15],  # cap at 15 tools
                'occurrences': len(sequences),
                'domain': domain,
            })
    
    proposed = 0
    if not dry_run:
        for candidate in candidates:
            _write_proposed_skill(candidate)
            proposed += 1
    
    return {
        'analyzed_logs': len(log_files),
        'sequences_found': len(all_sequences),
        'candidates': candidates,
        'proposed': proposed,
        'hours_back': hours_back,
    }


def _write_proposed_skill(candidate: dict) -> Path:
    """Write a proposed skill SKILL.md to the proposed directory."""
    import yaml
    
    name = candidate['name']
    skill_dir = PROPOSED_DIR / name
    skill_dir.mkdir(parents=True, exist_ok=True)
    
    meta = {
        'name': name,
        'description': candidate['description'],
        'version': '0.1',
        'status': 'proposed',
        'agent': 'lumina',
        'license': 'MIT',
        'tags': [candidate['domain']],
        'proposed_at': datetime.now().isoformat(),
        'occurrences': candidate['occurrences'],
    }
    
    procedure = f"""## Observed Procedure

This skill was automatically extracted from {candidate['occurrences']} similar conversation trajectories.

### Tool sequence observed:
```
{chr(10).join(f"- {t}" for t in candidate['tool_sequence'])}
```

## Approval needed

This skill is in **proposed** status. the operator must approve before it becomes active.
Reply to the Lumina notification with `approve {name}` or `reject {name}`.

## Notes

Auto-generated by soma_trajectory.py at {datetime.now().strftime('%Y-%m-%d %H:%M')}
"""
    
    content = f"---\n{yaml.dump(meta, default_flow_style=False)}---\n\n# {name}\n\n{candidate['description']}\n\n{procedure}"
    skill_file = skill_dir / 'SKILL.md'
    skill_file.write_text(content)
    return skill_file


if __name__ == '__main__':
    import argparse
    parser = argparse.ArgumentParser(description='Analyze conversation trajectories for skill candidates')
    parser.add_argument('--hours', type=int, default=24, help='Hours to look back')
    parser.add_argument('--dry-run', action='store_true', help='Analyze without writing proposed skills')
    args = parser.parse_args()
    
    result = analyze_trajectories(hours_back=args.hours, dry_run=args.dry_run)
    print(json.dumps(result, indent=2))
