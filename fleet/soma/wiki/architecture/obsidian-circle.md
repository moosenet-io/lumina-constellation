# The Obsidian Circle

The Obsidian Circle is Lumina's multi-model deliberation engine. When a decision is too consequential, ambiguous, or architecturally loaded for a single inference pass, Lumina convenes the Circle — a structured council of AI personas that reason independently, then synthesize into a single recommendation with a calibrated confidence score.

## When to Use It

The Circle has real cost. Before calling `convene()`, apply the inference de-bloating rules:

| Situation | Use Instead |
|-----------|-------------|
| Simple factual lookup | Python or template |
| One-step classification | Qwen local (Lumina Fast) |
| Single-source synthesis | Claude Haiku (Lumina Fast) |
| Complex multi-source judgment | Obsidian Circle (correct use) |
| Architectural decisions | Obsidian Circle `architecture` preset |
| Security triage | Obsidian Circle `security` preset |

The Circle is the right tool when:
- Multiple valid perspectives exist and you need them weighed
- Confidence needs to be explicitly calibrated (not just "probably right")
- A wrong decision has meaningful blast radius
- You need an audit trail of how a conclusion was reached

---

## How `convene()` Works

```python
from obsidian_circle import convene

result = convene(
    question="Should we migrate Nexus to async Postgres?",
    circle="architecture",    # preset name
    budget=0.50,              # max USD
    mode="multi",             # "multi" or "prism"
    output_schema={...},      # optional JSON schema for structured output
    resume=True,              # resume from checkpoint if crash-interrupted
)
```

### Execution Flow

1. **Resolve preset** — Load member list, synthesis model, and default schema
2. **Checkpoint check** — If `resume=True`, check for a prior incomplete session with the same question. Resume from the last completed member.
3. **Member loop** — For each council member:
   - Inject persona system prompt (their unique reasoning angle)
   - Share tool results from prior members (broadcast pattern)
   - Run ReAct prompt: `<reasoning>` → `<position>` → `<confidence>`
   - Save checkpoint after every member completes
4. **Synthesis** — Mr. Wizard reads all positions weighted by confidence and produces a final recommendation
5. **Validate output** — If `output_schema` provided, validate and coerce the JSON result
6. **Confidence gating** — Apply threshold to determine action guidance
7. **Return** — Full result dict including positions, synthesis, cost, and session metadata

### Return Value

```python
{
    "result":          "...",      # synthesis text or validated JSON
    "confidence":      0.82,       # 0.0–1.0
    "action":          "auto_act", # see Confidence Thresholds below
    "positions":       [...],      # each member's position + reasoning
    "synthesis":       "...",      # raw Mr. Wizard output
    "cost_usd":        0.043,
    "elapsed_s":       12.4,
    "circle":          "architecture",
    "mode":            "multi",
    "member_count":    3,
    "session_id":      "a3f2b1c4d5e6f7g8",
    "resumed":         false,
    "deliberation_log": {...}      # full audit trail
}
```

---

## Circle Presets

Presets control who participates and how much compute is allocated.

| Preset | Members | Cost Est. | Best For |
|--------|---------|-----------|----------|
| `quick` | 1 (Pragmatist) | ~$0.005 | Fast judgment, routing decisions |
| `architecture` | 3 (Architect + Skeptic + Pragmatist) | ~$0.03 | System design choices |
| `security` | 2 (Security + Devil's Advocate) | ~$0.02 | Threat modeling, remediation decisions |
| `cost` | 2 (Cost + Pragmatist) | ~$0.02 | Inference allocation, optimization |
| `research` | 2 (Architect + User) | ~$0.02 | Evaluating proposals from operator |
| `full` | 7 (all personas) | ~$0.12 | High-stakes architectural decisions |
| `custom` | Configurable | Variable | Custom one-off configurations |

### Custom Presets

Store custom presets in `constellation.yaml` under `council.circles`:

```yaml
council:
  circles:
    my_preset:
      members:
        - id: wizard
          model: "Lumina"
          persona_id: pragmatist
          max_tokens: 600
      synthesis_model: "Lumina"
      description: "Quick pragmatist-only check"
```

Or via the API:
```python
from obsidian_circle.presets import save_custom_preset
save_custom_preset("my_preset", {...})
```

---

## Personas (Prism System)

Each persona has a distinct system prompt that shapes reasoning direction.

| ID | Name | Reasoning Angle |
|----|------|----------------|
| `architect` | The Architect | Systems design, scalability, component boundaries |
| `skeptic` | The Skeptic | Failure modes, assumptions, what can go wrong |
| `pragmatist` | The Pragmatist | Operational reality, deployment, what ships |
| `security` | The Security Analyst | Threat modeling, trust boundaries, blast radius |
| `user` | The User Advocate | Peter's perspective — non-technical, voice-based |
| `cost` | The Cost Analyst | Inference cost chain, de-bloating opportunities |
| `devils_advocate` | Devil's Advocate | Argue the strongest case against the proposal |

### Custom Personas

```python
from obsidian_circle.personas import save_persona

save_persona(
    id="domain_expert",
    name="The Domain Expert",
    description="Deep Lumina system knowledge",
    system_prompt="You are a senior engineer who has read every line of the Lumina codebase..."
)
```

Custom personas are stored in `constellation.yaml` under `council.personas`.

---

## Prism Mode vs Multi Mode

| | Multi Mode | Prism Mode |
|--|-----------|-----------|
| Models | Different models per member | One model, all members |
| Personas | Each member's assigned persona | Same — different personas |
| Cost | Higher (multiple models) | Lower (one model, repeated) |
| Diversity | Higher (true model disagreement) | Lower (one model's biases) |
| Use When | Genuine uncertainty | Exploring angles of a known problem |

**Prism mode** is useful when you trust a single model but want structured multi-perspective analysis. It's cheaper and faster, trades true model diversity for cost.

```bash
python3 cli.py 'Should we use Redis or Postgres for session storage?' --circle architecture --prism
```

---

## Confidence Thresholds

After synthesis, the engine evaluates `confidence` and sets `action`:

| Confidence | Action | Meaning |
|-----------|--------|---------|
| ≥ 0.80 | `auto_act` | High agreement — execute without operator confirmation |
| 0.50–0.79 | `ask_operator` | Moderate agreement — surface to Peter for approval |
| < 0.50 | `surface_deliberation` | Low confidence — show full deliberation, do not act |

These thresholds apply in all automatic integrations (Vector gate escalation, Sentinel remediation, Vigil prioritization).

---

## Session Checkpointing

Long deliberations (multi-member, high-budget) can be interrupted by crashes, timeouts, or container restarts. The checkpoint system ensures partial work is not lost.

- After every member completes, state is saved to `engram/council-sessions/<session_id>.json`
- Session IDs are derived from a hash of `(circle, question)` — deterministic per unique deliberation
- On `convene()` restart, a matching checkpoint is loaded and the member loop resumes from the next incomplete member
- Checkpoints expire after 24 hours and are automatically removed

To force a fresh deliberation (discard checkpoint):
```python
convene(question, circle='full', resume=False)
```

Or from CLI:
```bash
python3 cli.py 'my question' --circle full --no-resume
```

---

## Structured Output

When `output_schema` is provided, synthesis output is validated against a JSON schema with type coercion:

```python
result = convene(
    question="Diagnose this alert",
    circle="security",
    output_schema={
        "type": "object",
        "properties": {
            "severity": {"type": "string", "enum": ["low", "medium", "high", "critical"]},
            "can_auto_remediate": {"type": "boolean"},
            "remediation_command": {"type": "string"},
            "confidence": {"type": "number"}
        },
        "required": ["severity", "can_auto_remediate", "confidence"]
    }
)
structured = result["result"]  # validated dict
```

If JSON parsing fails, the engine falls back to returning the raw synthesis text with `parse_error: true` in the synthesis dict.

---

## Cost Model

Rough estimates per deliberation:

| Circle | Typical Cost | Tokens In | Tokens Out |
|--------|-------------|----------|-----------|
| quick | $0.003–$0.008 | ~800 | ~400 |
| architecture | $0.020–$0.045 | ~2400 | ~1200 |
| security | $0.015–$0.035 | ~1600 | ~800 |
| full | $0.08–$0.15 | ~5600 | ~2800 |

Budget enforcement is hard — if `total_cost >= budget`, remaining members are skipped with a `budget_exhausted` error entry. Set budget conservatively; the `quick` preset fits within $0.01 for most questions.

---

## Module Integrations

The Circle is integrated into three fleet modules:

### Vector — Gate Escalation (`fleet/vector/council_gate.py`)
When a Vector task accumulates ≥ 3 Calx gate failures, the Circle deliberates:
- Uses `quick` preset
- Output schema: `{recommendation, reasoning, suggested_subtasks, confidence}`
- Recommendations: `split` / `reduce_scope` / `escalate` / `retry`

### Vigil — Briefing Prioritization (`fleet/vigil/council_prioritize.py`)
When alerts are present or section count > 4:
- Uses `quick` preset, $0.02 budget
- Output schema: `{priority_sections, tone, lead_with, skip_sections}`

### Sentinel — Auto-Remediation (`fleet/sentinel/council_remediate.py`)
For potentially dangerous remediation commands:
- Uses `security` preset (adversarial + skeptical personas)
- Hard safety gates: `ironclaw`, `matrix`, `postgres`, `llm_cost` are never auto-remediated
- Requires ≥ 0.85 confidence to execute any command

---

## CLI Reference

```bash
# Basic deliberation
python3 fleet/obsidian_circle/cli.py 'Should we add a rate limiter to Nexus?'

# Architecture preset with higher budget
python3 fleet/obsidian_circle/cli.py 'Design the Synapse event pipeline' \
  --circle architecture --budget 1.00

# Prism mode (one model, multiple personas)
python3 fleet/obsidian_circle/cli.py 'Evaluate our key rotation policy' \
  --circle security --prism

# JSON output for scripting
python3 fleet/obsidian_circle/cli.py 'Rate this approach' --json | jq .confidence

# List presets and personas
python3 fleet/obsidian_circle/cli.py --list-presets
python3 fleet/obsidian_circle/cli.py --list-personas

# Exit codes: 0 = auto_act/ask_operator, 2 = surface_deliberation
```

---

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `LITELLM_URL` | (required) | LiteLLM proxy endpoint |
| `LITELLM_MASTER_KEY` | — | Auth key for LiteLLM |
| `FLEET_DIR` | `/opt/lumina-fleet` | Root of fleet directory |
| `CONSTELLATION_PATH` | `$FLEET_DIR/constellation.yaml` | Constellation config |
| `PULSE_MARKERS_PATH` | `$FLEET_DIR/pulse/markers.json` | Pulse marker store |
