"""Feature: create_sandbox.

What happens:
  POST /v1/sandboxes — creates an isolated namespace (or mock equivalent),
  returns sandbox_id + namespace + expiry. This is the start of a run.
"""

from __future__ import annotations

from common import cleanup, create_sandbox, expect_ok, expect_status, require_controller


def test_create_happy_path():
    """Valid create returns ready sandbox with raphael-run-* namespace."""
    client = require_controller()
    resp = create_sandbox(client, "create-ok")
    body = expect_ok(resp, "create")
    try:
        assert body["status"] == "ready", body
        assert body["sandbox_id"].startswith("sb-"), body
        assert body["namespace"].startswith("raphael-run-"), body
        assert "expires_at" in body and "created_at" in body
        assert body.get("service_account") == "raphael-sandbox-sa"
    finally:
        cleanup(client, body.get("sandbox_id"))


def test_create_rejects_short_commit():
    """Break attempt: commit_sha shorter than 7 chars must fail."""
    client = require_controller()
    resp = create_sandbox(client, "bad-sha", commit_sha="abc")
    body = expect_status(resp, 400, "create-short-sha")
    assert body["error"]["code"] == "invalid_request", body


def test_create_rejects_empty_run_id():
    """Break attempt: empty run_id must fail."""
    client = require_controller()
    resp = client.create(
        {
            "run_id": "   ",
            "tenant_id": "t",
            "repository": {"owner": "a", "name": "b"},
            "commit_sha": "abcdef1",
        }
    )
    body = expect_status(resp, 400, "create-empty-run")
    assert body["error"]["code"] == "invalid_request", body


def test_create_duplicate_active_run_conflicts():
    """Break attempt: same run_id while sandbox still ready should conflict."""
    client = require_controller()
    run_id = "manual-dup-run-fixed"
    first = client.create(
        {
            "run_id": run_id,
            "tenant_id": "t",
            "repository": {"owner": "a", "name": "b"},
            "commit_sha": "abcdef1234567",
        }
    )
    body1 = expect_ok(first, "create-first")
    sid = body1["sandbox_id"]
    try:
        second = client.create(
            {
                "run_id": run_id,
                "tenant_id": "t",
                "repository": {"owner": "a", "name": "b"},
                "commit_sha": "abcdef1234567",
            }
        )
        # Same run_id hashes to same sandbox_id → conflict on insert
        assert second["status_code"] in {409, 400}, second
        assert "error" in second["body"], second
    finally:
        cleanup(client, sid)


def test_create_timeout_clamped_fields_accepted():
    """timeout_minutes at edges should still create (clamped server-side)."""
    client = require_controller()
    resp = create_sandbox(client, "timeout", timeout_minutes=1)
    body = expect_ok(resp, "create-timeout-1")
    cleanup(client, body["sandbox_id"])


TESTS = [
    ("create_happy", test_create_happy_path, "Create returns ready sandbox"),
    ("create_short_sha", test_create_rejects_short_commit, "Reject short commit_sha"),
    ("create_empty_run", test_create_rejects_empty_run_id, "Reject empty run_id"),
    ("create_duplicate", test_create_duplicate_active_run_conflicts, "Duplicate run conflicts"),
    ("create_timeout", test_create_timeout_clamped_fields_accepted, "Accept small timeout"),
]
