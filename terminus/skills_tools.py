# /opt/ai-mcp/skills_tools.py
import os, sys, json
from pathlib import Path

SKILLS_BASE = os.environ.get('SKILLS_DIR', '/opt/lumina-fleet/skills')

def _parse_frontmatter(content):
    import re, yaml
    m = re.match(r'^---\n(.*?)\n---\n(.*)', content, re.DOTALL)
    if not m:
        return {}, content
    try:
        return yaml.safe_load(m.group(1)) or {}, m.group(2).strip()
    except Exception:
        return {}, content

def register_skills_tools(mcp):

    @mcp.tool()
    def skills_list() -> dict:
        """List all available agent skills with names and descriptions.
        Returns skills from the active skills directory in agentskills.io format."""
        active_dir = Path(SKILLS_BASE) / 'active'
        skills = []
        if active_dir.exists():
            for skill_dir in sorted(active_dir.iterdir()):
                skill_file = skill_dir / 'SKILL.md'
                if skill_dir.is_dir() and skill_file.exists():
                    content = skill_file.read_text()
                    meta, body = _parse_frontmatter(content)
                    skills.append({
                        'name': meta.get('name', skill_dir.name),
                        'description': meta.get('description', ''),
                        'version': meta.get('version', '1.0'),
                        'agent': meta.get('agent', ''),
                        'tags': meta.get('tags', []),
                    })
        return {'count': len(skills), 'skills': skills}

    @mcp.tool()
    def skills_read(skill_name: str) -> dict:
        """Read the full SKILL.md content for a named skill.
        skill_name: exact name of the skill (e.g. 'morning-briefing', 'health-check', 'code-review')"""
        for base in ['active', 'proposed']:
            skill_file = Path(SKILLS_BASE) / base / skill_name / 'SKILL.md'
            if skill_file.exists():
                content = skill_file.read_text()
                meta, body = _parse_frontmatter(content)
                return {
                    'name': skill_name,
                    'status': base,
                    'meta': meta,
                    'body': body,
                    'raw': content,
                }
        return {'error': f'Skill {skill_name!r} not found. Use skills_list() to see available skills.'}

    @mcp.tool()
    def skills_create(
        skill_name: str,
        description: str,
        procedure: str,
        agent: str = 'lumina',
        tags: str = '',
        proposed: bool = True,
    ) -> dict:
        """Create a new skill in agentskills.io format.
        skill_name: directory name (kebab-case, e.g. 'my-skill')
        description: one-line description
        procedure: markdown body describing the procedure
        agent: which agent owns this skill
        tags: comma-separated tags
        proposed: if True (default), creates in proposed/ for review. False creates directly in active/."""
        import yaml
        tag_list = [t.strip() for t in tags.split(',') if t.strip()]
        meta = {
            'name': skill_name,
            'description': description,
            'version': '1.0',
            'agent': agent,
            'license': 'MIT',
            'tags': tag_list,
        }
        content = f"---\n{yaml.dump(meta, default_flow_style=False)}---\n\n# {skill_name}\n\n{procedure}"

        status = 'proposed' if proposed else 'active'
        skill_dir = Path(SKILLS_BASE) / status / skill_name
        skill_dir.mkdir(parents=True, exist_ok=True)
        (skill_dir / 'SKILL.md').write_text(content)
        return {'status': 'created', 'skill': skill_name, 'location': status, 'path': str(skill_dir / 'SKILL.md')}
