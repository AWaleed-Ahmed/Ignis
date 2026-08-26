"""Feature: destroy_sandbox.

What happens:
  POST /v1/sandboxes/{id}/destroy — deletes the namespace and marks sandbox destroyed.
  Must be idempotent: destroying twice is success, not a crash.
"""

from __future__ import annotations

from common import cleanup, create_sandbox, expect_ok, require_controller


def test_destroy_happy_and_idempotent():
    """Destroy once → destroyed/already_destroyed; second call stays safe."""
    client = require_controller()
    created = expect_ok(create_sandbox(client, "destroy-ok"), "create")
    sid = created["sandbox_id"]
    d1 = expect_ok(client.destroy(sid, {"reason": "first"}), "destroy-1")
    assert d1["status"] in {"destroyed", "already_destroyed"}, d1
    d2 = expect_ok(client.destroy(sid, {"reason": "second"}), "destroy-2")
    assert d2["status"] == "already_destroyed", d2


def test_destroy_unknown_id_is_safe():
    """Break attempt: random sandbox_id should not 500; treat as already gone."""
    client = require_controller()
    body = expect_ok(client.destroy("sb-doesnotexist99", {"reason": "ghost"}), "destroy-unknown")
    assert body["status"] == "already_destroyed", body


def test_ops_after_destroy_fail():
    """Break attempt: deploy/observe after destroy must not succeed as ready."""
    client = require_controller()
    created = expect_ok(create_sandbox(client, "post-destroy"), "create")
    sid = created["sandbox_id"]
    expect_ok(client.destroy(sid), "destroy")
    deploy = client.deploy(
        sid,
        {
            "repository_sha": "abcdef1234567",
            "workspace_path": str(
                __import__("common", fromlist=["scenario"]).scenario("probe_port_mismatch")
            ),
            "manifests": {"type": "yaml", "path": "deploy/manifests"},
        },
    )
    assert deploy["status_code"] == 404, deploy
    observe = client.observe(sid, {})
    assert observe["status_code"] == 404, observe


def test_destroy_stress_twenty():
    """Create+destroy 20 sandboxes; every destroy must succeed."""
    client = require_controller()
    ids = []
    for i in range(20):
        body = expect_ok(create_sandbox(client, f"dstress{i}", timeout_minutes=5), f"create-{i}")
        ids.append(body["sandbox_id"])
    for sid in ids:
        d1 = expect_ok(client.destroy(sid), f"destroy-{sid}")
        assert d1["status"] in {"destroyed", "already_destroyed"}
        d2 = expect_ok(client.destroy(sid), f"redestroy-{sid}")
        assert d2["status"] == "already_destroyed"


TESTS = [
    ("destroy_idempotent", test_destroy_happy_and_idempotent, "Destroy twice safely"),
    ("destroy_unknown", test_destroy_unknown_id_is_safe, "Unknown id is already_destroyed"),
    ("destroy_blocks_later_ops", test_ops_after_destroy_fail, "Ops after destroy return 404"),
    ("destroy_stress_20", test_destroy_stress_twenty, "Create/destroy x20 stress"),
]
