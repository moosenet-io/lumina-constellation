#!/usr/bin/env python3
"""
migrate-from-openclaw.py — Lumina Constellation OpenClaw Migration Tool

Migrates data from an existing OpenClaw installation to Lumina Constellation.
Safe: reads source read-only. Dry-run by default.

Usage:
    python3 migrate-from-openclaw.py                      # dry-run, auto-detect ~/.openclaw
    python3 migrate-from-openclaw.py --execute             # execute migration
    python3 migrate-from-openclaw.py --source /path       # custom OpenClaw directory
    python3 migrate-from-openclaw.py --include-history     # also migrate conversation history

Requires: Python 3.10+, no external dependencies.

Data mapping:
    ~/.openclaw/SOUL.md      → /root/.ironclaw/LUMINA.md      (personality/system prompt)
    ~/.openclaw/USER.md      → /root/.ironclaw/USER.md         (user profile — direct copy)
    ~/.openclaw/MEMORY.md    → Engram (seeded via engram_store)
    ~/.openclaw/memory/      → Engram namespaces
    ~/.openclaw/.env         → Printed as Infisical import instructions (never stored)
    ~/.openclaw/skills/      → /opt/lumina-fleet/skills/active/ (agentskills.io format)
    ~/.openclaw/history/     → Engram journals (opt-in with --include-history)
"""

import argparse
import json
import os
import re
import shutil
import sys
from pathlib import Path
from datetime import datetime

VERSION = "1.0.0"
REPORT_PATH = Path("migration-report.md")

# Target paths
IRONCLAW_DIR = Path(os.environ.get("IRONCLAW_DIR", Path.home() / ".ironclaw"))
SKILLS_ACTIVE = Path(os.environ.get("SKILLS_DIR", "/opt/lumina-fleet/skills/active"))
ENGRAM_STORE_CMD = os.environ.get("ENGRAM_CMD", "python3 /opt/lumina-fleet/engram/engram.py store")


class MigrationResult:
    def __init__(self):
        self.migrated = []
        self.skipped = []
        self.warnings = []
        self.errors = []
        self.api_keys_found = []

    def add(self, category, item, note=""):
        self.migrated.append((category, item, note))

    def skip(self, category, item, reason):
        self.skipped.append((category, item, reason))

    def warn(self, msg):
        self.warnings.append(msg)
        print(f"  ⚠  {msg}")

    def error(self, msg):
        self.errors.append(msg)
        print(f"  ✗  {msg}")


def detect_openclaw(source: Path | None) -> Path | None:
    """Find OpenClaw data directory."""
    candidates = [
        source,
        Path.home() / ".openclaw",
        Path.home() / ".claude",
        Path("/root/.openclaw"),
    ]
    for p in candidates:
        if p and p.exists() and (p / "SOUL.md").exists():
            return p
    return None


def migrate_soul(src: Path, result: MigrationResult, dry_run: bool):
    """SOUL.md → LUMINA.md (personality / system prompt)."""
    soul = src / "SOUL.md"
    if not soul.exists():
        result.skip("personality", "SOUL.md", "not found")
        return

    target = IRONCLAW_DIR / "LUMINA.md"
    size = soul.stat().st_size

    if dry_run:
        print(f"  → SOUL.md ({size}B) would copy to {target}")
        result.add("personality", "SOUL.md", f"→ {target} (dry-run)")
        return

    IRONCLAW_DIR.mkdir(parents=True, exist_ok=True)
    if target.exists():
        backup = target.with_suffix(f".bak.{datetime.now().strftime('%Y%m%d%H%M%S')}")
        shutil.copy2(target, backup)
        print(f"  Backed up existing LUMINA.md → {backup.name}")

    shutil.copy2(soul, target)
    print(f"  ✓ SOUL.md → {target}")
    result.add("personality", "SOUL.md", f"→ {target}")


def migrate_user(src: Path, result: MigrationResult, dry_run: bool):
    """USER.md → USER.md (user profile — direct copy)."""
    user = src / "USER.md"
    if not user.exists():
        result.skip("user-profile", "USER.md", "not found")
        return

    target = IRONCLAW_DIR / "USER.md"
    size = user.stat().st_size

    if dry_run:
        print(f"  → USER.md ({size}B) would copy to {target}")
        result.add("user-profile", "USER.md", f"→ {target} (dry-run)")
        return

    IRONCLAW_DIR.mkdir(parents=True, exist_ok=True)
    shutil.copy2(user, target)
    print(f"  ✓ USER.md → {target}")
    result.add("user-profile", "USER.md", f"→ {target}")


def migrate_memory(src: Path, result: MigrationResult, dry_run: bool):
    """MEMORY.md and memory/ → Engram namespace entries."""
    memory_file = src / "MEMORY.md"
    memory_dir = src / "memory"

    facts = []

    # Parse MEMORY.md into discrete facts
    if memory_file.exists():
        content = memory_file.read_text()
        # Extract sections as separate facts
        sections = re.split(r'^##\s+', content, flags=re.MULTILINE)
        for section in sections[1:]:  # skip preamble
            lines = section.strip().splitlines()
            if lines:
                title = lines[0].strip()
                body = "\n".join(lines[1:]).strip()
                if body:
                    facts.append((f"openclaw/memory/{title.lower().replace(' ', '-')}", body))

    # Walk memory/ subdirectory
    if memory_dir.exists():
        for md_file in sorted(memory_dir.rglob("*.md")):
            key = "openclaw/" + str(md_file.relative_to(memory_dir)).replace(".md", "").replace("/", "/")
            content = md_file.read_text().strip()
            if content:
                facts.append((key, content))

    if not facts:
        result.skip("memory", "MEMORY.md + memory/", "no parseable content found")
        return

    print(f"  Found {len(facts)} memory facts to import")

    if dry_run:
        for key, content in facts[:3]:
            print(f"    → engram_store({key!r}, {content[:60]!r}...)")
        if len(facts) > 3:
            print(f"    → ... and {len(facts) - 3} more")
        result.add("memory", f"{len(facts)} facts", "would import to Engram (dry-run)")
        return

    # Write Engram import script
    import_script = IRONCLAW_DIR / "engram_import.json"
    import_script.write_text(json.dumps(facts, indent=2))
    print(f"  ✓ Memory facts saved to {import_script}")
    print(f"    Run: python3 /opt/lumina-fleet/engram/engram.py batch-store {import_script}")
    result.add("memory", f"{len(facts)} facts", f"→ saved to {import_script}")
    result.warn("Memory facts require manual Engram import. See migration-report.md for command.")


def migrate_skills(src: Path, result: MigrationResult, dry_run: bool):
    """skills/ → /opt/lumina-fleet/skills/active/ (agentskills.io format)."""
    skills_src = src / "skills"
    if not skills_src.exists():
        result.skip("skills", "skills/", "directory not found")
        return

    skill_dirs = [d for d in skills_src.iterdir() if d.is_dir() and (d / "SKILL.md").exists()]
    if not skill_dirs:
        result.skip("skills", "skills/", "no agentskills.io SKILL.md files found")
        return

    print(f"  Found {len(skill_dirs)} skills to migrate")
    for skill_dir in sorted(skill_dirs):
        target = SKILLS_ACTIVE / skill_dir.name
        if dry_run:
            print(f"    → {skill_dir.name}/ would copy to {target}")
            result.add("skills", skill_dir.name, f"→ {target} (dry-run)")
            continue

        if target.exists():
            result.warn(f"Skill {skill_dir.name} already exists — skipping to avoid overwrite")
            result.skip("skills", skill_dir.name, "already exists in active/")
            continue

        SKILLS_ACTIVE.mkdir(parents=True, exist_ok=True)
        shutil.copytree(skill_dir, target)
        print(f"    ✓ {skill_dir.name}/ → {target}")
        result.add("skills", skill_dir.name, f"→ {target}")


def migrate_env(src: Path, result: MigrationResult, dry_run: bool):
    """Parse .env and print Infisical import instructions. Never write keys to files."""
    env_file = src / ".env"
    if not env_file.exists():
        result.skip("credentials", ".env", "not found")
        return

    lines = env_file.read_text().splitlines()
    keys = []
    for line in lines:
        if "=" in line and not line.startswith("#"):
            k = line.split("=")[0].strip()
            # Flag likely secrets
            if any(word in k.upper() for word in ["KEY", "TOKEN", "SECRET", "PASSWORD", "PASS"]):
                keys.append(k)

    if not keys:
        result.skip("credentials", ".env", "no secret keys found")
        return

    result.warn(
        f"Found {len(keys)} API keys in OpenClaw .env: {', '.join(keys)}. "
        "These are printed to console only — NOT written to any file. "
        "Add them to Infisical manually."
    )

    print(f"\n  API keys found in OpenClaw .env (copy to Infisical):")
    for line in lines:
        if "=" in line and not line.startswith("#"):
            k = line.split("=")[0].strip()
            if any(word in k.upper() for word in ["KEY", "TOKEN", "SECRET", "PASSWORD", "PASS"]):
                # Print key name only — not value
                print(f"    {k} = <value hidden — check source .env manually>")

    result.add("credentials", f"{len(keys)} API keys", "printed to console — add to Infisical manually")


def migrate_history(src: Path, result: MigrationResult, dry_run: bool):
    """Conversation history → Engram journals (optional, large)."""
    history_dir = src / "history"
    if not history_dir.exists():
        result.skip("history", "history/", "not found")
        return

    files = list(history_dir.rglob("*.json")) + list(history_dir.rglob("*.jsonl"))
    total_size = sum(f.stat().st_size for f in files)

    result.warn(
        f"Found {len(files)} conversation history files ({total_size / 1024 / 1024:.1f}MB). "
        "History import is not yet implemented — use engram_journal() for key decisions only."
    )
    result.skip("history", f"{len(files)} files", "not implemented — use Engram journals manually")


def write_report(src: Path, result: MigrationResult, dry_run: bool, include_history: bool):
    """Write migration report markdown."""
    lines = [
        f"# OpenClaw Migration Report",
        f"",
        f"**Date:** {datetime.now().strftime('%Y-%m-%d %H:%M')}",
        f"**Source:** `{src}`",
        f"**Mode:** {'DRY RUN (no changes made)' if dry_run else 'EXECUTED'}",
        f"",
        f"## Migrated",
        "",
    ]
    if result.migrated:
        lines.append("| Category | Item | Notes |")
        lines.append("|----------|------|-------|")
        for cat, item, note in result.migrated:
            lines.append(f"| {cat} | {item} | {note} |")
    else:
        lines.append("_Nothing migrated._")

    lines += ["", "## Skipped", ""]
    if result.skipped:
        lines.append("| Category | Item | Reason |")
        lines.append("|----------|------|--------|")
        for cat, item, reason in result.skipped:
            lines.append(f"| {cat} | {item} | {reason} |")
    else:
        lines.append("_Nothing skipped._")

    if result.warnings:
        lines += ["", "## Warnings", ""]
        for w in result.warnings:
            lines.append(f"- {w}")

    if result.errors:
        lines += ["", "## Errors", ""]
        for e in result.errors:
            lines.append(f"- {e}")

    lines += [
        "", "## Next Steps", "",
        "1. Review this report and verify migrated files",
        f"2. If memory facts were extracted, run the Engram batch import",
        "3. Add API keys to Infisical (see credential section above)",
        "4. Run `deploy/ironclaw-setup.sh` to seed IronClaw vault from env vars",
        "5. Start IronClaw and verify it loads your personality and user profile",
        "",
        "## What was NOT migrated",
        "",
        "- OpenClaw gateway process state (stateless — IronClaw restarts cleanly)",
        "- OpenClaw conversation history (use --include-history to opt in, then import manually)",
        "- OpenClaw plugin code (re-install as Lumina skills or MCP plugins)",
        "",
    ]

    REPORT_PATH.write_text("\n".join(lines))
    print(f"\n  Report written to {REPORT_PATH}")


def main():
    parser = argparse.ArgumentParser(
        description="Migrate OpenClaw data to Lumina Constellation",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    parser.add_argument("--source", type=Path, default=None, help="OpenClaw directory (default: ~/.openclaw)")
    parser.add_argument("--execute", action="store_true", help="Execute migration (default: dry-run)")
    parser.add_argument("--include-history", action="store_true", help="Include conversation history migration")
    parser.add_argument("--version", action="version", version=f"%(prog)s {VERSION}")
    args = parser.parse_args()

    dry_run = not args.execute

    print(f"Lumina OpenClaw Migration Tool v{VERSION}")
    print(f"Mode: {'DRY RUN' if dry_run else 'EXECUTE'}")
    print()

    # Detect source
    src = detect_openclaw(args.source)
    if not src:
        print("Error: OpenClaw directory not found.")
        print("Tried: ~/.openclaw, ~/.claude, /root/.openclaw")
        print("Use --source /path/to/openclaw to specify manually.")
        sys.exit(1)

    print(f"Source: {src}")
    print(f"Target IronClaw: {IRONCLAW_DIR}")
    print(f"Target Skills: {SKILLS_ACTIVE}")
    print()

    result = MigrationResult()

    print("[ Personality / System Prompt ]")
    migrate_soul(src, result, dry_run)

    print("\n[ User Profile ]")
    migrate_user(src, result, dry_run)

    print("\n[ Memory / Knowledge ]")
    migrate_memory(src, result, dry_run)

    print("\n[ Skills ]")
    migrate_skills(src, result, dry_run)

    print("\n[ Credentials ]")
    migrate_env(src, result, dry_run)

    if args.include_history:
        print("\n[ Conversation History ]")
        migrate_history(src, result, dry_run)

    print("\n[ Report ]")
    write_report(src, result, dry_run, args.include_history)

    print()
    print(f"Summary: {len(result.migrated)} migrated, {len(result.skipped)} skipped, "
          f"{len(result.warnings)} warnings, {len(result.errors)} errors")

    if dry_run:
        print()
        print("This was a DRY RUN. No changes were made.")
        print("Run with --execute to perform the migration.")

    return 0 if not result.errors else 1


if __name__ == "__main__":
    sys.exit(main())
