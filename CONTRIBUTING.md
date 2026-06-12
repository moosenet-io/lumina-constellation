# Contributing to Lumina Constellation

Thanks for your interest in contributing! Lumina Constellation is a Rust
workspace for a self-hosted, privacy-first personal AI assistant. Contributions
of all kinds are welcome — bug reports, fixes, features, docs, and tests.

## Development setup

Lumina is a standard Cargo workspace.

```bash
# Prerequisites: a recent stable Rust toolchain (rustup recommended) and OpenSSL.
git clone https://github.com/<your-fork>/lumina-constellation.git
cd lumina-constellation

# Build the whole workspace
cargo build --workspace

# Run the test suite
cargo test --workspace
```

Configuration is supplied entirely through environment variables (see
`crates/*/src/config.rs` for the available keys, and
[`docs/deployment.md`](docs/deployment.md#configuration-reference) for what each
one does). Copy `.env.example` to `.env` and fill in your own values — **never**
commit a populated `.env`.

## Project layout

- `crates/lumina-core` — the orchestrator: chat channels, memory subsystem, prompt
  assembly, scheduler, security, vault.
- `crates/chord-proxy` — the smart inference proxy and agentic tool-calling loop.
- `crates/terminus-rs` — the MCP tool hub (version control, project tracking,
  monitoring, web research, calendar, and more).
- `daemon/` — supporting native daemons.
- `docs/` — architecture and module documentation.
- `tests/` — integration and behavioral tests.

## Pull request process

Changes follow a **worktree → review → merge** flow:

1. Fork the repository (or, as a maintainer, create a git worktree) and start a
   topic branch off `main`. Working in a worktree keeps unrelated changes isolated.
2. Keep changes focused — one logical change per PR.
3. Add or update tests for any behavior you change.
4. Ensure `cargo build --workspace`, `cargo test --workspace`,
   `cargo fmt --all`, and `cargo clippy --workspace` are all clean.
5. Open a PR with a clear description of the change and its motivation.
6. Address review feedback, then squash-merge once approved and green.

## Rust style

- Format with `cargo fmt --all` (rustfmt defaults).
- Lint with `cargo clippy --workspace --all-targets` and resolve warnings.
- Prefer explicit error types (`thiserror`) over `unwrap()`/`panic!` in
  non-test code. Tests may `unwrap()` freely.
- Keep modules small and documented with `//!` module docs and `///` item docs.

## Commit format

Use clear, imperative commit subjects, optionally with a conventional-commit
type prefix:

```
fix(memory): correct retrieval scoring for empty queries
feat(briefing): add weekly cost summary to the briefing engine
docs: clarify vault setup in README
```

## Security & privacy

This project handles personal data and credentials. Please read
[`SECURITY.md`](./SECURITY.md) before contributing. Never include real secrets,
private IP addresses, hostnames, personal data, or other infrastructure details
in code, tests, comments, or commit messages — use placeholders and environment
variables instead.

By contributing, you agree that your contributions are licensed under the
project's [MIT License](./LICENSE).
