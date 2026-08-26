from __future__ import annotations

import uuid

from client import SandboxClient


def test_create_destroy_twenty_times_no_leak(client: SandboxClient):
    ids = []
    for i in range(20):
        created = client.create(
            {
                "run_id": f"stress-{i}-{uuid.uuid4().hex[:6]}",
                "tenant_id": "local-dev",
                "repository": {"owner": "raphael", "name": "demo"},
                "commit_sha": "abcdef1234567",
                "timeout_minutes": 5,
            }
        )
        ids.append(created["sandbox_id"])
    for sid in ids:
        assert client.destroy(sid)["status"] in {"destroyed", "already_destroyed"}
        assert client.destroy(sid)["status"] == "already_destroyed"
