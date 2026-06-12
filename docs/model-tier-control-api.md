# Model Storage Tier — Control API

`chord-proxy` exposes model storage-tier management on its **control port** (`CHORD_CONTROL_PORT`, default **8090**) — a second HTTP listener separate from the inference/proxy port (`CHORD_PROXY_PORT`, default 8099). All endpoints require the same JWT bearer auth as the proxy (`Authorization: Bearer <token>`, HS256 signed with `CHORD_JWT_SECRET`). The control listener is best-effort: if it fails to bind, the proxy keeps serving.

This API is the contract a dashboard consumes to display and manage model tiers.

## Endpoints

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| GET | `/api/models` | yes | List every model in the registry with tier, size, timestamps, protected flag |
| GET | `/api/models/{name}` | yes | Single model detail; `404` if unknown |
| POST | `/api/models/{name}/archive` | yes | Archive a warm model (warm → cold). `409` if Hot (unload first); `403` if protected |
| POST | `/api/models/{name}/pull` | yes | Pull a cold model (cold → warm) from the archive; pre-pull eviction frees space if needed |
| POST | `/api/models/{name}/protect` | yes | Toggle/set the protected flag (never auto-archived) |
| GET | `/api/storage` | yes | Disk usage summary for local and archive tiers |
| POST | `/api/models/sweep` | yes | Manually trigger the eviction sweep (cooldown + disk-pressure); returns `202` |

`{name}` is the fully-tagged model name as known to the local model server (e.g. `qwen3-coder:30b`); colons in the segment are permitted.

## Responses

- `GET /api/models` → `200` JSON array of records:
  ```json
  [{ "name": "qwen3-coder:30b", "tier": "warm", "size_bytes": 18601248000,
     "local_path": "/var/lib/model-server/models", "archive_path": null,
     "last_requested": 1781120000, "last_loaded": null, "protected": true }]
  ```
  `tier` is `hot` | `warm` | `cold`; timestamps are Unix epoch seconds (nullable).
- `GET /api/storage` → `200` JSON with `local` and `archive` objects (`used_bytes`/`free_bytes`/`total_bytes`); archive fields are null when the mount is unavailable.
- Mutating endpoints return the affected model's updated record (or an accepted/202 for `sweep`).
- Errors use the proxy's auth-error envelope for `401`, and JSON `{ "error": "..." }` for `403`/`404`/`409`/`503`.

## Config (non-secret, behavioral)

| Env var | Default | Meaning |
|---------|---------|---------|
| `CHORD_CONTROL_PORT` | 8090 | Control API listener port |
| `MODEL_LOCAL_PATH` | /var/lib/model-server/models | Warm tier (local model files) |
| `MODEL_ARCHIVE_PATH` | /mnt/archive/models | Cold tier (network/archive storage) |
| `MODEL_DISK_PRESSURE_PERCENT` | 80 | Disk-pressure eviction threshold |
| `MODEL_WARM_COOLDOWN_HOURS` | 168 | Idle hours before cooldown archival |
| `MODEL_SWEEP_INTERVAL_SECS` | 1800 | Background eviction sweep interval |
| `MODEL_PROTECTED` | (6 core models) | Comma-separated never-archived models |
