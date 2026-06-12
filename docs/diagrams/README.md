# Architecture Diagrams

Mermaid sources for the Lumina Constellation architecture. All diagrams use generic,
functional descriptions (no infrastructure-specific names).

| Source | Renders to | Shows |
|--------|-----------|-------|
| `architecture-overview.mmd` | `architecture-overview.svg` | The three crates and their runtime relationships |
| `lumina-core.mmd` | `lumina-core.svg` | Agent runtime internals (loop, memory, security, channels) |
| `chord-proxy.mmd` | `chord-proxy.svg` | Inference proxy + tool gateway pipeline |
| `terminus-rs.mmd` | `terminus-rs.svg` | Tool hub registry and tool-group breakdown |

## Rendering

```bash
npx -p @mermaid-js/mermaid-cli mmdc -i docs/diagrams/architecture-overview.mmd -o docs/diagrams/architecture-overview.svg
```

> **Note:** SVGs are generated from these Mermaid sources during release prep. The
> `.mmd` sources are the source of truth; regenerate the SVGs whenever a source changes.
