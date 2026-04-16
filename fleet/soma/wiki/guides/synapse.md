# Synapse: Spontaneous Conversation Subsystem

Synapse is Lumina's initiative engine — it allows Lumina to reach out proactively, without waiting for the operator to send a message. Instead of being purely reactive (respond to input), Lumina monitors context and fires relevant observations, reminders, or ideas at appropriate moments.

**Status**: Designed (SY.1–SY.10). Implementation pending.

---

## The Problem Synapse Solves

Without Synapse, Lumina is a request-response system. Operator asks, Lumina answers. The limitation: The operator has to think of the question. He has to know what to ask and when to ask it.

Synapse inverts this for bounded categories of situation:
- A meeting ends and Lumina notices the task list didn't update
- The morning briefing mentions a flight but calendar shows no travel buffer
- A Vector task has been idle for 6 hours — Lumina asks if it should be aborted
- The operator mentioned wanting to look at a budget item last week — it comes up in context

These are things a thoughtful assistant would notice. Synapse makes Lumina capable of noticing them without the operator having to prompt.

---

## Architecture

Synapse has three components:

### 1. Trigger Evaluation

A lightweight scheduler runs trigger evaluations on configurable intervals (typically 5–15 minutes). Each trigger is a Python callable that returns `None` (no message) or a message dict.

```python
# Example trigger structure
def check_idle_vector_tasks(context: dict) -> Optional[dict]:
    idle_tasks = [t for t in context['vector_tasks'] if t['idle_hours'] > 6]
    if not idle_tasks:
        return None
    return {
        'message': f"Vector task '{idle_tasks[0]['title']}' has been idle for {idle_tasks[0]['idle_hours']}h. Abort?",
        'priority': 'normal',
        'trigger_type': 'idle_task',
        'requires_response': True,
    }
```

### 2. Message Generation

When a trigger fires, Synapse generates the actual message using the **lowest sufficient inference level**:

| Trigger Type | Generation Method |
|-------------|------------------|
| Simple threshold alert | Template (no LLM) |
| Data-derived observation | Template + Python |
| Contextual observation | Haiku/Lumina Fast |
| Complex multi-source insight | Lumina (Sonnet) |
| Requires judgment | Obsidian Circle |

Most triggers should use templates. LLM generation is reserved for observations that genuinely require language synthesis.

### 3. Delivery

Synapse delivers messages to the operator via Matrix. Messages are rate-limited to prevent noise.

---

## Trigger Types

### Scheduled Triggers
Fire on a calendar or interval basis.

```yaml
triggers:
  - id: morning_reminder
    schedule: "08:30"
    condition: has_open_tasks
    template: morning_task_summary

  - id: evening_wrap
    schedule: "17:00"
    condition: tasks_completed_today > 0
    template: day_summary
```

### Event Triggers
Fire when a monitored state changes.

```yaml
triggers:
  - id: vector_idle
    event: vector_task_idle
    threshold_hours: 6
    message_type: action_required

  - id: budget_alert
    event: daily_cost_threshold
    threshold_usd: 2.50
    message_type: alert

  - id: secret_expiry
    event: secret_age_warn
    source: sentinel
    message_type: maintenance
```

### Context Triggers
Fire when Lumina's context contains a pattern that warrants surfacing.

```yaml
triggers:
  - id: travel_gap
    context_pattern: "calendar.has_flight AND NOT calendar.has_travel_buffer"
    cooldown_hours: 48
    message_type: suggestion

  - id: remembered_intent
    context_pattern: "engram.recent_mentions.age_days > 5"
    source: engram_recall
    message_type: nudge
```

---

## Configuration

Triggers are defined in `constellation.yaml` under `synapse.triggers`:

```yaml
synapse:
  enabled: true
  quiet_hours: "22:00-07:00"   # No messages during these hours
  max_per_hour: 3              # Rate limit
  delivery_channel: matrix      # "matrix" or "nexus"

  triggers:
    - id: vector_idle
      enabled: true
      ...
```

Synapse also respects **Pulse** for time-of-day context:
- Morning (before 12:00): Allow all trigger types
- Afternoon: Operational triggers only (no nudges)
- Evening (after 18:00): Urgent alerts only
- Night (after 22:00): Silent (respect quiet_hours)

---

## Feedback Loop

Synapse learns from the operator's responses:

- **Dismissed without action**: Message was noise. Lower trigger sensitivity or increase cooldown.
- **Acted on immediately**: Message was valuable. Maintain or increase frequency.
- **Explicitly disabled**: Remove trigger from active set (save in constellation.yaml).

Feedback is captured by Lumina's response handling, not by Synapse directly. Lumina calls `synapse_feedback(trigger_id, outcome)` after the operator's reaction is classified.

---

## Implementation Plan (SY.1–SY.10)

| Item | Description | File |
|------|-------------|------|
| SY.1 | Trigger registry + scheduler loop | `fleet/synapse/scheduler.py` |
| SY.2 | Built-in triggers: Vector idle, budget alert | `fleet/synapse/triggers/` |
| SY.3 | Template message generator | `fleet/synapse/generator.py` |
| SY.4 | Matrix delivery + rate limiter | `fleet/synapse/delivery.py` |
| SY.5 | Feedback capture API | `fleet/synapse/feedback.py` |
| SY.6 | Pulse integration (time-of-day gating) | `fleet/synapse/pulse_gate.py` |
| SY.7 | Engram recall triggers | `fleet/synapse/triggers/recall.py` |
| SY.8 | Soma admin panel (enable/disable triggers) | Soma `/synapse` page |
| SY.9 | Context trigger engine | `fleet/synapse/context_eval.py` |
| SY.10 | Docs + safety reference | Wiki |

---

## Related Docs

- [Synapse Safety Reference](../reference/synapse-safety.md)
- [Pulse Guide](./pulse.md)
- [Obsidian Circle](../architecture/obsidian-circle.md) — used for complex Synapse observations
