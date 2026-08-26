from __future__ import annotations

import uuid
from pathlib import Path

import pytest

from client import SandboxClient

SCENARIOS = Path(__file__).resolve().parents[1] / "scenarios"


def _create(client: SandboxClient, suffix: str) -> dict:
    run_id = f"test-{suffix}-{uuid.uuid4().hex[:8]}"
    return client.create(
        {
            "run_id": run_id,
            "tenant_id": "local-dev",
            "repository": {"owner": "raphael", "name": "demo"},
            "commit_sha": "abcdef1234567",
            "timeout_minutes": 20,
            "secret_fixture_set": "payments-test",
        }
    )


def test_health(client: SandboxClient):
    assert client.health()["service"] == "raphael-sandbox-controller"


def test_create_destroy_idempotent(client: SandboxClient):
    created = _create(client, "lifecycle")
    sid = created["sandbox_id"]
    assert created["status"] == "ready"
    assert created["namespace"].startswith("raphael-run-")

    d1 = client.destroy(sid, {"reason": "test"})
    assert d1["status"] in {"destroyed", "already_destroyed"}
    d2 = client.destroy(sid, {"reason": "test-again"})
    assert d2["status"] == "already_destroyed"


def test_probe_port_mismatch_reproduce_and_fix(client: SandboxClient):
    workspace = SCENARIOS / "probe_port_mismatch"
    created = _create(client, "probe")
    sid = created["sandbox_id"]

    deployed = client.deploy(
        sid,
        {
            "repository_sha": "abcdef1234567",
            "workspace_path": str(workspace),
            "manifests": {"type": "yaml", "path": "deploy/manifests"},
            "wait_seconds": 5,
        },
    )
    assert deployed["status_code"] == 200
    assert deployed["body"]["status"] == "deployed"
    assert "fidelity" in deployed["body"]
    assert isinstance(deployed["body"]["fidelity"]["material_gaps"], list)

    observed = client.observe(sid, {})
    assert observed["signature"]["class"] == "probe_misconfiguration"
    assert observed["signature"]["key"].startswith("probe_port_mismatch:payments-api:")
    assert observed["signature"]["reproduced"] is True

    before_key = observed["signature"]["key"]

    fixed = client.deploy(
        sid,
        {
            "repository_sha": "abcdef1234567",
            "workspace_path": str(workspace),
            "manifests": {"type": "yaml", "path": "deploy/manifests_fixed"},
            "wait_seconds": 5,
        },
    )
    assert fixed["status_code"] == 200

    after = client.observe(sid, {})
    assert after["signature"]["class"] == "healthy"

    validation = client.validate(
        sid,
        {
            "plan": {
                "commands": ["true"],
                "health_checks": [
                    {"type": "rollout", "resource": "deployment/payments-api", "mandatory": True},
                    {
                        "type": "signature_absent",
                        "mandatory": True,
                    },
                ],
                "compare_to_signature_key": before_key,
            }
        },
    )
    assert validation["fail_closed"] is False
    assert validation["passed"] is True
    assert validation["signature_cleared"] is True

    finalized = client.finalize(sid, {"notes": "probe port fix validated"})
    assert finalized["status_code"] == 200, finalized
    assert finalized["body"]["status"] == "finalized"
    assert finalized["body"]["result_id"].startswith("res-")
    assert finalized["body"]["record"]["validation"]["passed"] is True
    assert finalized["body"]["record"]["before_signature"]["class"] == "probe_misconfiguration"
    assert finalized["body"]["record"]["after_signature"]["class"] == "healthy"

    again = client.finalize(sid, {})
    assert again["status_code"] == 200
    assert again["body"]["status"] == "already_finalized"
    assert again["body"]["result_id"] == finalized["body"]["result_id"]

    fetched = client.get_result(sid)
    assert fetched["status_code"] == 200
    assert fetched["body"]["result_id"] == finalized["body"]["result_id"]

    client.destroy(sid)


def test_finalize_fails_without_validation(client: SandboxClient):
    created = _create(client, "nofinal")
    sid = created["sandbox_id"]
    result = client.finalize(sid, {})
    assert result["status_code"] == 400
    assert result["body"]["error"]["code"] == "invalid_request"
    client.destroy(sid)


def test_policy_blocks_privileged(client: SandboxClient):
    workspace = SCENARIOS / "policy_privileged"
    created = _create(client, "policy")
    sid = created["sandbox_id"]
    deployed = client.deploy(
        sid,
        {
            "repository_sha": "abcdef1234567",
            "workspace_path": str(workspace),
            "manifests": {"type": "yaml", "path": "deploy/manifests"},
        },
    )
    assert deployed["status_code"] == 422
    assert deployed["body"]["error"]["code"] == "policy_blocked"
    client.destroy(sid)


@pytest.mark.parametrize(
    "scenario,expected_class",
    [
        ("bad_image", "bad_image_reference"),
        ("missing_configmap_key", "invalid_missing_config"),
        ("resource_oom", "resource_constraint"),
        ("service_port_mismatch", "service_port_mismatch"),
    ],
)
def test_yaml_failure_classes(client: SandboxClient, scenario: str, expected_class: str):
    workspace = SCENARIOS / scenario
    created = _create(client, scenario.replace("_", "")[:12])
    sid = created["sandbox_id"]
    deployed = client.deploy(
        sid,
        {
            "repository_sha": "abcdef1234567",
            "workspace_path": str(workspace),
            "manifests": {"type": "yaml", "path": "deploy/manifests"},
        },
    )
    assert deployed["status_code"] == 200
    observed = client.observe(sid, {})
    assert observed["signature"]["class"] == expected_class
    client.destroy(sid)


def test_helm_renderer_scenario(client: SandboxClient):
    workspace = SCENARIOS / "helm_probe_mismatch"
    created = _create(client, "helm")
    sid = created["sandbox_id"]
    deployed = client.deploy(
        sid,
        {
            "repository_sha": "abcdef1234567",
            "workspace_path": str(workspace),
            "manifests": {
                "type": "helm",
                "chart": "deploy/chart",
                "values": ["deploy/chart/values.yaml"],
                "release_name": "payments",
            },
        },
    )
    assert deployed["status_code"] == 200, deployed
    observed = client.observe(sid, {})
    assert observed["signature"]["class"] == "probe_misconfiguration"
    client.destroy(sid)


def test_kustomize_renderer_scenario(client: SandboxClient):
    workspace = SCENARIOS / "kustomize_rename"
    created = _create(client, "kust")
    sid = created["sandbox_id"]
    deployed = client.deploy(
        sid,
        {
            "repository_sha": "abcdef1234567",
            "workspace_path": str(workspace),
            "manifests": {
                "type": "kustomize",
                "overlay": "deploy/overlays/staging",
            },
        },
    )
    assert deployed["status_code"] == 200, deployed
    observed = client.observe(sid, {})
    assert observed["signature"]["class"] == "invalid_missing_config"
    client.destroy(sid)


def test_fail_closed_unknown_command(client: SandboxClient):
    created = _create(client, "failclosed")
    sid = created["sandbox_id"]
    # Deploy something healthy-ish first so sandbox has state
    workspace = SCENARIOS / "probe_port_mismatch"
    client.deploy(
        sid,
        {
            "repository_sha": "abcdef1234567",
            "workspace_path": str(workspace),
            "manifests": {"type": "yaml", "path": "deploy/manifests_fixed"},
        },
    )
    result = client.validate(
        sid,
        {
            "plan": {
                "commands": ["rm -rf /"],  # not allowlisted
            }
        },
    )
    assert result["fail_closed"] is True
    assert result["passed"] is False
    client.destroy(sid)
