"""Feature: deploy_revision (YAML renderer).

What happens:
  POST /v1/sandboxes/{id}/deploy — render manifests from a workspace path,
  policy-check them, apply into the sandbox namespace, return fidelity + artifacts.
"""

from __future__ import annotations

from common import cleanup, create_sandbox, expect_ok, expect_status, require_controller, scenario


def test_deploy_yaml_broken_probe():
    """Deploy broken probe manifests; expect deployed + fidelity report."""
    client = require_controller()
    created = expect_ok(create_sandbox(client, "deploy-yaml"), "create")
    sid = created["sandbox_id"]
    try:
        resp = client.deploy(
            sid,
            {
                "repository_sha": "abcdef1234567",
                "workspace_path": str(scenario("probe_port_mismatch")),
                "manifests": {"type": "yaml", "path": "deploy/manifests"},
            },
        )
        body = expect_ok(resp, "deploy")
        assert body["status"] == "deployed", body
        assert body["rendered_artifact_ids"], body
        assert "fidelity" in body and "score" in body["fidelity"], body
        assert isinstance(body["fidelity"]["material_gaps"], list), body
    finally:
        cleanup(client, sid)


def test_deploy_missing_workspace_fails():
    """Break attempt: bad workspace_path must fail closed."""
    client = require_controller()
    created = expect_ok(create_sandbox(client, "deploy-badws"), "create")
    sid = created["sandbox_id"]
    try:
        resp = client.deploy(
            sid,
            {
                "repository_sha": "abcdef1234567",
                "workspace_path": "/tmp/raphael-does-not-exist-xyz",
                "manifests": {"type": "yaml", "path": "deploy/manifests"},
            },
        )
        body = expect_status(resp, 400, "deploy-missing-ws")
        assert body["error"]["code"] == "invalid_request", body
    finally:
        cleanup(client, sid)


def test_deploy_missing_manifest_path_fails():
    """Break attempt: yaml path that does not exist → render_failed."""
    client = require_controller()
    created = expect_ok(create_sandbox(client, "deploy-badpath"), "create")
    sid = created["sandbox_id"]
    try:
        resp = client.deploy(
            sid,
            {
                "repository_sha": "abcdef1234567",
                "workspace_path": str(scenario("probe_port_mismatch")),
                "manifests": {"type": "yaml", "path": "deploy/nope"},
            },
        )
        assert resp["status_code"] >= 400, resp
        assert resp["body"]["error"]["code"] in {"render_failed", "invalid_request"}, resp
    finally:
        cleanup(client, sid)


def test_deploy_unknown_manifest_type_fails():
    """Break attempt: manifests.type=terraform (unsupported) must fail."""
    client = require_controller()
    created = expect_ok(create_sandbox(client, "deploy-badtype"), "create")
    sid = created["sandbox_id"]
    try:
        resp = client.deploy(
            sid,
            {
                "repository_sha": "abcdef1234567",
                "workspace_path": str(scenario("probe_port_mismatch")),
                "manifests": {"type": "terraform", "path": "deploy/manifests"},
            },
        )
        body = expect_status(resp, 400, "deploy-bad-type")
        assert body["error"]["code"] == "invalid_request", body
    finally:
        cleanup(client, sid)


def test_deploy_without_sandbox_fails():
    """Break attempt: deploy to missing sandbox_id → 404."""
    client = require_controller()
    resp = client.deploy(
        "sb-missing0000",
        {
            "repository_sha": "abcdef1234567",
            "workspace_path": str(scenario("probe_port_mismatch")),
            "manifests": {"type": "yaml", "path": "deploy/manifests"},
        },
    )
    expect_status(resp, 404, "deploy-missing-sandbox")


def test_deploy_redeploy_overwrites():
    """Redeploy fixed manifests after broken; both should succeed."""
    client = require_controller()
    created = expect_ok(create_sandbox(client, "redeploy"), "create")
    sid = created["sandbox_id"]
    ws = str(scenario("probe_port_mismatch"))
    try:
        b1 = expect_ok(
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
        assert b1["status"] == "deployed"
        b2 = expect_ok(
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
        assert b2["status"] == "deployed"
    finally:
        cleanup(client, sid)


TESTS = [
    ("deploy_yaml_broken", test_deploy_yaml_broken_probe, "Deploy broken YAML scenario"),
    ("deploy_missing_ws", test_deploy_missing_workspace_fails, "Missing workspace fails"),
    ("deploy_missing_path", test_deploy_missing_manifest_path_fails, "Missing manifest path fails"),
    ("deploy_bad_type", test_deploy_unknown_manifest_type_fails, "Unknown renderer type fails"),
    ("deploy_missing_sb", test_deploy_without_sandbox_fails, "Missing sandbox 404"),
    ("deploy_redeploy", test_deploy_redeploy_overwrites, "Redeploy broken→fixed"),
]
