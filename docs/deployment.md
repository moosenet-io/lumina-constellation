# Deployment

How to build, configure, and run Lumina Constellation — on a single machine or across
several — and how to handle secrets safely.

## Prerequisites

| Requirement | Notes |
|-------------|-------|
| **Rust toolchain** | A recent stable Rust ([rustup](https://rustup.rs) recommended) to build the workspace. |
| **A local model server** | A model serving runtime that serves open-weight models locally. Run it natively (not in a container) for direct GPU access. |
| **A GPU** | A discrete GPU with **Vulkan** or **ROCm** support, or **Apple Silicon** with Metal. CPU-only inference works but is much slower. A machine with a large unified-memory pool (for example, an AMD APU with unified memory, or Apple Silicon with 64 GB+) can keep several models resident at once. |
| **A chat channel** | A messaging endpoint the assistant talks on — for example a Matrix homeserver (self-hosted or public). |
| **A relational + vector store** | A local database for episodic memory and a local vector store for semantic memory. |

### Build

```bash
git clone https://github.com/<your-org>/lumina-constellation.git
cd lumina-constellation
cargo build --workspace --release
```

Binaries land in `target/release/`: `lumina-core`, `chord-proxy`, and the `terminus-rs`
tool hub.

### Pull a model

Pull your models into the local model server, for example:

- `qwen3:8b` — an example local chat model
- `nomic-embed-text` — an example embedding model for semantic memory

## Configuration reference

All configuration is supplied through environment variables. Copy `.env.example` to `.env`
and fill in your own values. **Never commit a populated `.env`.**

> In the examples below, `198.51.100.x` / `203.0.113.x` are documentation IP ranges
> (RFC 5737) — substitute your own addresses. Use `localhost` when a service runs on the
> same host.

### Core orchestrator (`lumina-core`)

| Variable | Example | Description |
|----------|---------|-------------|
| `LUMINA_HTTP_BIND` | `0.0.0.0:8080` | Address the orchestrator's HTTP interface binds to. |
| `LUMINA_HTTP_TOKEN` | `<bearer-token>` | Bearer token protecting the orchestrator's HTTP interface. |
| `LUMINA_SYSTEM_PROMPT` | `You are ...` | Persona / system prompt for the assistant. |
| `LUMINA_ADMIN_MATRIX_ID` | `@operator:example.org` | Chat ID of the operator/admin. |
| `LUMINA_EGRESS_ALLOWLIST` | `example.com,api.example.net` | Comma-separated allowlist of outbound hosts tools may reach. |
| `LUMINA_SESSION_TIMEOUT_SECS` | `1800` | Idle timeout before a session is closed. |
| `SESSION_IDLE_MINUTES` | `30` | Idle window for session bookkeeping. |
| `CONVERSATION_WINDOW` | `20` | Number of recent turns kept verbatim in working memory. |

#### Conversation memory

| Variable | Example | Description |
|----------|---------|-------------|
| `LUMINA_CONV_BUFFER_ENABLED` | `true` | Enable the working-memory buffer. |
| `LUMINA_CONV_BUFFER_SIZE` | `20` | Max turns held in the buffer. |
| `LUMINA_CONV_TOKEN_BUDGET` | `4000` | Token budget before older turns are summarized. |
| `LUMINA_CONV_SUMMARIZE_ENABLED` | `true` | Summarize (rather than drop) overflow turns. |
| `LUMINA_CONV_SUMMARIZE_THRESHOLD` | `3000` | Token threshold that triggers summarization. |
| `LUMINA_CONV_SUMMARIZE_MODEL` | `qwen3:8b` | Model used to summarize. |
| `LUMINA_CONV_SUMMARIZE_URL` | `http://localhost:11434` | Endpoint for the summarization model. |

#### Memory store

| Variable | Example | Description |
|----------|---------|-------------|
| `ENGRAM_EMBED_MODEL` | `nomic-embed-text` | Embedding model for semantic memory. |
| `OLLAMA_EMBEDDING_URL` | `http://localhost:11434` | Endpoint of the local model server serving the embedding model. |

### Chat channel (Matrix)

| Variable | Example | Description |
|----------|---------|-------------|
| `MATRIX_HOMESERVER` | `https://matrix.example.org` | Homeserver URL. |
| `MATRIX_USER` | `@assistant:example.org` | Bot account user ID. |
| `MATRIX_PASSWORD` | `<password>` | Bot account password (resolve from the vault). |
| `MATRIX_ROOM_ID` | `!room:example.org` | Room the assistant operates in. |
| `MATRIX_ALLOWED_USERS` | `@operator:example.org` | Comma-separated allowlist of users who may talk to it. |
| `MATRIX_STORE_PATH` | `./data/matrix` | Local path for the Matrix client store. |

### Inference proxy (`chord-proxy`)

| Variable | Example | Description |
|----------|---------|-------------|
| `CHORD_PROXY_PORT` | `8099` | Inference/proxy listener port. |
| `CHORD_PROXY_URL` | `http://localhost:8099` | URL clients use to reach the proxy. |
| `CHORD_CONTROL_PORT` | `8090` | Separate control-API listener (model-tier management). |
| `CHORD_JWT_SECRET` | `<random-32-bytes>` | HS256 signing secret for proxy/control auth. |
| `CHORD_LLM_URL` | `http://localhost:11434` | Backend inference endpoint (local model server). |
| `CHORD_MODEL_ALIASES` | `fast=qwen3:8b,deep=...` | Friendly aliases mapped to concrete models. |
| `CHORD_AGENTIC_MODE` | `true` | Enable the agentic tool-calling loop. |
| `CHORD_TOOL_TIMEOUT_SECS` | `30` | Per-tool-call timeout. |
| `CHORD_CATALOG_CACHE_SECS` | `60` | Tool-catalog cache TTL. |
| `CHORD_RATE_LLM_USER` / `CHORD_RATE_LLM_GUEST` | `60` / `10` | Per-minute LLM rate limits by caller class. |
| `CHORD_RATE_TOOL_USER` / `CHORD_RATE_TOOL_GUEST` | `120` / `20` | Per-minute tool-call rate limits. |
| `CHORD_RATE_DEEP_USER` / `CHORD_RATE_DEEP_GUEST` | `10` / `2` | Per-minute deep-reasoning rate limits. |

### Local inference backends

| Variable | Example | Description |
|----------|---------|-------------|
| `OLLAMA_URL` | `http://localhost:11434` | Primary (GPU) local-model-server endpoint. |
| `OLLAMA_CPU_URL` | `http://localhost:11435` | Optional CPU-only local-model-server endpoint for overflow. |

### Tool hub (`terminus-rs`)

| Variable | Example | Description |
|----------|---------|-------------|
| `TERMINUS_HOST` | `http://localhost:8083` | Tool-hub address. |
| `MCP_BACKEND_URL` | `http://localhost:8083` | Backend the proxy dispatches MCP tool calls to. |
| `MCP_MAX_TOOL_CALLS` | `8` | Max tool calls per agentic turn. |

### Model storage tiers

| Variable | Example | Description |
|----------|---------|-------------|
| `MODEL_REGISTRY_PATH` | `./data/model-registry.json` | Where model-tier state is recorded. |
| `MODEL_LOCAL_PATH` | `/var/lib/model-server/models` | Warm tier — local model directory. |
| `MODEL_ARCHIVE_PATH` | `/mnt/archive/models` | Cold tier — archive directory (e.g. network storage). |
| `MODEL_PROTECTED` | `qwen3:8b,nomic-embed-text` | Comma-separated models never auto-archived. |
| `MODEL_DISK_PRESSURE_PERCENT` | `80` | Disk-usage threshold that triggers eviction. |
| `MODEL_WARM_COOLDOWN_HOURS` | `168` | Idle hours before a warm model is archived. |
| `MODEL_SWEEP_INTERVAL_SECS` | `1800` | Background eviction-sweep interval. |
| `MODEL_PULL_TIMEOUT_SECS` | `600` | Timeout when pulling a cold model back to warm. |

See [model-tier-control-api.md](model-tier-control-api.md) for the control-API contract.

## Single-host deployment

For most operators, run everything on one machine: the local model server natively, and the
three Lumina binaries as long-running services.

1. Install the local model server and pull your models.
2. Build the workspace (`cargo build --workspace --release`).
3. Create your `.env` from `.env.example` and point every URL at `localhost`.
4. Run the binaries under a process supervisor (e.g. systemd units) so they restart on
   failure and start at boot. Start `chord-proxy` and `terminus-rs` before `lumina-core`.

A machine with a large unified-memory pool can keep several models hot simultaneously, so
routine operation never touches the cloud.

The example unit file below assumes the workspace is checked out at `./` (repo-relative);
adjust the paths and `EnvironmentFile` location to wherever you deploy.

### Example systemd unit

```ini
# /etc/systemd/system/lumina-core.service
[Unit]
Description=Lumina orchestrator
After=network-online.target

[Service]
WorkingDirectory=/srv/lumina-constellation
EnvironmentFile=/srv/lumina-constellation/.env
ExecStart=/srv/lumina-constellation/target/release/lumina-core
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

## Multi-host deployment

To spread load, run inference on a GPU machine and the services elsewhere. Only the URLs
change — there is no separate "distributed" build.

```
+--------------------------+        +---------------------------+
|  services host           |        |  inference host (GPU)     |
|  lumina-core             |        |  local model server       |
|  chord-proxy             | -----> |  (native)                 |
|  terminus-rs             |  LAN   |  models hot in memory     |
+--------------------------+        +---------------------------+
```

On the GPU host, start the local model server listening on the LAN (bind it to
`0.0.0.0:11434` or your model server's equivalent).

On the services host, point the inference variables at it (substitute your own LAN address):

```bash
OLLAMA_URL=http://198.51.100.20:11434
CHORD_LLM_URL=http://198.51.100.20:11434
OLLAMA_EMBEDDING_URL=http://198.51.100.20:11434
```

Keep the link on a trusted private network; a local model server typically has no
authentication of its own.

## Secrets management

Secrets are never hardcoded and never committed.

- **Vault-resolved at runtime.** Credentials (chat-channel password, proxy signing secret,
  cloud API keys) are stored in an encrypted vault and injected as environment variables
  when the services start.
- **`.env` for local development only.** Copy `.env.example` to `.env`, fill in your values,
  and keep `.env` out of version control (it is git-ignored). Use placeholders, not real
  secrets, anywhere a file might be committed.
- **Optional external secret manager.** For multi-host setups you can fetch secrets from a
  self-hosted secrets management backend at service start and merge them into the
  environment, rather than storing a `.env` on each host.
- **Generate strong secrets.** For signing/session keys: `openssl rand -hex 32`.

If you believe a secret has been exposed, treat it as compromised: rotate it immediately.
See [SECURITY.md](../SECURITY.md).
