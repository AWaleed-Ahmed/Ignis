"""Feature: finalize_result + GET result.

What happens:
  After validation passes, POST .../finalize freezes an immutable validated-fix
  record and returns result_id. GET .../result reads it back.
  Sandbox does NOT open a GitHub PR — that is later agent work.
"""

from __future__ import annotations

from common import cleanup, create_sandbox, expect_ok, expect_status, require_controller, scenario


def _pipeline_to_validated(client, sid: str) -> str:
    ws = str(scenario("probe_port_mismatch"))
    expect_ok(
        client.deploy(
            sid,
            {
                "repository_sha": "abcdef1234567",
                "workspace_path": ws,
                "manifests": {"type": "yaml", "path": "deploy/manifests"},
            },
        ),
        "deploy-broken",
    )
    before = expect_ok(client.observe(sid), "observe-broken")["signature"]["key"]
    expect_ok(
        client.deploy(
            sid,
            {
                "repository_sha": "abcdef1234567",
                "workspace_path": ws,
                "manifests": {"type": "yaml", "path": "deploy/manifests_fixed"},
            },
        ),
        "deploy-fixed",
    )
    expect_ok(client.observe(sid), "observe-fixed")
    expect_ok(
        client.validate(
            sid,
            {
                "plan": {
                    "commands": ["true"],
                    "health_checks": [
                        {
                            "type": "rollout",
                            "resource": "deployment/payments-api",
                            "timeout_seconds": 90,
                        },
                        {"type": "signature_absent", "mandatory": True},
                    ],
                    "compare_to_signature_key": before,
                }
            },
        ),
        "validate",
    )
    return before


def test_finalize_happy_and_idempotent():
    """Finalize after pass → result_id; second finalize → already_finalized."""
    client = require_controller()
    created = expect_ok(create_sandbox(client, "fin-ok"), "create")
    sid = created["sandbox_id"]
    try:
        _pipeline_to_validated(client, sid)
        first = expect_ok(
            client.finalize(sid, {"notes": "manual test freeze"}),
            "finalize-1",
        )
        assert first["status"] == "finalized", first
        assert first["result_id"].startswith("res-"), first
        assert first["record"]["validation"]["passed"] is True
        assert first["record"]["before_signature"]["class"] == "probe_misconfiguration"
        assert first["record"]["after_signature"]["class"] == "healthy"
        assert first["record"]["content_hash"], first

        second = expect_ok(client.finalize(sid, {}), "finalize-2")
        assert second["status"] == "already_finalized", second
        assert second["result_id"] == first["result_id"]

        fetched = expect_ok(client.get_result(sid), "get-result")
        assert fetched["result_id"] == first["result_id"]
    finally:
        cleanup(client, sid)


def test_finalize_without_validation_fails():
    """Break attempt: finalize right after create must fail."""
    client = require_controller()
    created = expect_ok(create_sandbox(client, "fin-early"), "create")
    sid = created["sandbox_id"]
    try:
        body = expect_status(client.finalize(sid, {}), 400, "finalize-early")
        assert body["error"]["code"] == "invalid_request", body
        missing = expect_status(client.get_result(sid), 404, "get-result-missing")
        assert "error" in missing
    finally:
        cleanup(client, sid)


def test_finalize_require_patch_without_patch_fails():
    """Break attempt: require_patch=true but deploy used path-only fix (no patch)."""
    client = require_controller()
    created = expect_ok(create_sandbox(client, "fin-patch"), "create")
    sid = created["sandbox_id"]
    try:
        _pipeline_to_validated(client, sid)
        resp = client.finalize(sid, {"require_patch": True})
        assert resp["status_code"] == 400, resp
        assert resp["body"]["error"]["code"] == "invalid_request", resp
    finally:
        cleanup(client, sid)


def test_redeploy_clears_finalize():
    """After finalize, a new deploy should clear frozen result (must finalize again)."""
    client = require_controller()
    created = expect_ok(create_sandbox(client, "fin-clear"), "create")
    sid = created["sandbox_id"]
    ws = str(scenario("probe_port_mismatch"))
    try:
        _pipeline_to_validated(client, sid)
        expect_ok(client.finalize(sid, {}), "finalize")
        expect_ok(client.get_result(sid), "get-before-clear")
        # Redeploy clears finalize
        expect_ok(
            client.deploy(
                sid,
                {
                    "repository_sha": "abcdef1234567",
                    "workspace_path": ws,
                    "manifests": {"type": "yaml", "path": "deploy/manifests_fixed"},
                },
            ),
            "redeploy",
        )
        expect_status(client.get_result(sid), 404, "get-after-clear")
    finally:
        cleanup(client, sid)


TESTS = [
    ("finalize_happy", test_finalize_happy_and_idempotent, "Finalize + idempotent + GET"),
    ("finalize_too_early", test_finalize_without_validation_fails, "Finalize without validate fails"),
    ("finalize_require_patch", test_finalize_require_patch_without_patch_fails, "require_patch without patch"),
    ("finalize_cleared_on_redeploy", test_redeploy_clears_finalize, "Redeploy clears frozen result"),
]
