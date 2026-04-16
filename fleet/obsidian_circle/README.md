# ✦ Obsidian Circle

> Four models walk into a room. Only the best answer walks out.

**Obsidian Circle** is the multi-model reasoning council that deliberates on complex problems through a consensus-driven framework.

## What it does

- Convenes multiple AI models (Ollama, OpenRouter, Anthropic) to analyze a single prompt.
- Manages session checkpointing and context sharing between council members.
- Uses specialized personas to ensure diverse perspectives during deliberation.
- Synthesizes a final recommendation using the strongest available model (Mr. Wizard).
- Validates output confidence and ensures reasoning transparency.

## Key files

| File | Purpose |
|------|---------|
| `engine.py` | Council orchestration and deliberation logic |
| `presets.py` | Configured council configurations (e.g., "Code Review", "Creative") |
| `personas.py` | Defined archetypes for council members |
| `output.py` | Final synthesis and validation of council decisions |

## Talks to

- **[Mr. Wizard](../wizard/)** — Acts as the synthesis engine for final decisions.
- **[Myelin](../myelin/)** — Validates budget availability before convening the council.
- **[Engram](../engram/)** — Retrieves relevant context and stores council outcomes.

## Configuration

Presets and member models defined in `presets.py`. Local execution requires a running LiteLLM/Ollama stack.

---

Part of [Lumina Constellation](../../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
