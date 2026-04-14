# Vector — Standalone Mode Quick Start

Standalone mode runs Vector on any machine with Python 3.10+ and git. No Lumina infrastructure required — uses local SQLite, file-based memory, and stdout messaging.

## Requirements

- Python 3.10+
- Git configured with credentials
- LiteLLM-compatible API endpoint (or Anthropic API key)
- `pip3 install pyyaml`

## Setup (5 minutes)

```bash
# 1. Clone the repo
git clone http://<gitea-ip>:3000/moosenet/lumina-fleet.git
cd lumina-fleet/vector

# 2. Copy config
cp config/vector.yaml.example vector.yaml

# 3. Edit vector.yaml — set mode: standalone and your LLM endpoint
nano vector.yaml

# 4. Set your API key
export LITELLM_MASTER_KEY=sk-...  # or ANTHROPIC_API_KEY

# 5. Run a task
python3 vector.py run --task "Add a hello world test" --repo /path/to/your/repo
```

## vector.yaml (standalone)

```yaml
mode: standalone

llm:
  endpoint: http://<litellm-ip>:4000   # LiteLLM proxy
  api_key_env: LITELLM_MASTER_KEY
  model: claude-sonnet-4-6              # or claude-haiku-4-5 for cheaper runs

standalone:
  state_db: ./vector-state.db           # SQLite task state
  memory_dir: ./memory                  # Local knowledge files
  conventions: ./memory/conventions.md  # Coding conventions
  log_file: ./vector.log
  interactive: false                    # true = pause before each commit
  max_iterations: 10
  max_cost_per_run: 5.00
```

## Running Tasks

```bash
# Run a single task
python3 vector.py run \
  --task "Add input validation to the auth module" \
  --repo ~/projects/my-repo

# Check session status
python3 vector.py status

# Check remaining budget
python3 vector.py cost

# Interactive mode (review before each change)
python3 vector.py run --task "Refactor database layer" --interactive
```

## How It Works

1. **Plan** — Vector calls Claude to decompose your task into 3-7 atomic subtasks
2. **Execute** — For each subtask: generate code change, write file, run test
3. **Review** — Assess quality, iterate or proceed to next task
4. **Complete** — All tasks done: report + memory stored for future runs

## Memory & Conventions

Vector reads `memory/conventions.md` to understand your codebase conventions. Edit it to match your project:

```markdown
# Project Conventions
- Python 3.11, snake_case, type hints required
- Tests in tests/ directory, pytest
- All functions need docstrings
- No print() — use logging module
```

## Cost Control

Default limit: $5.00 per run. Override in vector.yaml:

```yaml
standalone:
  max_cost_per_run: 2.00  # tighter budget
```

Vector stops and reports before exceeding the limit.

## Escalations

When Vector encounters ambiguity or repeated failures, it logs an escalation:

```
[vector] ESCALATED: ambiguity on "Add OAuth support"
Reason: Task requires access to external OAuth provider credentials
```

In standalone mode, escalations are written to `vector.log`. In integrated mode, they go to Lumina via Nexus for judgment.

## Troubleshooting

| Issue | Fix |
|-------|-----|
| `Model not found` | Check `llm.model` in vector.yaml matches available models |
| `API key not set` | `export LITELLM_MASTER_KEY=your-key` |
| `git push failed` | Configure git credentials: `git config credential.helper store` |
| `Task always escalates` | Check conventions.md — add more context about your codebase |
| `Cost exceeded immediately` | Increase `max_cost_per_run` or use cheaper model (haiku) |
