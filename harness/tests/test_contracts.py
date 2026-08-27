from __future__ import annotations

import json
from pathlib import Path

from jsonschema import Draft202012Validator

CONTRACTS = Path(__file__).resolve().parents[2] / "contracts" / "sandbox"


def _load(name: str) -> dict:
    return json.loads((CONTRACTS / name).read_text())


def test_create_request_schema_accepts_minimal():
    schema = _load("create_sandbox.request.json")
    instance = {
        "run_id": "run-1",
        "tenant_id": "t1",
        "repository": {"owner": "acme", "name": "payments"},
        "commit_sha": "abcdef1",
    }
    Draft202012Validator(schema).validate(instance)


def test_failure_signature_schema_accepts_probe_example():
    schema = _load("failure_signature.json")
    instance = {
        "class": "probe_misconfiguration",
        "key": "probe_port_mismatch:payments-api:8080!=9090",
        "normalized": {
            "reason": "ReadinessProbePortMismatch",
            "resource_kind": "Deployment",
            "resource_name": "payments-api",
        },
        "reproduced": True,
        "evidence_refs": [{"kind": "k8s_event", "id": "event-0"}],
        "observed_at": "2026-08-09T12:00:00Z",
    }
    Draft202012Validator(schema).validate(instance)


def test_error_envelope_schema():
    schema = _load("error_envelope.json")
    instance = {
        "error": {
            "code": "policy_blocked",
            "message": "privileged containers are blocked",
            "retryable": False,
        }
    }
    Draft202012Validator(schema).validate(instance)


def test_destroy_response_schema():
    schema = _load("destroy_sandbox.response.json")
    instance = {
        "sandbox_id": "sb-abc",
        "status": "already_destroyed",
        "destroyed_at": "2026-08-09T12:00:00Z",
    }
    Draft202012Validator(schema).validate(instance)


def test_validation_results_schema():
    schema = _load("validation_results.json")
    instance = {
        "sandbox_id": "sb-abc",
        "passed": False,
        "fail_closed": True,
        "full_validation": False,
        "checks": [
            {
                "name": "http",
                "kind": "health_http",
                "status": "unavailable",
                "duration_ms": 1,
            }
        ],
        "completed_at": "2026-08-09T12:00:00Z",
    }
    Draft202012Validator(schema).validate(instance)


def test_finalize_response_schema_shape():
    schema = _load("finalize_result.response.json")
    # Structural check without resolving remote $refs deeply
    assert "result_id" in schema["required"]
    assert "record" in schema["required"]
    record = _load("validated_fix_record.json")
    assert "content_hash" in record["required"]
    assert "validation" in record["required"]
