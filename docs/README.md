# Docs — Help System

This directory contains the built-in documentation for Lumina Constellation. Soma serves these docs at runtime via the `/api/docs/` endpoint.

---

## Structure

```
docs/
├── getting-started/
│   └── installation.md       # First-run setup guide
├── modules/                  # One file per module
│   ├── nexus.md
│   ├── vigil.md
│   ├── sentinel.md
│   └── ...
├── guides/                   # How-to guides
│   ├── adding-an-agent.md
│   ├── managing-costs.md
│   └── connecting-matrix.md
├── reference/                # API and config reference
│   ├── agent-yaml-format.md
│   └── env-variables.md
└── index.md                  # Doc index (auto-generated)
```

---

## Key Docs

| Document | What it covers |
|----------|---------------|
| [getting-started/installation.md](getting-started/installation.md) | First-run setup, Docker deploy, Soma wizard walkthrough |
| guides/adding-an-agent.md | How to create a new agent with `.agent.yaml` |
| guides/managing-costs.md | Inference de-bloating, cost tracking with Myelin, budget alerts |
| guides/connecting-matrix.md | Matrix server setup, bot account, room configuration |
| reference/agent-yaml-format.md | All `.agent.yaml` fields documented |

---

## How Soma Serves Docs

Soma exposes a read-only API for the help system:

```
GET  /api/docs/{path}           # Returns doc content as markdown
GET  /api/docs/search?q=...     # Full-text search across all docs
GET  /api/docs/index            # Returns the full doc index
```

Docs are rendered in the Soma web UI and can be queried by Lumina during a conversation. For example, "how do I add a new agent?" causes Lumina to fetch the relevant guide and summarize it rather than hallucinating.

---

## Adding Documentation

1. Create a `.md` file in the appropriate subdirectory.
2. Use standard markdown. Frontmatter is optional but supported:
   ```yaml
   ---
   title: "My Guide"
   module: myelin
   tags: [costs, inference, governance]
   ---
   ```
3. Regenerate the index:
   ```bash
   python3 /opt/lumina-fleet/shared/docs_generator.py
   ```
4. The doc is immediately available via Soma's API — no restart needed.

---

## Auto-Generated Reference

The docs generator reads all `.agent.yaml` files and module docstrings to produce the reference section automatically. Run it after adding or modifying agents or tool modules:

```bash
python3 /opt/lumina-fleet/shared/docs_generator.py
```

---

## History / Lineage

The built-in help system was designed in session 11 as part of the NPC Feature 3 spec (documentation infrastructure). Before session 11, module documentation was scattered across individual README files with no runtime discoverability. Soma's `/wiki` page and `/api/docs/` endpoint were added simultaneously so that Lumina can answer "how do I do X?" by fetching relevant docs rather than hallucinating.

The auto-generated reference section (`docs_generator.py`) was added to prevent agent documentation from going stale after refactors — it rebuilds from the live `.agent.yaml` files and module docstrings on demand.

## Credits

- Documentation structure — influenced by [NPCSH docs](https://npc-shell.readthedocs.io/) module documentation patterns
- Markdown frontmatter parsing — Python `markdown` and `python-frontmatter` libraries

## Related

- [fleet/shared/docs_generator.py](../fleet/shared/docs_generator.py) — Doc index generator
- [agents/README.md](../agents/README.md) — Agent definition format
- [Root README](../README.md) — System overview
