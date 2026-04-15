import pytest

def pytest_configure(config):
    config.addinivalue_line("markers", "spectra: Spectra browser agent tests")
    config.addinivalue_line("markers", "security: Security/adversarial tests")
    config.addinivalue_line("markers", "integration: Integration tests requiring live services")
