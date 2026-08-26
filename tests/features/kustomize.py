"""Feature: Kustomize renderer adapter.

What happens:
  deploy_revision with manifests.type=kustomize builds an overlay
  (kubectl kustomize / fallback file concat) then applies.
"""

from __future__ import annotations

from common import cleanup, create_sandbox, expect_ok, require_controller, scenario


def test_kustomize_missing_config_reproduces():
    """Staging overlay with missing ConfigMap key → invalid_missing_config."""
    client = require_controller()
    created = expect_ok(create_sandbox(client, "kust-ok"), "create")
    sid = created["sandbox_id"]
    try:
        body = expect_ok(
            client.deploy(
                sid,
                {
                    "repository_sha": "abcdef1234567",
                    "workspace_path": str(scenario("kustomize_rename")),
                    "manifests": {
                        "type": "kustomize",
                        "overlay": "deploy/overlays/staging",
                    },
                },
            ),
            "deploy-kust",
        )
        assert body["status"] == "deployed", body
        sig = expect_ok(client.observe(sid), "observe")["signature"]
        assert sig["class"] == "invalid_missing_config", sig
    finally:
        cleanup(client, sid)


def test_kustomize_missing_overlay_fails():
    """Break attempt: bad overlay path fails."""
    client = require_controller()
    created = expect_ok(create_sandbox(client, "kust-miss"), "create")
    sid = created["sandbox_id"]
    try:
        resp = client.deploy(
            sid,
            {
                "repository_sha": "abcdef1234567",
                "workspace_path": str(scenario("kustomize_rename")),
                "manifests": {"type": "kustomize", "overlay": "deploy/overlays/nope"},
            },
        )
        assert resp["status_code"] >= 400, resp
        assert resp["body"]["error"]["code"] in {"render_failed", "invalid_request"}, resp
    finally:
        cleanup(client, sid)


TESTS = [
    ("kustomize_reproduce", test_kustomize_missing_config_reproduces, "Kustomize overlay reproduces"),
    ("kustomize_missing", test_kustomize_missing_overlay_fails, "Missing overlay fails"),
]
