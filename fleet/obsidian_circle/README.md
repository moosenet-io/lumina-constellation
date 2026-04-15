# ✦ Obsidian Circle

> "Four models walk into a room. Only the best answer walks out."

**The Obsidian Circle** is Lumina's multi-model reasoning council — it convenes multiple AI models with distinct personas to deliberate on hard problems, then synthesizes their positions into a single calibrated recommendation.

## What it does

- Runs a structured **ReAct loop** for each council member (think → observe → form position)
- Supports **7 circle presets**: `quick`, `architecture`, `security`, `cost`, `research`, `full`, `custom`
- **7 built-in personas**: Architect, Skeptic, Pragmatist, Security Analyst, User Advocate, Cost Analyst, Devil's Advocate
- **Prism mode**: one model, multiple personas — same reasoning, lower cost
- **Session checkpointing**: resumes automatically after crashes; checkpoints in `engram/council-sessions/`
- **Confidence thresholds**: ≥0.80 → auto-act, 0.50–0.79 → ask operator, <0.50 → surface deliberation

## Key files

| File | Purpose |
|------|---------|
| `engine.py` | `convene()` — the core deliberation loop with budget enforcement |
| `presets.py` | 7 built-in circle presets + YAML CRUD for custom presets |
| `personas.py` | 7 built-in personas + custom persona management |
| `output.py` | JSON schema validation, confidence thresholds, formatting |
| `cli.py` | `lumina-council` CLI — `--circle`, `--prism`, `--budget`, `--json` flags |

## Talks to

- **LiteLLM** (via `LITELLM_URL`) — all model calls routed through the proxy
- **Engram** — checkpoints stored in `council-sessions/` namespace
- **Vector** (`council_gate.py`) — gate failure escalation after 3+ Calx failures
- **Vigil** (`council_prioritize.py`) — briefing section prioritization when alerts present
- **Sentinel** (`council_remediate.py`) — auto-remediation diagnosis with adversarial personas

## Configuration

```bash
LITELLM_URL=http://your-litellm-host:4000
LITELLM_MASTER_KEY=sk-...
FLEET_DIR=/opt/lumina-fleet
```

Custom presets and personas stored in `constellation.yaml` under `council.circles` and `council.personas`.

---

Part of [Lumina Constellation](../../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
