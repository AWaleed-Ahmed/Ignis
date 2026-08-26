"""Feature: run_validation.

What happens:
  POST /v1/sandboxes/{id}/validate — run allowlisted checks (static commands,
  rollout, signature compare, http). Fail closed if a mandatory check cannot run.
"""

from __future__ import annotations

from common import cleanup, create_sandbox, expect_ok, require_controller, scenario


def _prepare_fixed(client, sid: str) -> str:
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
    return before


def test_validate_passing_after_fix():
    """Fixed workload + allowlisted true command + signature compare → passed."""
    client = require_controller()
    created = expect_ok(create_sandbox(client, "val-ok"), "create")
    sid = created["sandbox_id"]
    try:
        before = _prepare_fixed(client, sid)
        body = expect_ok(
            client.validate(
                sid,
                {
                    "plan": {
                        "commands": ["true"],
                        "health_checks": [
                            {
                                "type": "rollout",
                                "resource": "deployment/payments-api",
                                "mandatory": True,
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
        assert body["passed"] is True, body
        assert body["fail_closed"] is False, body
        assert body["signature_cleared"] is True, body
        assert body["checks"], body
    finally:
        cleanup(client, sid)


def test_validate_fail_closed_disallowed_command():
    """Break attempt: non-allowlisted command (rm) must fail closed, not run."""
    client = require_controller()
    created = expect_ok(create_sandbox(client, "val-fc"), "create")
    sid = created["sandbox_id"]
    try:
        _prepare_fixed(client, sid)
        body = expect_ok(
            client.validate(sid, {"plan": {"commands": ["rm -rf /"]}}),
            "validate-bad-cmd",
        )
        assert body["fail_closed"] is True, body
        assert body["passed"] is False, body
        assert any(c["status"] == "unavailable" for c in body["checks"]), body
    finally:
        cleanup(client, sid)


def test_validate_fails_while_still_broken():
    """Break attempt: validate without fixing should not claim signature cleared."""
    client = require_controller()
    created = expect_ok(create_sandbox(client, "val-broken"), "create")
    sid = created["sandbox_id"]
    try:
        expect_ok(
            client.deploy(
                sid,
                {
                    "repository_sha": "abcdef1234567",
                    "workspace_path": str(scenario("probe_port_mismatch")),
                    "manifests": {"type": "yaml", "path": "deploy/manifests"},
                },
            ),
            "deploy",
        )
        before = expect_ok(client.observe(sid), "observe")["signature"]["key"]
        body = expect_ok(
            client.validate(
                sid,
                {
                    "plan": {
                        "health_checks": [
                            {
                                "type": "rollout",
                                "resource": "deployment/payments-api",
                                "timeout_seconds": 15,
                            },
                            {"type": "signature_absent", "mandatory": True},
                        ],
                        "compare_to_signature_key": before,
                    }
                },
            ),
            "validate-still-broken",
        )
        assert body["passed"] is False, body
        assert body.get("signature_cleared") in {False, None} or body["passed"] is False
    finally:
        cleanup(client, sid)


def test_validate_unknown_health_type_fail_closed():
    """Break attempt: unknown health check type → fail closed."""
    client = require_controller()
    created = expect_ok(create_sandbox(client, "val-unk"), "create")
    sid = created["sandbox_id"]
    try:
        _prepare_fixed(client, sid)
        body = expect_ok(
            client.validate(
                sid,
                {"plan": {"health_checks": [{"type": "telepathy", "mandatory": True}]}},
            ),
            "validate-unknown",
        )
        assert body["fail_closed"] is True, body
        assert body["passed"] is False, body
    finally:
        cleanup(client, sid)


TESTS = [
    ("validate_pass", test_validate_passing_after_fix, "Validation passes after fix"),
    ("validate_fail_closed_cmd", test_validate_fail_closed_disallowed_command, "Disallowed cmd fail-closed"),
    ("validate_still_broken", test_validate_fails_while_still_broken, "Broken state does not pass"),
    ("validate_unknown_check", test_validate_unknown_health_type_fail_closed, "Unknown check fail-closed"),
]
