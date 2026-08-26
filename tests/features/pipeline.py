"""Feature: full pipeline stress + break-the-happy-path attempts.

What happens end-to-end:
  create → deploy broken → observe → deploy fixed → validate → finalize → destroy
"""

from __future__ import annotations

import concurrent.futures

from common import cleanup, create_sandbox, expect_ok, require_controller, scenario


def _full_pipeline(client, suffix: str) -> str:
    created = expect_ok(create_sandbox(client, suffix, timeout_minutes=10), "create")
    sid = created["sandbox_id"]
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
    after = expect_ok(client.observe(sid), "observe-fixed")["signature"]
    assert after["class"] == "healthy", after
    val = expect_ok(
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
    assert val["passed"] is True, val
    fin = expect_ok(client.finalize(sid, {"notes": f"pipeline-{suffix}"}), "finalize")
    assert fin["status"] in {"finalized", "already_finalized"}, fin
    return sid


def test_full_pipeline_once():
    """One complete happy path including finalize + destroy."""
    client = require_controller()
    sid = _full_pipeline(client, "pipe-once")
    expect_ok(client.destroy(sid), "destroy")
    expect_ok(client.destroy(sid), "destroy-again")


def test_full_pipeline_serial_stress_five():
    """Run full pipeline 5 times in a row (serial stress)."""
    client = require_controller()
    ids = []
    try:
        for i in range(5):
            ids.append(_full_pipeline(client, f"pipe-s{i}"))
    finally:
        for sid in ids:
            cleanup(client, sid)


def test_full_pipeline_parallel_stress_four():
    """Run 4 pipelines concurrently — looks for races in registry/mock."""
    client = require_controller()

    def one(i: int) -> str:
        # Each thread gets its own client instance.
        local = require_controller()
        return _full_pipeline(local, f"pipe-p{i}")

    ids: list[str] = []
    try:
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            futures = [pool.submit(one, i) for i in range(4)]
            for fut in concurrent.futures.as_completed(futures):
                ids.append(fut.result())
    finally:
        for sid in ids:
            cleanup(client, sid)


def test_break_finalize_then_validate_again_requires_refinalize():
    """Validate again after finalize clears freeze; GET result becomes 404 until re-finalize."""
    client = require_controller()
    sid = _full_pipeline(client, "pipe-reval")
    try:
        expect_ok(client.get_result(sid), "get-1")
        # Re-validate clears finalize (per service rules)
        before = "probe_port_mismatch:payments-api:8080!=9090"
        expect_ok(
            client.validate(
                sid,
                {
                    "plan": {
                        "commands": ["true"],
                        "health_checks": [{"type": "signature_absent", "mandatory": True}],
                        "compare_to_signature_key": before,
                    }
                },
            ),
            "re-validate",
        )
        missing = client.get_result(sid)
        assert missing["status_code"] == 404, missing
        expect_ok(client.finalize(sid, {}), "re-finalize")
        expect_ok(client.get_result(sid), "get-2")
    finally:
        cleanup(client, sid)


TESTS = [
    ("pipeline_once", test_full_pipeline_once, "Full happy path once"),
    ("pipeline_serial_5", test_full_pipeline_serial_stress_five, "Full pipeline x5 serial"),
    ("pipeline_parallel_4", test_full_pipeline_parallel_stress_four, "Full pipeline x4 parallel"),
    ("pipeline_revalidate_clears", test_break_finalize_then_validate_again_requires_refinalize, "Re-validate clears finalize"),
]
