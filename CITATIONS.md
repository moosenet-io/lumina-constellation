# Citations and credits

## Architectural influences

**Geoffrey Huntley's Ralph loop** — The feedback-gated autonomous development pattern that influenced ARCADE and Vector's design. The core insight: autonomous agents need explicit approval gates, not just error handling. Lumina's entire agent architecture uses this pattern.
- [ghuntley.com](https://ghuntley.com)

**Calx behavioral correction** — T1/T2/T3 trigger system for maintaining code quality in autonomous loops. Originally from getcalx/oss (now archived by author, moved to hosted platform). Integrated natively into Vector's CalxEngine. The behavioral correction concept — catching quality drift before it compounds — is foundational to how Vector operates.
- [getcalx.dev](https://getcalx.dev)

**IronClaw** — The agent runtime that powers Lumina. Provides the MCP transport, LLM backend routing, encrypted vault, planning mode, and channel system. Lumina is fundamentally an IronClaw agent with a constellation of tools and services built around it.
- [claw-project/ironclaw](https://github.com/claw-project/ironclaw)

## Academic references

**Agentic Code Reasoning** — Ugare, S. & Chandra, S., Meta (2026). Semi-formal structured reasoning with certificate templates for code analysis. The paper introduced the concept of LLMs producing structured "certificates" that can be verified and composed. Implemented in Lumina's Cortex module and Obsidian Circle's code review sessions.
- [arxiv.org/abs/2603.01896](https://arxiv.org/abs/2603.01896)

**SkillClaw: Let Skills Evolve Collectively with Agentic Evolver** — Ma, Z. et al. (2026). Collective skill evolution in multi-user agent ecosystems. Inspired Lumina's skill evolution system (soma_trajectory + propose + tracker) and the agentskills.io integration.
- [arxiv.org/abs/2604.08377](https://arxiv.org/abs/2604.08377)
- [AMAP-ML/SkillClaw](https://github.com/AMAP-ML/SkillClaw)

**Large Language Models Cannot Self-Correct Reasoning Yet** — Huang, J. et al. (2023). This paper directly influenced the multi-model Obsidian Circle design: rather than asking one model to self-correct, Lumina convenes multiple models with different architectures and training biases. Disagreement between models is the signal, not a bug.
- [arxiv.org/abs/2310.01798](https://arxiv.org/abs/2310.01798)

**code-review-graph** — Tirth Patel. Tree-sitter AST knowledge graph with blast-radius analysis. Powers Cortex's code intelligence — analyzes repository structure, dependency chains, and change impact without reading every file.
- [tirth8205/code-review-graph](https://github.com/tirth8205/code-review-graph)

## Runtime and frameworks

| Project | Role in Lumina | Source |
|---------|---------------|--------|
| **FastMCP** | MCP server framework powering Terminus (272+ tools) | [jlowin/fastmcp](https://github.com/jlowin/fastmcp) |
| **LiteLLM** | Unified LLM proxy — routes between Ollama, OpenRouter, Anthropic API | [BerriAI/litellm](https://github.com/BerriAI/litellm) |
| **Ollama** | Local model serving (Qwen3.5 fleet on Strix Halo / Apple Silicon) | [ollama/ollama](https://github.com/ollama/ollama) |
| **Playwright** | Browser automation engine powering Spectra | [microsoft/playwright](https://github.com/microsoft/playwright) |
| **Caddy** | Automatic HTTPS reverse proxy | [caddyserver/caddy](https://github.com/caddyserver/caddy) |
| **rrweb** | Session recording for Spectra browser sessions | [rrweb-io/rrweb](https://github.com/rrweb-io/rrweb) |
| **noVNC** | Web-based VNC client for Spectra Live View | [novnc/noVNC](https://github.com/novnc/noVNC) |
| **sqlite-vec** | Vector embeddings for Engram's RAG memory | [asg017/sqlite-vec](https://github.com/asg017/sqlite-vec) |

## Self-hosted backends

| Project | Powers module | Source |
|---------|--------------|--------|
| **Actual Budget** | Ledger (finance tracking) | [actualbudget/actual](https://github.com/actualbudget/actual) |
| **Grocy** | Hearth (kitchen/pantry management) | [grocy/grocy](https://github.com/grocy/grocy) |
| **LubeLogger** | Relay (vehicle maintenance) | [hargata/lubelog](https://github.com/hargata/lubelog) |
| **SearXNG** | Seer (privacy-respecting web research) | [searxng/searxng](https://github.com/searxng/searxng) |
| **Plane CE** | The Plexus (work queue / project management) | [makeplane/plane](https://github.com/makeplane/plane) |
| **Tuwunel** | Matrix homeserver (messaging channel) | [avdb13/tuwunel](https://github.com/avdb13/tuwunel) |

## Infrastructure

| Project | Role | Source |
|---------|------|--------|
| **virtualization platform** | Hypervisor for self-hosted deployment | [virtualization.com](https://virtualization.com) |
| **Gitea** | Self-hosted Git — source of truth for all code | [go-gitea/gitea](https://github.com/go-gitea/gitea) |
| **Infisical** | Secrets management — runtime fetch, Ansible-based rotation | [Infisical/infisical](https://github.com/Infisical/infisical) |
| **Prometheus** | Metrics collection for Sentinel monitoring | [prometheus/prometheus](https://github.com/prometheus/prometheus) |
| **CoreDNS** | Internal DNS resolution | [coredns/coredns](https://github.com/coredns/coredns) |
| **AdGuard Home** | DNS-level ad blocking | [AdguardTeam/AdGuardHome](https://github.com/AdguardTeam/AdGuardHome) |

## Models

| Model family | Provider | Role in Lumina |
|-------------|----------|---------------|
| **Qwen3 / Qwen3.5** | Alibaba (Apache 2.0) | Primary local inference — daily driver, fast scaffolding, embeddings |
| **Claude** | Anthropic | Cloud frontier reasoning, Obsidian Circle Architect seat, all build sessions |
| **GPT / o3** | OpenAI | Obsidian Circle Skeptic Seer seat, adversarial reasoning |
| **DeepSeek** | DeepSeek | Obsidian Circle Keeper of Operations seat (local via Ollama) |
| **Gemini** | Google | Obsidian Circle Wandering Fool seat, associative reasoning |

## Built with

This project was built by [@moosenet-io](https://github.com/moosenet-io), a non-developer with no coding background, directing AI through voice transcription and agentic development loops.

**Claude** ([Anthropic](https://anthropic.com)) served as co-developer — specifications, implementation, autonomous build sessions (up to 18 hours each), and infrastructure debugging via Claude Code.

30 specification documents. 850+ work items across 23 Plane projects. 17 build sessions. The entire development process is documented in the [specs/](specs/) directory.
