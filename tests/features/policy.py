"""Feature: policy / isolation hardening.

What happens:
  Before apply, manifests are scanned. Privileged containers, hostPath,
  hostNetwork, hostPID, and unlabeled Secrets are blocked (policy_blocked).
"""

from __future__ import annotations

from common import cleanup, create_sandbox, expect_ok, expect_status, require_controller, scenario


def test_policy_blocks_privileged():
    """Privileged: true must be rejected with policy_blocked (HTTP 422)."""
    client = require_controller()
    created = expect_ok(create_sandbox(client, "pol-priv"), "create")
    sid = created["sandbox_id"]
    try:
        resp = client.deploy(
            sid,
            {
                "repository_sha": "abcdef1234567",
                "workspace_path": str(scenario("policy_privileged")),
                "manifests": {"type": "yaml", "path": "deploy/manifests"},
            },
        )
        body = expect_status(resp, 422, "policy-privileged")
        assert body["error"]["code"] == "policy_blocked", body
        assert "privileged" in body["error"]["message"].lower(), body
    finally:
        cleanup(client, sid)


def test_policy_blocks_host_path():
    """Break attempt: hostPath volume should be policy_blocked."""
    client = require_controller()
    created = expect_ok(create_sandbox(client, "pol-host"), "create")
    sid = created["sandbox_id"]
    # Inline patch via deploy files in a temp copy isn't needed — write a tiny
    # workspace under /tmp by deploying with patch.files onto a known scenario root.
    # Easier: use patch.files on probe workspace to inject hostPath deploy.
    ws = str(scenario("probe_port_mismatch"))
    evil = """apiVersion: apps/v1
kind: Deployment
metadata:
  name: evil-hostpath
spec:
  replicas: 1
  selector:
    matchLabels:
      app: evil
  template:
    metadata:
      labels:
        app: evil
    spec:
      containers:
        - name: app
          image: hashicorp/http-echo:1.0
          volumeMounts:
            - name: host
              mountPath: /host
      volumes:
        - name: host
          hostPath:
            path: /etc
"""
    try:
        resp = client.deploy(
            sid,
            {
                "repository_sha": "abcdef1234567",
                "workspace_path": ws,
                "manifests": {"type": "yaml", "path": "deploy/manifests"},
                "patch": {
                    "files": [
                        {
                            "path": "deploy/manifests/broken.yaml",
                            "content": evil,
                        }
                    ]
                },
            },
        )
        body = expect_status(resp, 422, "policy-hostpath")
        assert body["error"]["code"] == "policy_blocked", body
        assert "hostpath" in body["error"]["message"].lower(), body
    finally:
        cleanup(client, sid)


def test_fidelity_lists_secret_fixture_gap():
    """When secret_fixture_set is configured, fidelity should mention substitution."""
    client = require_controller()
    created = expect_ok(create_sandbox(client, "pol-fid"), "create")
    sid = created["sandbox_id"]
    try:
        body = expect_ok(
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
        gaps = body["fidelity"]["material_gaps"]
        assert any("secret" in g.lower() for g in gaps), body["fidelity"]
        subs = body["fidelity"].get("substitutions") or []
        assert any("secret_fixture" in s.get("name", "") for s in subs), body["fidelity"]
    finally:
        cleanup(client, sid)


TESTS = [
    ("policy_privileged", test_policy_blocks_privileged, "Block privileged containers"),
    ("policy_hostpath", test_policy_blocks_host_path, "Block hostPath volumes"),
    ("fidelity_secret_gap", test_fidelity_lists_secret_fixture_gap, "Fidelity reports secret fixture"),
]
