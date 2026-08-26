"""Feature: P2 — persistence, admin cleanup, artifacts, stress.

What happens:
  Durable store under RAPHAEL_DATA_DIR / RAPHAEL_SQLITE_PATH,
  artifact files under RAPHAEL_ARTIFACT_DIR,
  POST /v1/admin/force-cleanup for operator teardown,
  parallel create/destroy stress.
"""

from __future__ import annotations

import concurrent.futures
import os
from pathlib import Path

from common import (
    REPO_ROOT,
    cleanup,
    create_sandbox,
    expect_ok,
    require_controller,
    scenario,
)


def test_artifacts_written_to_disk():
    """Deploy writes artifact files under RAPHAEL_ARTIFACT_DIR (or default)."""
    client = require_controller()
    created = expect_ok(create_sandbox(client, "p2-art"), "create")
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
        assert body["rendered_artifact_ids"], body
        root = Path(os.environ.get("RAPHAEL_ARTIFACT_DIR", ".raphael-artifacts"))
        # Controller may run with cwd=sandbox/controller
        candidates = [
            root / sid,
            REPO_ROOT / root / sid,
            REPO_ROOT / "sandbox" / "controller" / root / sid,
            Path.cwd() / root / sid,
        ]
        assert any(p.exists() and any(p.iterdir()) for p in candidates if True), (
            f"expected artifact dir for {sid} under one of {candidates}"
        )
    finally:
        cleanup(client, sid)


def test_force_cleanup_by_sandbox_id():
    """Admin force-cleanup destroys a ready sandbox."""
    client = require_controller()
    created = expect_ok(create_sandbox(client, "p2-fc"), "create")
    sid = created["sandbox_id"]
    body = expect_ok(
        client.force_cleanup(
            {
                "sandbox_id": sid,
                "reconcile_leaks": False,
                "reason": "p2-test",
            }
        ),
        "force-cleanup",
    )
    assert sid in body.get("destroyed_sandboxes", []), body
    # Second destroy is idempotent
    again = expect_ok(client.destroy(sid), "destroy-again")
    assert again["status"] in {"already_destroyed", "destroyed"}, again


def test_persist_dir_created_on_lifecycle():
    """After create+finalize path, durable store directory should exist when configured."""
    client = require_controller()
    created = expect_ok(create_sandbox(client, "p2-persist"), "create")
    sid = created["sandbox_id"]
    try:
        ws = str(scenario("probe_port_mismatch"))
        expect_ok(
            client.deploy(
                sid,
                {
                    "repository_sha": "abcdef1234567",
                    "workspace_path": ws,
                    "manifests": {"type": "yaml", "path": "deploy/manifests_fixed"},
                },
            ),
            "deploy",
        )
        expect_ok(client.observe(sid), "observe")
        expect_ok(
            client.validate(sid, {"plan": {"commands": ["true"]}}),
            "validate",
        )
        fin = expect_ok(client.finalize(sid, {"notes": "p2"}), "finalize")
        assert fin.get("result_id"), fin
        data_candidates = [
            Path(".raphael-data"),
            REPO_ROOT / ".raphael-data",
            REPO_ROOT / "sandbox" / "controller" / ".raphael-data",
            Path(os.environ["RAPHAEL_DATA_DIR"]) if os.environ.get("RAPHAEL_DATA_DIR") else None,
            Path(os.environ["RAPHAEL_SQLITE_PATH"]).parent
            if os.environ.get("RAPHAEL_SQLITE_PATH")
            else None,
        ]
        found = False
        for d in data_candidates:
            if d and d.exists():
                found = True
                break
        assert found, f"durable data dir not found among {data_candidates}"
    finally:
        cleanup(client, sid)


def test_parallel_create_destroy_stress():
    """Stress: 8 parallel create→destroy cycles (mock or kind)."""
    client = require_controller()

    def one(i: int) -> str:
        local = require_controller()
        created = expect_ok(create_sandbox(local, f"p2s{i}", timeout_minutes=5), f"create-{i}")
        sid = created["sandbox_id"]
        expect_ok(local.destroy(sid), f"destroy-{i}")
        expect_ok(local.destroy(sid), f"destroy-again-{i}")
        return sid

    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
        ids = list(pool.map(one, range(8)))
    assert len(ids) == 8
    assert len(set(ids)) == 8


TESTS = [
    ("artifacts_disk", test_artifacts_written_to_disk, "Artifacts written to disk"),
    ("force_cleanup", test_force_cleanup_by_sandbox_id, "Admin force-cleanup by id"),
    ("persist_dir", test_persist_dir_created_on_lifecycle, "Durable store dir created"),
    ("parallel_cd_8", test_parallel_create_destroy_stress, "Parallel create/destroy x8"),
]
