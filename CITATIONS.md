# Citations and credits

## Core technologies

- **[IronClaw](https://github.com/claw-project/ironclaw)** — Agent runtime. Lumina runs on IronClaw v0.24.0.
- **[FastMCP](https://github.com/jlowin/fastmcp)** — MCP server framework. Powers Terminus.
- **[LiteLLM](https://github.com/BerriAI/litellm)** — Model proxy. Routes between Ollama and cloud providers.
- **[Ollama](https://ollama.com)** — Local inference runtime. Serves all local models.
- **[Playwright](https://playwright.dev)** — Browser automation. Powers Spectra.
- **[rrweb](https://github.com/rrweb-io/rrweb)** — Session recording. Captures Spectra browser sessions for replay.
- **[noVNC](https://novnc.com)** — Web-based VNC. Powers Spectra Live View.
- **[Plane CE](https://plane.so)** — Project management. Powers the Plexus work queue.
- **[Tuwunel](https://github.com/avdb13/tuwunel)** — Matrix homeserver. Powers messaging.
- **[Actual Budget](https://actualbudget.org)** — Budget tracking. Powers Ledger.
- **[LubeLogger](https://lubelogger.com)** — Vehicle maintenance tracking. Powers Relay.
- **[SearXNG](https://docs.searxng.org)** — Privacy-respecting metasearch. Powers Seer.
- **[Prisma](https://prisma.io)** — Database ORM. Used by LiteLLM for key management.

## Methodologies

- **[Calx behavioral correction](https://getcalx.dev)** — T1/T2/T3/T4 trigger system for code quality guardrails in autonomous development loops. Integrated natively into Vector. getcalx/oss archived by original author; credit preserved in Vector README.
- **[Geoffrey Huntley's Ralph loop](https://ghuntley.com)** — Feedback-gated autonomous development pattern. Influenced ARCADE and Vector's design.

## Models

- **[Qwen3 / Qwen3.5](https://qwenlm.github.io)** by Alibaba — Primary local inference models. Apache 2.0 license.
- **[Claude](https://anthropic.com)** by Anthropic — Cloud frontier reasoning and all 17 build sessions.
- **[GPT-4](https://openai.com)** by OpenAI — Obsidian Circle council member.
- **[DeepSeek](https://deepseek.com)** — Obsidian Circle council member, local code models.
- **[Gemini](https://deepmind.google)** by Google — Obsidian Circle council member.

## Infrastructure

- **[Proxmox VE](https://proxmox.com)** — Hypervisor for MooseNet homelab cluster.
- **[Gitea](https://gitea.com)** — Self-hosted Git. Source of truth for all code.
- **[Infisical](https://infisical.com)** — Secrets management. All runtime secrets stored here.
- **[Caddy](https://caddyserver.com)** — Reverse proxy with automatic TLS.

## Design

- **[constellation.css](fleet/soma/)** — The shared design system used across all Soma pages and module HTML reports. Dark/light mode, CSS variables, reusable components.

## Built with

This project was built by [Peter Boose](https://github.com/LeMajesticMoose) ("Moose"), a field marketing manager with no coding background, directing AI through voice transcription and agentic development loops.

- 30 specification documents written in Claude.ai
- 850+ work items tracked across 23 Plane projects
- 17 autonomous build sessions with Claude Code, up to 18 hours each
- Zero lines written by hand

The entire development process is documented in the [specs/](specs/) directory. If you can describe what you want clearly, you can build something like this.
