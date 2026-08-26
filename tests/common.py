"""Shared helpers for sandbox/tests.

What this layer does:
- Talks to the running sandbox controller over HTTP
- Finds scenario workspaces under sandbox/harness/scenarios
- Provides small assert helpers with clear failure messages
"""

from __future__ import annotations

import os
import sys
import uuid
from pathlib import Path
from typing import Any

# Prefer harness venv httpx if available; otherwise system/httpx.
TESTS_DIR = Path(__file__).resolve().parent
SANDBOX_DIR = TESTS_DIR.parent
REPO_ROOT = SANDBOX_DIR.parent
SCENARIOS = SANDBOX_DIR / "harness" / "scenarios"
HARNESS_VENV_SITE = SANDBOX_DIR / "harness" / ".venv" / "lib"

# Add harness package path so we can reuse ideas; keep our own client here for clarity.
if str(TESTS_DIR) not in sys.path:
    sys.path.insert(0, str(TESTS_DIR))

try:
    import httpx
except ImportError as exc:  # pragma: no cover
    raise SystemExit(
        "httpx is required. Install with:\n"
        "  python3 -m pip install httpx\n"
        "or use the harness venv:\n"
        "  sandbox/harness/.venv/bin/python sandbox/tests/test.py"
    ) from exc


DEFAULT_BASE = os.environ.get("RAPHAEL_SANDBOX_URL", "http://127.0.0.1:8090")



class ApiError(AssertionError):
    """Raised when an expected success call fails."""


class Client:
    """Thin HTTP client. Every mutating call returns {status_code, body}."""

    def __init__(self, base_url: str = DEFAULT_BASE, timeout: float = 180.0) -> None:
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout

    def health(self) -> dict[str, Any]:
        with httpx.Client(base_url=self.base_url, timeout=self.timeout) as c:
            r = c.get("/health")
            return {"status_code": r.status_code, "body": _json_or_text(r)}

    def create(self, body: dict[str, Any]) -> dict[str, Any]:
        with httpx.Client(base_url=self.base_url, timeout=self.timeout) as c:
            r = c.post("/v1/sandboxes", json=body)
            return {"status_code": r.status_code, "body": _json_or_text(r)}

    def deploy(self, sandbox_id: str, body: dict[str, Any]) -> dict[str, Any]:
        with httpx.Client(base_url=self.base_url, timeout=self.timeout) as c:
            r = c.post(f"/v1/sandboxes/{sandbox_id}/deploy", json=body)
            return {"status_code": r.status_code, "body": _json_or_text(r)}

    def observe(self, sandbox_id: str, body: dict[str, Any] | None = None) -> dict[str, Any]:
        with httpx.Client(base_url=self.base_url, timeout=self.timeout) as c:
            r = c.post(f"/v1/sandboxes/{sandbox_id}/observe", json=body or {})
            return {"status_code": r.status_code, "body": _json_or_text(r)}

    def validate(self, sandbox_id: str, body: dict[str, Any]) -> dict[str, Any]:
        with httpx.Client(base_url=self.base_url, timeout=self.timeout) as c:
            r = c.post(f"/v1/sandboxes/{sandbox_id}/validate", json=body)
            return {"status_code": r.status_code, "body": _json_or_text(r)}

    def finalize(self, sandbox_id: str, body: dict[str, Any] | None = None) -> dict[str, Any]:
        with httpx.Client(base_url=self.base_url, timeout=self.timeout) as c:
            r = c.post(f"/v1/sandboxes/{sandbox_id}/finalize", json=body or {})
            return {"status_code": r.status_code, "body": _json_or_text(r)}

    def get_result(self, sandbox_id: str) -> dict[str, Any]:
        with httpx.Client(base_url=self.base_url, timeout=self.timeout) as c:
            r = c.get(f"/v1/sandboxes/{sandbox_id}/result")
            return {"status_code": r.status_code, "body": _json_or_text(r)}

    def destroy(self, sandbox_id: str, body: dict[str, Any] | None = None) -> dict[str, Any]:
        with httpx.Client(base_url=self.base_url, timeout=self.timeout) as c:
            r = c.post(f"/v1/sandboxes/{sandbox_id}/destroy", json=body or {})
            return {"status_code": r.status_code, "body": _json_or_text(r)}

    def force_cleanup(self, body: dict[str, Any] | None = None) -> dict[str, Any]:
        with httpx.Client(base_url=self.base_url, timeout=self.timeout) as c:
            r = c.post("/v1/admin/force-cleanup", json=body or {})
            return {"status_code": r.status_code, "body": _json_or_text(r)}


def _json_or_text(r: httpx.Response) -> Any:
    try:
        return r.json()
    except Exception:
        return {"raw": r.text}


def new_client() -> Client:
    return Client()


def require_controller(client: Client | None = None) -> Client:
    client = client or new_client()
    try:
        h = client.health()
    except Exception as exc:  # noqa: BLE001
        raise SystemExit(
            f"Controller not reachable at {client.base_url}: {exc}\n"
            "Start it first:\n"
            "  cd sandbox/controller\n"
            "  RAPHAEL_CLUSTER_BACKEND=mock RAPHAEL_LISTEN=127.0.0.1:8090 cargo run"
        ) from exc
    if h["status_code"] != 200:
        raise SystemExit(f"Controller /health returned {h}")
    return client


def create_sandbox(
    client: Client,
    suffix: str,
    *,
    commit_sha: str = "abcdef1234567",
    timeout_minutes: int = 20,
    extra: dict[str, Any] | None = None,
) -> dict[str, Any]:
    body: dict[str, Any] = {
        "run_id": f"manual-{suffix}-{uuid.uuid4().hex[:8]}",
        "tenant_id": "local-dev",
        "repository": {"owner": "raphael", "name": "demo"},
        "commit_sha": commit_sha,
        "timeout_minutes": timeout_minutes,
        "secret_fixture_set": "payments-test",
    }
    if extra:
        body.update(extra)
    return client.create(body)


def expect_ok(resp: dict[str, Any], what: str) -> dict[str, Any]:
    if resp["status_code"] >= 400:
        raise ApiError(f"{what} failed: HTTP {resp['status_code']} body={resp['body']!r}")
    return resp["body"]


def expect_status(resp: dict[str, Any], code: int, what: str) -> dict[str, Any]:
    if resp["status_code"] != code:
        raise ApiError(
            f"{what}: expected HTTP {code}, got {resp['status_code']} body={resp['body']!r}"
        )
    return resp["body"]


def scenario(name: str) -> Path:
    path = SCENARIOS / name
    if not path.exists():
        raise FileNotFoundError(f"scenario missing: {path}")
    return path


def cleanup(client: Client, sandbox_id: str | None) -> None:
    if not sandbox_id:
        return
    client.destroy(sandbox_id, {"reason": "test-cleanup"})
