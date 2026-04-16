# Deployment Modes

> Three ways to run Lumina. All use the same codebase.

## Mode 1: All-in-one (recommended for most)

One machine runs everything: Docker services + Ollama.

```
Single machine (Framework Desktop, Strix Halo, Mac Studio)
├── Docker Compose
│   ├── lumina-postgres    (database)
│   ├── lumina-litellm     (model proxy)
│   ├── lumina-terminus    (MCP hub)
│   ├── lumina-fleet       (agent scheduler)
│   ├── lumina-soma        (dashboard)
│   ├── lumina-spectra     (browser agent)
│   └── lumina-caddy       (reverse proxy)
└── Ollama (native — direct GPU access)
```

**Best for:** 64GB+ unified memory (Strix Halo, Apple Silicon M4). Everything fits. $0/day.

**Setup:**
```bash
git clone https://github.com/moosenet-io/lumina-constellation.git
cd lumina-constellation
bash deploy/install.sh
```

## Mode 2: Split inference

Services run on one machine, Ollama runs on a separate GPU box.

```
Services machine (any x86 Linux/Mac, even a mini PC)
├── Docker Compose (all services)
└── .env: OLLAMA_HOST=http://your-gpu-box:11434

GPU machine (NVIDIA or AMD discrete GPU)
└── Ollama (native)
```

**Best for:** Existing homelab setup. Run services on a low-power box, models on your gaming PC.

**Setup:**
1. On the GPU box: install Ollama, run `OLLAMA_ORIGINS="*" ollama serve`
2. On the services box: `OLLAMA_HOST=http://gpu-box-ip:11434 bash deploy/install.sh`

## Mode 3: Distributed homelab

Multiple nodes via Proxmox or k8s. Each service runs in its own container or VM.

```
Proxmox cluster (3 nodes)
├── PVM node: Terminus, Postgres, dev control
├── PVS node: IronClaw/Lumina, Fleet services, Matrix
└── PVE node: Ollama (VM901, dedicated GPU)
```

**Best for:** Power users who want to match MooseNet's actual setup. Maximum isolation and flexibility.

**Setup:** Follow the [homelab guide](hardware-guide.md) and use the Ansible playbooks in `infra/ansible/`.

---

## Choosing a mode

| Question | Answer → Mode |
|----------|--------------|
| I just want it to work on one machine | Mode 1 |
| I already have a dedicated GPU box | Mode 2 |
| I run a homelab with multiple machines | Mode 3 |
| I want to try it on a cloud VPS | Mode 1 (with cloud inference fallback) |

---

## Docker Compose profiles

The `docker-compose.yml` uses profiles for optional services:

```bash
docker compose up -d                    # Core services only
docker compose --profile gpu up -d      # Add Ollama GPU sidecar (for discrete GPU)
docker compose --profile matrix up -d   # Add Conduit Matrix server
```

**Core services** (always started): postgres, litellm, terminus, fleet, soma, spectra, caddy

**Profile: gpu** — adds `lumina-ollama` sidecar. Only use this for discrete GPUs. On unified memory systems (Strix Halo, Apple Silicon), run Ollama natively for better performance.

**Profile: matrix** — adds `lumina-conduit` Matrix homeserver. Only needed if you want self-hosted Matrix messaging. You can also use an existing Matrix server (hosted or public).

---

## Environment variables

See `deploy/.env.example` for all variables. Key ones:

| Variable | Required | Description |
|----------|----------|-------------|
| `OLLAMA_HOST` | Yes | URL to your Ollama instance |
| `POSTGRES_PASSWORD` | Yes | Database password |
| `SOMA_SECRET_KEY` | Yes | Soma session secret (generate: `openssl rand -hex 32`) |
| `LITELLM_MASTER_KEY` | Yes | LiteLLM admin key |
| `OPENROUTER_API_KEY` | No | Cloud inference fallback |
| `LUMINA_TIMEZONE` | No | IANA timezone (default: America/Los_Angeles) |

---

## Ports

| Service | Default port | Override variable |
|---------|-------------|------------------|
| Soma dashboard | 8082 | `SOMA_PORT` |
| LiteLLM proxy | 4000 | `LITELLM_PORT` |
| Terminus MCP | 8083 | `TERMINUS_PORT` |
| Spectra browser | 8084 | `SPECTRA_PORT` |
| Spectra internal | 8085 | `SPECTRA_INTERNAL_PORT` |
| Postgres | 5432 | `POSTGRES_PORT` |
| Caddy HTTP | 80 | — |
| Caddy HTTPS | 443 | — |
| Matrix (Conduit) | 6167 | `MATRIX_PORT` |
