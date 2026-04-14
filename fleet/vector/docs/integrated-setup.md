# Vector — Integrated Mode Setup

Vector's **integrated mode** connects to the full Lumina stack: Plane CE for task state, Nexus inbox for messaging, and Engram for semantic memory. This is the production deployment on CT310.

## Prerequisites

- CT310 running with Axon and Nexus inbox operational
- PostgreSQL (CT300) reachable at `$INBOX_DB_HOST`
- Plane CE (CT315) reachable at `http://<plane-ip>`
- The Plexus (PX) project created in Plane
- LiteLLM proxy (CT215) running

## Environment Variables

Set in `/opt/lumina-fleet/axon/.env` (shared with Axon):

```bash
INBOX_DB_HOST=192.168.0.x   # CT300 Postgres
INBOX_DB_USER=lumina_inbox
INBOX_DB_PASS=...
PLANE_TOKEN_LUMINA=plane_api_...
GITEA_TOKEN=...
PX_PROJECT_ID=...            # The Plexus project UUID
LITELLM_MASTER_KEY=...
```

## vector.yaml (integrated mode)

Copy from `/opt/lumina-fleet/vector/config/vector.yaml.example` and set:

```yaml
mode: integrated

llm:
  endpoint: http://<litellm-ip>:4000
  api_key_env: LITELLM_MASTER_KEY
  model: claude-sonnet-4-6

integrated:
  nexus_db_host_env: INBOX_DB_HOST
  nexus_db_user_env: INBOX_DB_USER
  nexus_db_pass_env: INBOX_DB_PASS
  plane_url: http://<plane-ip>
  plane_token_env: PLANE_TOKEN_LUMINA
  plexus_project_id_env: PX_PROJECT_ID
  agent_id: vector
  max_cost_per_run: 5.00
  max_iterations: 10
```

## Deployment

```bash
# On CT310
cp /opt/lumina-fleet/vector/config/vector.yaml.example /opt/lumina-fleet/vector/vector.yaml
# Edit vector.yaml — set mode: integrated

# Test run
cd /opt/lumina-fleet/vector
python3 vector.py run --task "Write a hello world test file" --repo /tmp/test-repo

# Enable as service (standalone mode on-demand; integrated via Axon work orders)
systemctl enable vector.service
```

## How Lumina Delegates to Vector

Lumina uses `axon_submit_work` with op `dev_loop`:

```
axon_submit_work(
  op="dev_loop",
  description="Refactor auth module to use JWT",
  params={"repo": "moosenet/lumina-terminus", "task": "Refactor auth module to use JWT"}
)
```

Axon picks up the work order, calls Vector via subprocess, and reports results back through Nexus.

## Architecture

```
the operator → Lumina → axon_submit_work → Nexus → Axon → vector.py run → Git → PR
                                                ↑                        ↓
                                           Plane PX               nexus_send (result)
                                                                        ↓
                                                                    Lumina reports to the operator
```

## Backends

| Backend | Standalone | Integrated |
|---------|-----------|------------|
| State | SQLite (`vector-state.db`) | Plane CE (The Plexus, PX) |
| Messages | stdout / log file | Nexus inbox (PostgreSQL) |
| Memory | Local markdown files | Engram (sqlite-vec) |
| Cost gate | JSON file | Local (same) |

## Troubleshooting

- **Nexus connection fails**: Check `INBOX_DB_HOST` env var and CT300 UFW rules
- **Plane 401**: Regenerate `PLANE_TOKEN_LUMINA` in Infisical
- **Model errors**: LiteLLM proxy at CT215 must be running; check `LITELLM_MASTER_KEY`
- **Vector not picking up tasks**: Verify Axon is running (`systemctl status axon`) and polling Nexus
