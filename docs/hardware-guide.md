# Hardware guide

> What to buy, what to configure, and what to expect.

## Recommended configurations

| Budget | Hardware | RAM | What you get |
|--------|----------|-----|-------------|
| ~$800 | Mac Mini M4 | 32GB | Basic Lumina, 9B models, cloud for heavy reasoning |
| ~$1,600 | Framework Desktop 64GB | 64GB | Full Lumina, 35B MoE daily driver, local-only capable |
| ~$2,000 | Framework Desktop 128GB | 128GB | **Recommended.** Four models resident simultaneously. $0/day. |
| ~$3,000 | Mac Studio M4 Max | 96-128GB | Same capability as Framework 128GB, macOS ecosystem |

## Strix Halo (AMD Ryzen AI Max) setup

Strix Halo uses unified memory — CPU and GPU share the same RAM pool. You need to tell the OS how much the GPU can use.

**Linux (recommended):**
The installer handles this automatically. It sets kernel parameters:
- `amdgpu.gttsize` — GPU-accessible memory pool size
- `ttm.page_pool_size` / `ttm.pages_limit` — translation table limits
- Ollama systemd override with `HSA_OVERRIDE_GFX_VERSION=11.5.1`

**Windows:**
AMD Adrenalin → Performance → Tuning → Variable Graphics Memory → Custom → set to 96GB (on 128GB system).

## Apple Silicon setup

No special configuration needed. Ollama uses Metal automatically. Install Ollama, run the installer, done.

## Model fleet presets

The installer recommends models based on your detected hardware:

| Config | Primary | Secondary | Fast | All resident? |
|--------|---------|-----------|------|--------------|
| 32GB | Qwen3-8B (5GB) | Qwen3-4B (3GB) | — | Yes |
| 64GB | Qwen3.5-35B-A3B (20GB) | Qwen3.5-9B (6GB) | Qwen3.5-4B (3GB) | Yes |
| 128GB | Qwen3.5-122B-A10B (45GB) | Qwen3.5-35B-A3B (20GB) | Qwen3.5-27B (18GB) | Yes |

All sizes are Q4_K_M quantization. Models stay resident in memory (`OLLAMA_KEEP_ALIVE=-1`) for instant response.

## Discrete GPU users

Rule of thumb: any 8B model needs ~6-7GB VRAM, any 32B model needs ~22-24GB at Q4_K_M. The Soma setup wizard detects your GPU and recommends accordingly.

## External inference box

Running Ollama on a separate machine? Set `OLLAMA_HOST=http://your-gpu-box:11434` and Lumina routes requests to it transparently through LiteLLM.
