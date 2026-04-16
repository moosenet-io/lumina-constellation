# ✦ Tests

> "Trust, but verify. Then verify again."

Integration tests, adversarial security audits, and deployment reconciliation for Lumina Constellation.

## Test suites

| Location | What it covers |
|----------|---------------|
| `fleet/spectra/tests/test_spectra.py` | Spectra — Docker service, network isolation, sanitization, access control, MCP tools |
| `fleet/dura/dura_smoke_test.py` | Dura smoke test — calls every critical MCP tool, verifies response schema |

## Running

```bash
# Spectra tests (requires live Spectra service)
SPECTRA_URL=http://your-fleet-host:8084 pytest -m spectra fleet/spectra/tests/ -v

# Dura smoke test
python3 fleet/dura/dura_smoke_test.py --quick
```

## pytest markers

| Marker | Tests |
|--------|-------|
| `spectra` | Spectra browser agent tests |
| `security` | Adversarial/security-focused tests |
| `integration` | Tests requiring live running services |

Conftest autouse fixture closes all sessions before each test — prevents cascading 429s from max-session limits.

---

Part of [Lumina Constellation](../README.md) · Built by [MooseNet](https://github.com/moosenet-io)
