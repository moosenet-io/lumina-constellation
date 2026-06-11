"""Shared pytest fixtures (scaffold only)."""
import pytest

# TODO: Provide mock LiteLLM client
@pytest.fixture
def mock_litellm_client():
    pytest.skip("Scaffold — implement during sprint")

# TODO: Provide mock Engram store/query
@pytest.fixture
def mock_engram():
    pytest.skip("Scaffold — implement during sprint")

# TODO: Provide mock Plane API
@pytest.fixture
def mock_plane_api():
    pytest.skip("Scaffold — implement during sprint")

# Helper: import a module by file path (keeps scaffolds independent of package layout)
def import_module_from_path(module_name: str, module_path: str):
    import importlib.util
    spec = importlib.util.spec_from_file_location(module_name, module_path)
    if spec is None or spec.loader is None:
        raise ImportError(f"Cannot import {module_name} from {module_path}")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod
