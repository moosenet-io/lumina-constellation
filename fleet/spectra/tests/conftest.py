import json
import os
import urllib.request

import pytest

SPECTRA_URL = os.environ.get("SPECTRA_URL", "http://YOUR_FLEET_HOST:8084")
OP_KEY = "MY.1"


def pytest_configure(config):
    config.addinivalue_line("markers", "spectra: Spectra browser agent tests")
    config.addinivalue_line("markers", "security: Security/adversarial tests")
    config.addinivalue_line("markers", "integration: Integration tests requiring live services")


@pytest.fixture(autouse=True)
def close_all_sessions():
    """Close all open Spectra sessions before each test — prevents max-session 429s."""
    _close_sessions()
    yield
    # Also clean up after
    _close_sessions()


def _close_sessions():
    try:
        req = urllib.request.Request(f"{SPECTRA_URL}/sessions?consumer_key={OP_KEY}")
        with urllib.request.urlopen(req, timeout=5) as r:
            data = json.loads(r.read())
        for s in data.get("sessions", []):
            if s.get("state") == "active":
                close_req = urllib.request.Request(
                    f"{SPECTRA_URL}/session/close?session_id={s['id']}&consumer_key={OP_KEY}",
                    data=b"", method="POST",
                )
                try:
                    urllib.request.urlopen(close_req, timeout=3)
                except Exception:
                    pass
    except Exception:
        pass
