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

- **[Vector](../fleet/vector/)** — Vector invokes these tests during dev loops.
- **[Sentinel](../fleet/sentinel/)** — Sentinel monitors the results of scheduled test runs.
- **[Terminus](../terminus/)** — Uses Gitea/GitHub tools to report test status.

## Configuration

Managed via `pytest` configuration files in the root and individual module directories.

---

Part of [Lumina Constellation](../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
