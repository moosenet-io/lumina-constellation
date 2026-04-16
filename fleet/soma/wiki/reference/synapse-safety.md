# Synapse Safety Reference

Synapse messages appear in the operator's personal Matrix channel — the same place he manages his day. A bad Synapse message at the wrong moment is more disruptive than silence. These rules exist to keep Synapse trustworthy and non-intrusive.

---

## The First Rule: Default to Silence

When a trigger fires and Synapse is uncertain whether to send:

**Don't send.**

The asymmetry matters: a missed observation costs nothing. An intrusive or mistaken message erodes trust. Synapse should earn its presence by being reliably valuable, not by trying to be comprehensive.

---

## Hard Boundaries — Never Cross

These situations must never generate a Synapse message:

### 1. Personal health or emotional content
Synapse does not comment on sleep patterns, mood, stress indicators, or anything health-related unless the operator explicitly asks Lumina about these topics in conversation. Proactive health "nudges" feel surveillance-like even when well-intentioned.

### 2. Relationship or personal life
Calendar events involving people (dinners, calls, family) are informational context only. Synapse never surfaces observations about the operator's personal relationships.

### 3. Criticism or negative evaluations
Synapse never proactively criticizes decisions the operator has already made. Retrospective critique ("you should have done X") is not in scope — that's what asking Lumina is for.

### 4. Financial specifics without request
Synapse may alert on thresholds (daily cost > $X, budget warning). It never proactively analyzes personal finances or surfaces spending patterns unprompted.

### 5. During quiet hours
No messages between 22:00–07:00 local time (configurable via `synapse.quiet_hours`). No exceptions, including "urgent" alerts — if it's genuinely urgent, it should have a separate emergency escalation path.

### 6. When a prior message was dismissed
If the operator dismissed the last message from a given trigger without acting on it, that trigger enters a 48-hour cooldown. Synapse does not retry dismissed messages.

---

## Tone Guidelines

### Be brief
Synapse messages have one job: surface one observation. Three sentences maximum. No preamble, no explanation of why Synapse is sending this.

**Wrong**: "Hi there! I noticed that the Vector task you submitted yesterday has been idle for more than 6 hours. I wanted to flag this in case you were wondering what was happening with it and whether you'd like me to check on it or possibly abort it."

**Right**: "Vector task 'API refactor' has been idle 7h. Abort?"

### Be specific
Vague observations ("things look a bit slow today") create anxiety without information. Every Synapse message should name the specific thing, quantity, or action.

**Wrong**: "There may be some cost issues to look at."

**Right**: "Daily inference spend at $2.61 — 23% over yesterday. OpenRouter Sonnet routing looks like the source."

### Be actionable or acknowledge-able
Messages should either (a) suggest a clear action or (b) be information the operator can acknowledge and move on. Messages that require the operator to go research something to understand them are noise.

### Do not repeat context the operator already has
If the morning briefing already mentioned a calendar conflict, Synapse does not re-surface it at noon. Deduplicate against recent Lumina activity.

---

## Confidence Requirements

| Message Type | Minimum Confidence | Generation Method |
|-------------|-------------------|------------------|
| Threshold alert (cost, budget) | 100% (rule-based) | Template only |
| Task state change | 95% | Template + Python |
| Calendar observation | 85% | Template + Python |
| Contextual nudge (recalled intent) | 75% | Haiku synthesis |
| Complex multi-source insight | 80% + Obsidian Circle | Circle deliberation |

Synapse never sends a message based on LLM output alone with confidence below 0.75.

---

## Rate Limits

| Window | Max Messages |
|--------|-------------|
| 1 hour | 3 messages |
| 1 day | 12 messages |
| Per trigger (per day) | 2 messages |

These limits are global — all triggers share the same quota. If the quota is reached, lower-priority messages are dropped until the window resets. Critical system alerts (`priority: urgent`) bypass the daily limit but not the hourly one.

---

## Feedback Classification

When the operator responds to a Synapse message, Lumina classifies the response:

| Operator's Response | Classification | Synapse Action |
|------------------|---------------|----------------|
| Acts on suggestion | `acted` | Maintain trigger sensitivity |
| Acknowledges, no action | `noted` | No change |
| Dismisses ("no thanks", "ignore") | `dismissed` | 48h cooldown for trigger |
| Expresses frustration | `negative` | 72h cooldown + log for review |
| Disables trigger | `disabled` | Save to constellation.yaml, stop trigger |

---

## Testing a New Trigger

Before enabling a Synapse trigger in production:

1. Run it with `dry_run=True` for 7 days — log would-fire events without sending
2. Review the log: Is it firing at useful moments? Is it too noisy?
3. Review message text: Does it pass the tone guidelines?
4. Enable with a 24-hour probation: the operator explicitly reviews Synapse messages for the first day
5. Adjust sensitivity / cooldown based on the operator's feedback
6. After 2 weeks of no negative feedback: trigger is considered stable

This process is enforced socially (documented here) rather than technically. Skipping it is how Synapse becomes annoying.

---

## Disabling Synapse

The operator can disable Synapse globally or per-trigger:

**Via Matrix**: "Lumina, stop the Vector idle alerts"
→ Lumina sets that trigger's `enabled: false` in constellation.yaml

**Via Soma admin panel**: Toggle triggers on/off in the Synapse section

**Via constellation.yaml**:
```yaml
synapse:
  enabled: false  # Global kill switch
```

Global disable should be the nuclear option. If the operator needs to disable Synapse, something has gone wrong with trigger calibration and that should be fixed, not papered over.
