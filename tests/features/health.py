"""Feature: health check.

What happens:
  GET /health — controller says it is alive. No sandbox is created.
"""

from __future__ import annotations

from common import expect_ok, require_controller


def test_health_ok():
    """Controller responds with status=ok and service name."""
    client = require_controller()
    body = expect_ok(client.health(), "health")
    assert body.get("status") == "ok", body
    assert body.get("service") == "raphael-sandbox-controller", body


def test_health_is_fast_and_idempotent():
    """Hitting health many times must keep working (no state leak)."""
    client = require_controller()
    for _ in range(20):
        body = expect_ok(client.health(), "health-loop")
        assert body["status"] == "ok"


TESTS = [
    ("health_ok", test_health_ok, "Basic /health happy path"),
    ("health_idempotent", test_health_is_fast_and_idempotent, "Stress /health x20"),
]
