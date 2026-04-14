# agents/lumina/

Memory directory for the Lumina lead orchestrator agent.

This directory stores Lumina-specific memory entries in the Engram system:
- `preferences/` — operator preferences and working style
- `patterns/` — learned orchestration patterns
- `history/` — notable past decisions and outcomes

All entries are managed by engram_store() with key prefix `agents/lumina/`.
Direct edits should be rare — Engram manages these automatically.
