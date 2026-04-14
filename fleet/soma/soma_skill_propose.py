#!/usr/bin/env python3
"""
soma_skill_propose.py — Skill Proposal + Approval Workflow
Handles the lifecycle from proposed → approved/rejected skills.

Sends Nexus notifications to Lumina for the operator's approval.
On approval, moves skill from proposed/ to active/.

Usage:
  python3 soma_skill_propose.py --check        # check for new proposed skills, send notifications
  python3 soma_skill_propose.py --approve NAME  # approve a proposed skill
  python3 soma_skill_propose.py --reject NAME   # reject a proposed skill
  python3 soma_skill_propose.py --list          # list proposed skills
"""
import os
import sys
import json
import re
import urllib.request
from pathlib import Path
from datetime import datetime

SKILLS_DIR = Path(os.environ.get('SKILLS_DIR', '/opt/lumina-fleet/skills'))
PROPOSED_DIR = SKILLS_DIR / 'proposed'
ACTIVE_DIR = SKILLS_DIR / 'active'

NEXUS_DB_HOST = os.environ.get('INBOX_DB_HOST', 'YOUR_POSTGRES_IP')
NEXUS_DB_USER = os.environ.get('INBOX_DB_USER', 'lumina_inbox_user')
NEXUS_DB_PASS = os.environ.get('INBOX_DB_PASS', '')


def _send_nexus(from_agent: str, to_agent: str, message_type: str, payload: str) -> bool:
    """Send a message via Nexus inbox (direct psycopg2 insert)."""
    try:
        import psycopg2
        conn = psycopg2.connect(
            host=NEXUS_DB_HOST, dbname='lumina_inbox',
            user=NEXUS_DB_USER, password=NEXUS_DB_PASS, connect_timeout=5
        )
        cur = conn.cursor()
        cur.execute("""
            INSERT INTO inbox_messages (from_agent, to_agent, message_type, payload, priority, status)
            VALUES (%s, %s, %s, %s, 'normal', 'pending')
        """, (from_agent, to_agent, message_type, payload))
        conn.commit()
        conn.close()
        return True
    except Exception as e:
        print(f"Nexus send failed: {e}", file=sys.stderr)
        return False


def _parse_frontmatter(content: str) -> dict:
    """Extract YAML frontmatter from SKILL.md."""
    m = re.match(r'^---\n(.*?)\n---', content, re.DOTALL)
    if not m:
        return {}
    try:
        import yaml
        return yaml.safe_load(m.group(1)) or {}
    except Exception:
        return {}


def list_proposed() -> list[dict]:
    """List all proposed skills."""
    skills = []
    if not PROPOSED_DIR.exists():
        return skills
    for skill_dir in sorted(PROPOSED_DIR.iterdir()):
        skill_file = skill_dir / 'SKILL.md'
        if skill_dir.is_dir() and skill_file.exists():
            meta = _parse_frontmatter(skill_file.read_text())
            skills.append({
                'name': skill_dir.name,
                'description': meta.get('description', ''),
                'proposed_at': meta.get('proposed_at', ''),
                'occurrences': meta.get('occurrences', 0),
                'notified': meta.get('notification_sent', False),
            })
    return skills


def send_approval_requests() -> int:
    """Send Nexus notifications for proposed skills that haven't been notified yet."""
    proposed = list_proposed()
    sent = 0
    for skill in proposed:
        if not skill.get('notified'):
            payload = json.dumps({
                'action': 'skill_approval_request',
                'skill_name': skill['name'],
                'description': skill['description'],
                'occurrences': skill['occurrences'],
                'message': (
                    f"New skill proposed: **{skill['name']}**\n"
                    f"Description: {skill['description']}\n"
                    f"Observed {skill['occurrences']} times in recent conversations.\n\n"
                    f"Reply: `approve {skill['name']}` or `reject {skill['name']}`"
                )
            })
            ok = _send_nexus('soma', 'lumina', 'notification', payload)
            if ok:
                # Mark as notified
                skill_file = PROPOSED_DIR / skill['name'] / 'SKILL.md'
                content = skill_file.read_text()
                import yaml
                meta = _parse_frontmatter(content)
                meta['notification_sent'] = True
                meta['notified_at'] = datetime.now().isoformat()
                # Rewrite frontmatter
                body = re.sub(r'^---\n.*?\n---\n', '', content, flags=re.DOTALL).strip()
                new_content = f"---\n{yaml.dump(meta, default_flow_style=False)}---\n\n{body}"
                skill_file.write_text(new_content)
                sent += 1
                print(f"Notified: {skill['name']}")
    return sent


def approve_skill(name: str) -> bool:
    """Move a skill from proposed/ to active/."""
    src = PROPOSED_DIR / name
    dst = ACTIVE_DIR / name
    if not src.exists():
        print(f"Error: proposed skill '{name}' not found", file=sys.stderr)
        return False
    if dst.exists():
        import shutil
        shutil.rmtree(dst)
    import shutil
    shutil.copytree(src, dst)
    shutil.rmtree(src)
    
    # Update metadata
    skill_file = dst / 'SKILL.md'
    content = skill_file.read_text()
    import yaml
    meta = _parse_frontmatter(content)
    meta['status'] = 'active'
    meta['approved_at'] = datetime.now().isoformat()
    body = re.sub(r'^---\n.*?\n---\n', '', content, flags=re.DOTALL).strip()
    new_content = f"---\n{yaml.dump(meta, default_flow_style=False)}---\n\n{body}"
    skill_file.write_text(new_content)
    
    print(f"Approved and activated: {name}")
    _send_nexus('soma', 'lumina', 'notification', json.dumps({
        'action': 'skill_approved',
        'skill_name': name,
        'message': f"Skill '{name}' approved and now active. Available via skills_list."
    }))
    return True


def reject_skill(name: str) -> bool:
    """Remove a proposed skill."""
    src = PROPOSED_DIR / name
    if not src.exists():
        print(f"Error: proposed skill '{name}' not found", file=sys.stderr)
        return False
    import shutil
    shutil.rmtree(src)
    print(f"Rejected and removed: {name}")
    return True


if __name__ == '__main__':
    import argparse
    parser = argparse.ArgumentParser(description='Skill proposal workflow')
    parser.add_argument('--check', action='store_true', help='Send notifications for new proposed skills')
    parser.add_argument('--list', action='store_true', help='List proposed skills')
    parser.add_argument('--approve', metavar='NAME', help='Approve a proposed skill')
    parser.add_argument('--reject', metavar='NAME', help='Reject a proposed skill')
    args = parser.parse_args()
    
    if args.list:
        proposed = list_proposed()
        print(json.dumps(proposed, indent=2))
    elif args.check:
        sent = send_approval_requests()
        print(f"Sent {sent} approval notification(s)")
    elif args.approve:
        approve_skill(args.approve)
    elif args.reject:
        reject_skill(args.reject)
    else:
        parser.print_help()
