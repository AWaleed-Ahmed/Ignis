"""Feature: Helm renderer adapter.

What happens:
  deploy_revision with manifests.type=helm runs helm template (or fallback
  concat of chart templates) then applies like YAML.
"""

from __future__ import annotations

from common import cleanup, create_sandbox, expect_ok, require_controller, scenario


def test_helm_probe_mismatch_reproduces():
    """Helm chart with wrong probe port → probe_misconfiguration."""
    client = require_controller()
    created = expect_ok(create_sandbox(client, "helm-probe"), "create")
    sid = created["sandbox_id"]
    try:
        body = expect_ok(
            client.deploy(
                sid,
                {
                    "repository_sha": "abcdef1234567",
                    "workspace_path": str(scenario("helm_probe_mismatch")),
                    "manifests": {
                        "type": "helm",
                        "chart": "deploy/chart",
                        "values": ["deploy/chart/values.yaml"],
                        "release_name": "payments",
                    },
                },
            ),
            "deploy-helm",
        )
        assert body["status"] == "deployed", body
        assert "helm" in (body.get("message") or ""), body
        sig = expect_ok(client.observe(sid), "observe")["signature"]
        assert sig["class"] == "probe_misconfiguration", sig
    finally:
        cleanup(client, sid)


def test_helm_missing_chart_fails():
    """Break attempt: chart path missing → render_failed / invalid_request."""
    client = require_controller()
    created = expect_ok(create_sandbox(client, "helm-miss"), "create")
    sid = created["sandbox_id"]
    try:
        resp = client.deploy(
            sid,
            {
                "repository_sha": "abcdef1234567",
                "workspace_path": str(scenario("helm_probe_mismatch")),
                "manifests": {"type": "helm", "chart": "deploy/no-such-chart"},
            },
        )
        assert resp["status_code"] >= 400, resp
        assert resp["body"]["error"]["code"] in {"render_failed", "invalid_request"}, resp
    finally:
        cleanup(client, sid)


TESTS = [
    ("helm_reproduce", test_helm_probe_mismatch_reproduces, "Helm broken probe reproduces"),
    ("helm_missing_chart", test_helm_missing_chart_fails, "Missing helm chart fails"),
]
