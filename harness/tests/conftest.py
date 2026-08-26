from __future__ import annotations

from pathlib import Path

import pytest

from client import SandboxClient

SCENARIOS = Path(__file__).resolve().parents[1] / "scenarios"


@pytest.fixture(scope="session")
def client() -> SandboxClient:
    return SandboxClient()


@pytest.fixture(scope="session", autouse=True)
def require_controller(client: SandboxClient):
    try:
        health = client.health()
    except Exception as exc:  # noqa: BLE001
        pytest.skip(f"sandbox controller not reachable: {exc}")
    assert health.get("status") == "ok"


@pytest.fixture
def scenarios_root() -> Path:
    return SCENARIOS
