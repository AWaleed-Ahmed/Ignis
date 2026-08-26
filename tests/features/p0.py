"""P0 extras: secret fixtures, observe artifacts, clone-at-SHA contract.

What happens:
- create with secret_fixture_set applies synthetic secrets (policy-safe)
- observe stores event/log artifact ids
- deploy without workspace_path but with clone_url attempts git clone-at-SHA
"""

from __future__ import annotations

import subprocess
import tempfile
from pathlib import Path

from common import cleanup, create_sandbox, expect_ok, expect_status, require_controller, scenario


def test_create_applies_secret_fixture_set():
    """Create with payments-test fixture should succeed (fixtures applied)."""
    client = require_controller()
    resp = create_sandbox(client, "fix-ok")
    body = expect_ok(resp, "create")
    cleanup(client, body["sandbox_id"])


def test_observe_returns_artifact_ids():
    """After deploy+observe, artifact_ids should include event/log captures."""
    client = require_controller()
    created = expect_ok(create_sandbox(client, "art-obs"), "create")
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
        body = expect_ok(client.observe(sid), "observe")
        assert body["artifact_ids"], body
        assert body["signature"]["class"] == "probe_misconfiguration"
    finally:
        cleanup(client, sid)


def test_deploy_requires_workspace_or_clone_url():
    """Break attempt: no workspace_path and no clone_url → invalid_request."""
    client = require_controller()
    # create without clone_url
    created = expect_ok(
        client.create(
            {
                "run_id": f"manual-noclone-{__import__('uuid').uuid4().hex[:8]}",
                "tenant_id": "local-dev",
                "repository": {"owner": "raphael", "name": "demo"},
                "commit_sha": "abcdef1234567",
            }
        ),
        "create",
    )
    sid = created["sandbox_id"]
    try:
        resp = client.deploy(
            sid,
            {
                "repository_sha": "abcdef1234567",
                "manifests": {"type": "yaml", "path": "deploy/manifests"},
            },
        )
        body = expect_status(resp, 400, "deploy-no-ws")
        assert body["error"]["code"] == "invalid_request", body
    finally:
        cleanup(client, sid)


def test_clone_at_sha_local_git_repo():
    """Create a tiny local git repo, clone-at-SHA via file:// clone_url, deploy YAML from it."""
    client = require_controller()
    with tempfile.TemporaryDirectory(prefix="raphael-src-") as src:
        src_path = Path(src)
        (src_path / "deploy" / "manifests").mkdir(parents=True)
        broken = (scenario("probe_port_mismatch") / "deploy" / "manifests" / "broken.yaml").read_text()
        (src_path / "deploy" / "manifests" / "app.yaml").write_text(broken)
        subprocess.run(["git", "init"], cwd=src_path, check=True, capture_output=True)
        subprocess.run(["git", "config", "user.email", "raphael@test"], cwd=src_path, check=True)
        subprocess.run(["git", "config", "user.name", "Raphael"], cwd=src_path, check=True)
        subprocess.run(["git", "add", "."], cwd=src_path, check=True, capture_output=True)
        subprocess.run(
            ["git", "commit", "-m", "seed"],
            cwd=src_path,
            check=True,
            capture_output=True,
        )
        sha = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=src_path, text=True).strip()
        clone_url = f"file://{src_path}"

        created = expect_ok(
            client.create(
                {
                    "run_id": f"manual-clone-{__import__('uuid').uuid4().hex[:8]}",
                    "tenant_id": "local-dev",
                    "repository": {
                        "owner": "raphael",
                        "name": "demo",
                        "clone_url": clone_url,
                    },
                    "commit_sha": sha,
                    "secret_fixture_set": "payments-test",
                }
            ),
            "create",
        )
        sid = created["sandbox_id"]
        try:
            # No workspace_path — controller must clone
            body = expect_ok(
                client.deploy(
                    sid,
                    {
                        "repository_sha": sha,
                        "manifests": {"type": "yaml", "path": "deploy/manifests"},
                    },
                ),
                "deploy-cloned",
            )
            assert body["status"] == "deployed", body
            sig = expect_ok(client.observe(sid), "observe")["signature"]
            assert sig["class"] == "probe_misconfiguration", sig
        finally:
            cleanup(client, sid)


TESTS = [
    ("fixture_create", test_create_applies_secret_fixture_set, "Create applies secret fixture set"),
    ("observe_artifacts", test_observe_returns_artifact_ids, "Observe returns artifact ids"),
    ("deploy_needs_source", test_deploy_requires_workspace_or_clone_url, "Deploy needs workspace or clone_url"),
    ("clone_at_sha", test_clone_at_sha_local_git_repo, "Clone-at-SHA from local file:// repo"),
]
