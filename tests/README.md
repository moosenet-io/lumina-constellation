# ✦ Testing

> Ensuring the constellation stays bright.

**Testing** contains the test suites and fixtures for validating Lumina Constellation.

## What it does

- Houses unit, integration, and end-to-end tests for all modules.
- Provides test fixtures and mock data for consistent verification.
- Automates the "Validate" phase of the development lifecycle.
- Checks for regressions across the multi-agent fleet.
- Validates API contracts and data schemas between services.

## Key files

| File | Purpose |
|------|---------|
| `README.md` | Overview of testing strategies and conventions |

## Talks to

- **Dev-loop runner** — invokes these tests during autonomous development loops.
- **Health monitoring service** — monitors the results of scheduled test runs.
- **Tool hub** — uses self-hosted git server / GitHub tools to report test status.

## Configuration

Managed via `pytest` configuration files in the root and individual module directories.

---

Part of [Lumina Constellation](../README.md).
