"""Feature: observe_failure.

What happens:
  POST /v1/sandboxes/{id}/observe — look at the deployed workload and return a
  structured failure_signature (class + key + evidence), not free-form prose.
"""

from __future__ import annotations

from common import cleanup, create_sandbox, expect_ok, require_controller, scenario


def _deploy(client, sid: str, scenario_name: str, path: str = "deploy/manifests") -> None:
    expect_ok(
        client.deploy(
            sid,
            {
                "repository_sha": "abcdef1234567",
                "workspace_path": str(scenario(scenario_name)),
                "manifests": {"type": "yaml", "path": path},
            },
        ),
        f"deploy-{scenario_name}",
    )


def test_observe_probe_misconfiguration():
    """Broken readiness probe → probe_misconfiguration signature."""
    client = require_controller()
    created = expect_ok(create_sandbox(client, "obs-probe"), "create")
    sid = created["sandbox_id"]
    try:
        _deploy(client, sid, "probe_port_mismatch")
        body = expect_ok(client.observe(sid, {}), "observe")
        sig = body["signature"]
        assert sig["class"] == "probe_misconfiguration", sig
        assert sig["key"].startswith("probe_port_mismatch:payments-api:"), sig
        assert sig["reproduced"] is True
        assert sig["evidence_refs"], sig
    finally:
        cleanup(client, sid)


def test_observe_bad_image():
    client = require_controller()
    created = expect_ok(create_sandbox(client, "obs-image"), "create")
    sid = created["sandbox_id"]
    try:
        _deploy(client, sid, "bad_image")
        sig = expect_ok(client.observe(sid), "observe")["signature"]
        assert sig["class"] == "bad_image_reference", sig
    finally:
        cleanup(client, sid)


def test_observe_missing_configmap_key():
    client = require_controller()
    created = expect_ok(create_sandbox(client, "obs-cm"), "create")
    sid = created["sandbox_id"]
    try:
        _deploy(client, sid, "missing_configmap_key")
        sig = expect_ok(client.observe(sid), "observe")["signature"]
        assert sig["class"] == "invalid_missing_config", sig
    finally:
        cleanup(client, sid)


def test_observe_oom():
    client = require_controller()
    created = expect_ok(create_sandbox(client, "obs-oom"), "create")
    sid = created["sandbox_id"]
    try:
        _deploy(client, sid, "resource_oom")
        sig = expect_ok(client.observe(sid), "observe")["signature"]
        assert sig["class"] == "resource_constraint", sig
    finally:
        cleanup(client, sid)


def test_observe_service_port_mismatch():
    client = require_controller()
    created = expect_ok(create_sandbox(client, "obs-svc"), "create")
    sid = created["sandbox_id"]
    try:
        _deploy(client, sid, "service_port_mismatch")
        sig = expect_ok(client.observe(sid), "observe")["signature"]
        assert sig["class"] == "service_port_mismatch", sig
    finally:
        cleanup(client, sid)


def test_observe_before_deploy_fails():
    """Break attempt: observe with nothing deployed should fail."""
    client = require_controller()
    created = expect_ok(create_sandbox(client, "obs-empty"), "create")
    sid = created["sandbox_id"]
    try:
        resp = client.observe(sid, {})
        assert resp["status_code"] >= 400, resp
        assert resp["body"]["error"]["code"] in {
            "observation_failed",
            "not_found",
            "internal",
        }, resp
    finally:
        cleanup(client, sid)


def test_observe_expected_key_match_flag():
    """expected_signature_key should set matched_expected true/false."""
    client = require_controller()
    created = expect_ok(create_sandbox(client, "obs-match"), "create")
    sid = created["sandbox_id"]
    try:
        _deploy(client, sid, "probe_port_mismatch")
        first = expect_ok(client.observe(sid), "observe-1")
        key = first["signature"]["key"]
        matched = expect_ok(
            client.observe(sid, {"expected_signature_key": key}),
            "observe-match",
        )
        assert matched["matched_expected"] is True, matched
        mismatched = expect_ok(
            client.observe(sid, {"expected_signature_key": "not-the-key"}),
            "observe-mismatch",
        )
        assert mismatched["matched_expected"] is False, mismatched
    finally:
        cleanup(client, sid)


def test_observe_fixed_becomes_healthy():
    """After deploying fixed manifests, signature class should be healthy."""
    client = require_controller()
    created = expect_ok(create_sandbox(client, "obs-fix"), "create")
    sid = created["sandbox_id"]
    try:
        _deploy(client, sid, "probe_port_mismatch", "deploy/manifests")
        broken = expect_ok(client.observe(sid), "observe-broken")
        assert broken["signature"]["class"] == "probe_misconfiguration"
        _deploy(client, sid, "probe_port_mismatch", "deploy/manifests_fixed")
        fixed = expect_ok(client.observe(sid), "observe-fixed")
        assert fixed["signature"]["class"] == "healthy", fixed
    finally:
        cleanup(client, sid)


TESTS = [
    ("observe_probe", test_observe_probe_misconfiguration, "Probe mismatch signature"),
    ("observe_bad_image", test_observe_bad_image, "Bad image signature"),
    ("observe_missing_cm", test_observe_missing_configmap_key, "Missing ConfigMap key"),
    ("observe_oom", test_observe_oom, "OOM / resource constraint"),
    ("observe_service_port", test_observe_service_port_mismatch, "Service port mismatch"),
    ("observe_before_deploy", test_observe_before_deploy_fails, "Observe with no deploy fails"),
    ("observe_match_flag", test_observe_expected_key_match_flag, "expected_signature_key flag"),
    ("observe_fixed_healthy", test_observe_fixed_becomes_healthy, "Fixed deploy → healthy"),
]
