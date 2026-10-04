from __future__ import annotations

import json
import pytest
from pathlib import Path

from jsonschema import Draft202012Validator

CONTRACTS = Path(__file__).resolve().parents[2] / "contracts" / "sandbox"


def _load(name: str) -> dict:
    return json.loads((CONTRACTS / name).read_text())


@pytest.mark.parametrize("backend", ["kind", "kubeconfig", "mock", "kubectl"])
def test_create_response_accepts_supported_backends(backend):
    instance = {
        "sandbox_id": "sb-test", "namespace": "sandbox-test", "status": "ready",
        "created_at": "2026-09-27T00:00:00Z", "expires_at": "2026-09-27T01:00:00Z",
        "cluster_backend": backend,
    }
    validator = Draft202012Validator(_load("create_sandbox.response.json"))
    validator.validate(instance)
    assert not validator.is_valid({**instance, "cluster_backend": "unsupported-backend"})


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


@pytest.mark.parametrize("gaps", [
    ["image digests not resolved; tags only"],
    ["image digests not resolved; tags only: busybox:1.37.0"],
    ["image digests not resolved; tags only: app:v1", "image digests not resolved; tags only: sidecar:v2"],
])
def test_fidelity_schema_accepts_global_and_per_image_gap_messages(gaps):
    instance = {
        "score": 0.8,
        "checklist": {
            "same_commit": True, "same_render_path": True,
            "same_image_digest_or_tag": True, "equivalent_k8s_semantics": True,
            "equivalent_non_secret_config": True, "dependencies_available": True,
        },
        "material_gaps": gaps,
    }
    Draft202012Validator(_load("fidelity_report.json")).validate(instance)


def _secret_reference(status="covered"):
    return {"workload_kind":"Deployment", "workload_name":"app", "namespace":"sandbox", "source":"secretKeyRef", "secret_name":"db", "key":"URL", "optional":False, "status":status}


@pytest.mark.parametrize("status", ["covered", "missing_object", "missing_key", "unknown"])
def test_secret_coverage_contract_statuses_and_identity(status):
    schema = _load("fidelity_report.json")["properties"]["secret_coverage"]
    validator = Draft202012Validator(schema)
    report = {"format_version":1, "complete":status != "unknown", "truncated":False, "references":[_secret_reference(status)]}
    validator.validate(report)
    assert not validator.is_valid({**report, "format_version":2})
    assert not validator.is_valid({**report, "references":[{**_secret_reference(status), "secret_value":"forbidden"}]})


def test_complete_coverage_cannot_be_truncated_unknown_or_missing_reference_identity():
    validator = Draft202012Validator(_load("fidelity_report.json")["properties"]["secret_coverage"])
    report = {"format_version":1, "complete":True, "truncated":False, "references":[_secret_reference("missing_key")]}
    assert not validator.is_valid({**report, "truncated":True})
    assert not validator.is_valid({**report, "references":[_secret_reference("unknown")]})
    assert not validator.is_valid({**report, "references":[{"status":"missing_key"}]})
