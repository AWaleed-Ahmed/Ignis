"""HTTP client helpers for the sandbox controller."""

from __future__ import annotations

import os
from typing import Any

import httpx

DEFAULT_BASE = os.environ.get("RAPHAEL_SANDBOX_URL", "http://127.0.0.1:8090")


class SandboxClient:
    def __init__(self, base_url: str = DEFAULT_BASE, timeout: float = 30.0) -> None:
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout

    def health(self) -> dict[str, Any]:
        with httpx.Client(base_url=self.base_url, timeout=self.timeout) as client:
            r = client.get("/health")
            r.raise_for_status()
            return r.json()

    def create(self, body: dict[str, Any]) -> dict[str, Any]:
        with httpx.Client(base_url=self.base_url, timeout=self.timeout) as client:
            r = client.post("/v1/sandboxes", json=body)
            if r.status_code >= 400:
                raise AssertionError(f"create failed: {r.status_code} {r.text}")
            return r.json()

    def deploy(self, sandbox_id: str, body: dict[str, Any]) -> dict[str, Any]:
        with httpx.Client(base_url=self.base_url, timeout=self.timeout) as client:
            r = client.post(f"/v1/sandboxes/{sandbox_id}/deploy", json=body)
            return {"status_code": r.status_code, "body": r.json()}

    def observe(self, sandbox_id: str, body: dict[str, Any] | None = None) -> dict[str, Any]:
        with httpx.Client(base_url=self.base_url, timeout=self.timeout) as client:
            r = client.post(f"/v1/sandboxes/{sandbox_id}/observe", json=body or {})
            if r.status_code >= 400:
                raise AssertionError(f"observe failed: {r.status_code} {r.text}")
            return r.json()

    def validate(self, sandbox_id: str, body: dict[str, Any]) -> dict[str, Any]:
        with httpx.Client(base_url=self.base_url, timeout=self.timeout) as client:
            r = client.post(f"/v1/sandboxes/{sandbox_id}/validate", json=body)
            if r.status_code >= 400:
                raise AssertionError(f"validate failed: {r.status_code} {r.text}")
            return r.json()

    def finalize(self, sandbox_id: str, body: dict[str, Any] | None = None) -> dict[str, Any]:
        with httpx.Client(base_url=self.base_url, timeout=self.timeout) as client:
            r = client.post(f"/v1/sandboxes/{sandbox_id}/finalize", json=body or {})
            return {"status_code": r.status_code, "body": r.json()}

    def get_result(self, sandbox_id: str) -> dict[str, Any]:
        with httpx.Client(base_url=self.base_url, timeout=self.timeout) as client:
            r = client.get(f"/v1/sandboxes/{sandbox_id}/result")
            return {"status_code": r.status_code, "body": r.json()}

    def destroy(self, sandbox_id: str, body: dict[str, Any] | None = None) -> dict[str, Any]:
        with httpx.Client(base_url=self.base_url, timeout=self.timeout) as client:
            r = client.post(f"/v1/sandboxes/{sandbox_id}/destroy", json=body or {})
            if r.status_code >= 400:
                raise AssertionError(f"destroy failed: {r.status_code} {r.text}")
            return r.json()
