# Synapse — Memory-Driven Spontaneous Conversation

Synapse gives Lumina the ability to initiate conversation without waiting to be asked. It reads Engram, monitors temporal patterns, and sends messages through Matrix when it finds something genuinely worth saying.

**Deploys to:** `<fleet-host>` at `/opt/lumina-fleet/synapse/`
**Trigger:** systemd timer — every 30 minutes (configurable)
**Inference cost:** Near-zero. Trigger detection and relevance gate are pure Python. Only message composition touches a model (local Qwen, ~500 tokens).
**Default state:** OFF. Operator must explicitly enable from Soma → Config → Synapse.

Named for: the junction between neurons where signals fire spontaneously.

---

## What Synapse Does

Without Synapse, every agent in Lumina is reactive — it speaks only when spoken to. Synapse closes this gap:

1. **Detects triggers** — scans Engram for new facts, Zettelkasten hub nodes, temporal patterns, Sentinel events, Vector completions, and Nexus pending messages. Pure Python, $0.
2. **Relevance gate** — filters candidates by score threshold, quiet hours, rate limit, and topic blocklist. Pure Python, $0.
3. **Composes messages** — generates a natural, brief message (1–3 sentences) using a local model. ~$0.001 per message, or $0 with local Qwen.
4. **Sends via Matrix** — message delivered as Lumina. Source agent attributed: "Vigil noticed..." or "Vector just finished..."

---

## Architecture

- **Runs on:** `<fleet-host>` at `/opt/lumina-fleet/synapse/`
- **Dependencies:** Python 3.11+, Engram (local), local Ollama (Qwen for composition)
- **Connections:** Reads Engram (local DB); sends via Matrix through Lumina's channel; reads Sentinel, Vigil, Vector state files

---

## Three-Stage Pipeline

### Stage 1 — Trigger Detection (`synapse_scan.py`, Python, $0)

Checks all trigger sources every scan interval:

| Source | What it looks for |
|--------|------------------|
| Engram new facts | Facts stored in last 24h that link to operator interests |
| Engram hubs | Zettelkasten nodes with 3+ links the operator hasn't seen |
| Engram `needs_review` | Facts flagged as contradictions by memory evolution |
| Pulse temporal | Time since last interaction with a topic, day/time patterns |
| Sentinel | Resolved health issues worth mentioning |
| Vigil | News items matching interest keywords |
| Vector | Completed tasks not yet acknowledged |
| Nexus | Agent messages awaiting operator attention |

Output: candidate list with type, relevance score (0–1), and source data.

### Stage 2 — Relevance Gate (Python, $0)

Drops candidates that:
- Score below relevance threshold (default 0.6)
- Match the topic blocklist
- Were surfaced in the last 24h (no-repeat filter)
- Fall within quiet hours (default 10 PM – 7 AM)
- Would exceed the daily message cap (default 3)

### Stage 3 — Message Composition (local Qwen, ~$0)

For each approved trigger: compose a 1–3 sentence message that feels like a colleague mentioning something useful. Casual, not formal. Always attributes the source. Ends with an easy dismiss.

---

## Configuration

All settings in `constellation.yaml` under `synapse:`, editable from Soma → Config → Synapse tab.

| Setting | Default | Description |
|---------|---------|-------------|
| `enabled` | `false` | Master toggle. Must be explicitly enabled. |
| `scan_interval_minutes` | `30` | How often to check for triggers |
| `max_messages_per_day` | `3` | Hard cap on outbound messages |
| `relevance_threshold` | `0.6` | Minimum score to pass the gate |
| `quiet_hours_start` | `22:00` | No messages after this time |
| `quiet_hours_end` | `07:00` | No messages before this time |
| `channel` | `matrix` | Delivery channel |
| `strength` | `moderate` | `gentle` / `moderate` / `enthusiastic` |

### Strength Settings

| Strength | Threshold | Max/day | What it includes |
|----------|-----------|---------|-----------------|
| `gentle` | 0.8 | 1 | Task completions, critical health events only |
| `moderate` | 0.6 | 3 | Follow-ups, interest matches, weekly recaps |
| `enthusiastic` | 0.4 | 5 | Serendipitous connections, project ideas, idle check-ins |

---

## Safety Rules (Hard Limits, Not Configurable)

- Never initiate about health, medical, or mental health topics unless the operator explicitly boosts them
- Never create artificial urgency
- Never guilt-trip about inactivity
- Never reference activity in a way that feels surveilled
- Never send messages to household members about other members' activity
- Always attribute the source agent: "Vigil noticed..." not "I've been watching..."
- Quiet hours are absolute — no exceptions

---

## Feedback Loop

- 5 consecutive ignored messages → auto-reduce strength by one level
- 3 thumbs-down on a topic category → auto-add to topic blocklist
- Thumbs-up → +0.1 relevance bonus for that trigger type
- Weekly self-assessment (Python, no LLM) adjusts threshold based on response rate

---

## Files

| File | Purpose |
|------|---------|
| `synapse_scan.py` | Stage 1: trigger detection. Pure Python. |
| `synapse_gate.py` | Stage 2: relevance filtering. Pure Python. |
| `synapse_compose.py` | Stage 3: message composition via local model. |
| `synapse.service` | systemd service unit |
| `synapse.timer` | systemd timer — runs scan every 30 minutes |

---

## History / Lineage

Synapse was designed in Document 26 (April 2026) as the spontaneous conversation subsystem. Before Synapse, all Lumina agents were purely reactive — they only spoke when spoken to. Synapse closes the loop between memory storage and proactive communication, making the system feel less like a tool and more like a companion.

---

## Credits

- Zettelkasten linking model — influenced by [Niklas Luhmann's card index system](https://en.wikipedia.org/wiki/Zettelkasten) and the Obsidian note-linking community
- Spontaneous conversation design — inspired by proactive agent research in the LLM agent literature (2024-2025)
