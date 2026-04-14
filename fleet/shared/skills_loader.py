# /opt/lumina-fleet/shared/skills_loader.py
import os, yaml, re
from pathlib import Path

SKILLS_DIR = Path('/opt/lumina-fleet/skills/active')

def _parse_frontmatter(content: str) -> tuple[dict, str]:
    """Extract YAML frontmatter from SKILL.md content."""
    m = re.match(r'^---\n(.*?)\n---\n(.*)', content, re.DOTALL)
    if not m:
        return {}, content
    try:
        meta = yaml.safe_load(m.group(1))
        return meta or {}, m.group(2).strip()
    except Exception:
        return {}, content

def list_skills() -> list[dict]:
    """Return all available skills with metadata."""
    skills = []
    if not SKILLS_DIR.exists():
        return skills
    for skill_dir in sorted(SKILLS_DIR.iterdir()):
        skill_file = skill_dir / 'SKILL.md'
        if skill_dir.is_dir() and skill_file.exists():
            content = skill_file.read_text()
            meta, body = _parse_frontmatter(content)
            skills.append({
                'name': meta.get('name', skill_dir.name),
                'description': meta.get('description', ''),
                'version': meta.get('version', '1.0'),
                'author': meta.get('author', ''),
                'license': meta.get('license', 'MIT'),
                'path': str(skill_file),
                'preview': body[:200] if body else '',
            })
    return skills

def read_skill(name: str) -> dict | None:
    """Read full SKILL.md content for a named skill."""
    skill_dir = SKILLS_DIR / name
    skill_file = skill_dir / 'SKILL.md'
    if not skill_file.exists():
        return None
    content = skill_file.read_text()
    meta, body = _parse_frontmatter(content)
    return {'name': name, 'meta': meta, 'body': body, 'raw': content}

def write_skill(name: str, content: str, proposed: bool = False) -> bool:
    """Write a new skill SKILL.md. proposed=True puts it in proposed/ dir."""
    base = SKILLS_DIR.parent / ('proposed' if proposed else 'active')
    skill_dir = base / name
    skill_dir.mkdir(parents=True, exist_ok=True)
    (skill_dir / 'SKILL.md').write_text(content)
    return True

if __name__ == '__main__':
    skills = list_skills()
    print(f'Found {len(skills)} skills:')
    for s in skills:
        print(f'  {s["name"]}: {s["description"]}')
