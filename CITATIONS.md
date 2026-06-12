# Citations and Credits

## Architectural Influences

**Geoffrey Huntley's Ralph Loop** — The feedback-gated autonomous development pattern that influenced ARCADE, the original Python-era build orchestrator. The core insight — autonomous agents need explicit approval gates, not just error handling — carried forward into Harmony (the Rust successor) and the build pipeline's deterministic stage design. Lumina's agent architecture itself is independent of the Ralph pattern.
- [ghuntley.com](https://ghuntley.com)

**Calx Behavioral Correction** — T1/T2/T3 trigger system for maintaining code quality in autonomous development loops. Originally from getcalx/oss (now archived by author, moved to hosted platform). The behavioral correction concept — catching quality drift before it compounds — influenced the build pipeline's review gate design and test regression enforcement.
- [getcalx.dev](https://getcalx.dev)

**IronClaw / OpenClaw** — Lumina's earliest prototype ran as an IronClaw agent (the open-source autonomous AI runtime). IronClaw provided the original MCP transport, LLM backend routing, and channel system that shaped Lumina's initial architecture. Lumina has since been completely rewritten as a custom Rust implementation (`lumina-core`) and no longer uses IronClaw's runtime, but the architectural patterns — MCP-based tool calling, encrypted credential vaults, channel-based messaging, planning mode — trace directly back to IronClaw's design.
- [OpenClaw](https://github.com/openclaw/openclaw) (247K+ GitHub stars, formerly Clawdbot)
- [IronClaw](https://github.com/claw-project/ironclaw)

**OpenAI Assistants API** — The client-sends-context, server-runs-tool-loop, server-returns-result pattern. Direct inspiration for Chord's agentic execution mode: the agent sends context, Chord runs the tool loop internally with security guards, and returns the final response plus execution log.

**OpenAI Symphony** — Six-layer orchestration spec for deterministic agent pipelines. Influenced the build pipeline design: deterministic stages with entry/exit criteria, worktree isolation, dual-reviewer gates.

**Hermes Agent** — The "identity file" pattern: a persistent markdown document (2,000+ words) loaded into the system prompt every turn, containing personality, communication style, and user knowledge. Informed the Knowledge Digest and Personality Vector design — the system prompt should be rich and detailed, not a one-line instruction.

---

## Academic References

### Memory & Personalization

**PersonaMem-v2** — Compact memory profiles reconstructed from conversation archives. 2K-token compact memory outperforms 32K-token full conversation histories (16× more efficient). Validates the Knowledge Digest approach: reconstruct a compact profile from raw archives, don't retrieve individual memories.

**MemMachine** — Dual memory architecture (episodic + profile) with ground-truth preservation. 0.93 accuracy from storing raw conversations and reconstructing profiles, vs lower accuracy from LLM-extracted facts. Core principle adopted: "store raw conversations as ground truth, reconstruct during sleep-time."

**MAPLE (AAMAS 2026)** — Decomposes agent adaptation into Memory, Learning, and Personalization as architecturally distinct components. Key insight: Learning is asynchronous — it happens offline, separate from user interaction. Theoretical basis for sleep-time consolidation.

**SCM (Sleep-Consolidated Memory)** — Bounded working memory + multidimensional importance tagging + offline sleep-driven consolidation + algorithmic forgetting. Most biologically-inspired approach in the survey. Informed trait self-tuning (exponential decay windows) and active forgetting (archiving stale memories).

**Letta Sleep-Time Compute** — Agents reflect, consolidate, and improve during idle periods. Sleep-time compute as a scaling axis. Direct inspiration for the nightly Knowledge Digest reconstruction and weekly Personality Vector rebuild.
- [letta.com](https://www.letta.com/)
- Originally published as MemGPT: [arxiv.org/abs/2310.08560](https://arxiv.org/abs/2310.08560)

**arXiv:2606.04703** — Principle-level experience outperforms instance-level experience for LoRA fine-tuning. Teaching a model "the user prefers direct communication" (principle) is more effective than 50 examples of that preference (instances). Informed the memory type hierarchy and principle abstraction.

**"Large Language Models Cannot Self-Correct Reasoning Yet"** — Huang, J. et al. (2023). Directly influenced the multi-model review design: rather than asking one model to self-correct, convene multiple models with different architectures and training biases. Disagreement between models is the signal.
- [arxiv.org/abs/2310.01798](https://arxiv.org/abs/2310.01798)

### Context Compression & Long-Context

**LCLM: Latent Context Language Models** — Li, A. et al. (NYU, Modal Labs, UMD, Princeton, Columbia, Harvard, LLNL, Meta FAIR). June 2026. Encoder-decoder context compression at scale: 0.6B encoder, 4B decoder, 350B+ training tokens. Achieves 1:4 to 1:16 compression while preserving quality. Key insight: interleaved compressed/uncompressed segments train better than front-loading compressed content. The agent scaffolding pattern (compress everything, EXPAND on demand) validates the retrieval subagent architecture.
- [arxiv.org/abs/2606.09659](https://arxiv.org/abs/2606.09659)
- Models: [huggingface.co/latent-context](https://huggingface.co/latent-context)
- Code: [github.com/LeonLixyz/LCLM](https://github.com/LeonLixyz/LCLM)

**FlashMemory-DeepSeek-V4** — June 2026. Lookahead Sparse Attention with Neural Memory Indexer. Keeps KV cache on CPU, predicts which chunks the GPU needs. 86.5% memory reduction with +0.6% accuracy improvement — selective context acts as an "attention denoiser." The Neural Memory Indexer trains independently from the backbone model in 1 GPU-hour. Informed the predictive context fetch roadmap.
- [arxiv.org/abs/2606.09079](https://arxiv.org/abs/2606.09079)

### Code Intelligence

**Agentic Code Reasoning** — Ugare, S. & Chandra, S., Meta (2026). Semi-formal structured reasoning with certificate templates for code analysis. LLMs producing structured "certificates" that can be verified and composed. Implemented in the Cortex module's code review design.
- [arxiv.org/abs/2603.01896](https://arxiv.org/abs/2603.01896)

**code-review-graph** — Tirth Patel. Tree-sitter AST knowledge graph with blast-radius analysis. Powers code intelligence — analyzes repository structure, dependency chains, and change impact without reading every file.
- [github.com/tirth8205/code-review-graph](https://github.com/tirth8205/code-review-graph)

**SkillClaw: Let Skills Evolve Collectively with Agentic Evolver** — Ma, Z. et al. (2026). Collective skill evolution in multi-user agent ecosystems.
- [arxiv.org/abs/2604.08377](https://arxiv.org/abs/2604.08377)
- [github.com/AMAP-ML/SkillClaw](https://github.com/AMAP-ML/SkillClaw)

### Retrieval

**Harness-1** — 20B retrieval subagent trained with RL (CISPO) on GPT-OSS 20B. 0.730 average curated recall across 8 benchmarks. Stateful cognitive offloading: the harness maintains bookkeeping while the policy model makes semantic decisions.
- [arxiv.org/abs/2606.02373](https://arxiv.org/abs/2606.02373)
- Model: [huggingface.co/pat-jj/harness-1](https://huggingface.co/pat-jj/harness-1)

### Benchmarking

**Claw-SWE-Bench** — Zheng, M. et al. June 2026. Benchmark for evaluating OpenClaw-style agent harnesses on real software engineering tasks. 350 GitHub issue-resolution instances across 8 languages, 43 repositories. Standardized adapter protocol for comparing heterogeneous agents.
- [arxiv.org/abs/2606.12344](https://arxiv.org/abs/2606.12344)
- Code: [github.com/opensquilla/claw-swe-bench](https://github.com/opensquilla/claw-swe-bench)

**SWE-bench** — Jimenez et al. (2024). De facto standard for repository-level coding agent evaluation.
- [swebench.com](https://www.swebench.com/)

---

## Models

| Model Family | Provider | License | Role |
|-------------|----------|---------|------|
| **GPT-OSS 20B / 120B** | OpenAI | Open | Primary personality (20B) and deep reasoning (120B) |
| **Qwen3 / Qwen3.5 / Qwen3.6** | Alibaba | Apache 2.0 | Local inference fleet — code execution, review, summarization, embeddings |
| **DiffusionGemma 26B-A4B** | Google | Apache 2.0 | Batch processing — code review, spec enrichment (diffusion-based parallel generation) |
| **Harness-1** | pat-jj | Open | Research retrieval subagent (RL-trained on GPT-OSS 20B) |
| **Claude** | Anthropic | Commercial | Architecture planning, primary code review, build sessions |
| **Gemini** | Google | Commercial | Documentation, analysis, secondary review |

---

## Runtime and Frameworks

| Project | Role | Source |
|---------|------|--------|
| **Ollama** | Local model serving and VRAM lifecycle management | [ollama/ollama](https://github.com/ollama/ollama) |
| **llama.cpp** | Custom model builds (Vulkan/ROCm), DiffusionGemma daemon | [ggml-org/llama.cpp](https://github.com/ggml-org/llama.cpp) |
| **LiteLLM** | Unified LLM proxy — routes between local and cloud providers | [BerriAI/litellm](https://github.com/BerriAI/litellm) |
| **Playwright** | Browser automation engine for visual testing | [microsoft/playwright](https://github.com/microsoft/playwright) |

## Self-Hosted Services

| Project | Powers | Source |
|---------|--------|--------|
| **Plane CE** | Work queue and project management | [makeplane/plane](https://github.com/makeplane/plane) |
| **Tuwunel** | Matrix homeserver (messaging channel) | [avdb13/tuwunel](https://github.com/avdb13/tuwunel) |
| **SearXNG** | Privacy-respecting web research | [searxng/searxng](https://github.com/searxng/searxng) |
| **Actual Budget** | Finance tracking integration | [actualbudget/actual](https://github.com/actualbudget/actual) |

## Infrastructure

| Project | Role | Source |
|---------|------|--------|
| **Proxmox VE** | Virtualization platform for self-hosted deployment | [proxmox.com](https://www.proxmox.com/) |
| **Gitea** | Self-hosted Git — source of truth for all code | [go-gitea/gitea](https://github.com/go-gitea/gitea) |
| **Infisical** | Secrets management with runtime fetch and rotation | [Infisical/infisical](https://github.com/Infisical/infisical) |
| **Prometheus** | Metrics collection for monitoring | [prometheus/prometheus](https://github.com/prometheus/prometheus) |
| **CoreDNS** | Internal DNS resolution | [coredns/coredns](https://github.com/coredns/coredns) |
| **AdGuard Home** | DNS-level ad blocking | [AdguardTeam/AdGuardHome](https://github.com/AdguardTeam/AdGuardHome) |
| **PostgreSQL** | Primary datastore for episodic memory and state | [postgresql.org](https://www.postgresql.org/) |

---

## Design Principles Validated by Research

1. **Reconstruct, don't retrieve** — Profile reconstruction from raw archives during sleep-time outperforms retrieval at query time (MemMachine, PersonaMem-v2, SCM).
2. **Selective retrieval beats exhaustive context** — Compact memory consistently outperforms full history (PersonaMem 16×, FlashMemory +0.6% accuracy from filtering).
3. **Interleaved compressed + verbatim** — Mixing compressed and raw context in the prompt trains and performs better than front-loading all compressed content (LCLM).
4. **Compress everything, expand on demand** — Keep a compressed overview, selectively expand relevant segments (LCLM agent scaffolding, Harness-1 retrieval pattern).
5. **Tiered storage with lifecycle management** — Hot/warm/cold hierarchies apply at both model level (VRAM → SSD → NFS) and context level (buffer → episodic → semantic).
6. **Multiple reviewers catch what self-review misses** — Disagreement between models is the signal (Huang et al. 2023, implemented via dual-reviewer build pipeline).
7. **Diffusion generation for batch, autoregressive for chat** — Parallel canvas generation excels at offline tasks; streaming autoregressive wins for interactive conversation (DiffusionGemma evaluation, two rounds).

---

## Built With

This project was built by a non-developer with no coding background, directing AI through voice transcription and agentic development loops.

**Claude** ([Anthropic](https://anthropic.com)) served as co-developer — specifications, architecture, implementation review, and autonomous build sessions via Claude Code.

The entire development process is documented through specification documents and build reports in the repository.
