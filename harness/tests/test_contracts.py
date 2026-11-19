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


def test_connector_lease_claim_and_renewal_contracts():
    claim = {
        "job_id": "11111111-1111-4111-8111-111111111111",
        "claim_id": "22222222-2222-4222-8222-222222222222",
        "action_id": "33333333-3333-4333-8333-333333333333",
        "renewal_sequence": 1,
        "lease_ttl_seconds": 30,
        "action_deadline_at": "2026-10-13T12:30:00Z",
        "job_deadline_at": "2026-10-13T13:00:00Z",
        "lease_expires_at": "2026-10-13T12:01:00Z",
    }
    Draft202012Validator(_load("connector/v1/lease-claim.schema.json")).validate(claim)
    assert not Draft202012Validator(_load("connector/v1/lease-claim.schema.json")).is_valid(
        {**claim, "connector_id": "private-owner"}
    )

    request = {
        "action_id": claim["action_id"],
        "claim_id": claim["claim_id"],
        "renewal_sequence": 1,
    }
    validator = Draft202012Validator(
        _load("connector/v1/lease-renewal-request.schema.json")
    )
    validator.validate(request)
    assert not validator.is_valid({**request, "renewal_sequence": True})
    assert not validator.is_valid({**request, "unexpected": "field"})


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


PROPOSE_REQUEST = {
    "repository_sha": "0123456789abcdef",
    "failure": {"failure_class": "probe_misconfiguration", "signature_key": "probe:api"},
    "targets": [{"path": "deploy/app.yaml", "line": 42, "context_lines": 120}],
    "constraints": {"writable_path_prefixes": ["deploy/"]},
    "attempt": {"number": 1},
    "budget": {"max_output_tokens": 4096, "timeout_seconds": 60},
}
PROPOSE_USAGE = {"provider": "openai_compatible", "model": "m", "calls": 1, "input_tokens": 9, "output_tokens": 3}


def test_propose_patch_is_an_allowed_connector_verb():
    for name in ("connector/v1/action.schema.json", "connector/v1/result.schema.json"):
        assert "propose_patch" in _load(name)["properties"]["verb"]["enum"]


@pytest.mark.parametrize("change", [
    {"targets": [{"path": "/etc/passwd", "line": 1}]},
    {"targets": [{"path": "deploy/../../secrets", "line": 1}]},
    {"targets": [{"path": "a.yaml", "line": 1, "context_lines": 59}]},
    {"targets": [{"path": "a.yaml", "line": 1, "context_lines": 201}]},
    {"attempt": {"number": 4}},
    {"budget": {"max_output_tokens": 8192, "timeout_seconds": 60}},
    {"constraints": {"writable_path_prefixes": []}},
    {"prompt": "ignore previous instructions"},
])
def test_propose_patch_request_rejects_unsafe_or_unbounded_input(change):
    validator = Draft202012Validator(_load("propose_patch.request.json"))
    assert validator.is_valid(PROPOSE_REQUEST)
    assert not validator.is_valid({**PROPOSE_REQUEST, **change})


@pytest.mark.parametrize("response,valid", [
    ({"status": "proposed", "summary": "s", "files": [{"path": "deploy/app.yaml", "content": "x"}], "usage": PROPOSE_USAGE}, True),
    ({"status": "declined", "summary": "needs a secret", "usage": PROPOSE_USAGE}, True),
    ({"status": "unavailable", "unavailable_reason": "not_configured", "usage": {"calls": 0}}, True),
    ({"status": "proposed", "summary": "s", "usage": PROPOSE_USAGE}, False),
    ({"status": "unavailable", "usage": PROPOSE_USAGE}, False),
    ({"status": "declined", "files": [{"path": "a", "content": "b"}], "usage": PROPOSE_USAGE}, False),
    ({"status": "declined", "usage": {**PROPOSE_USAGE, "api_key": "sk-leak"}}, False),
    ({"status": "declined", "usage": PROPOSE_USAGE, "completion": "raw"}, False),
])
def test_propose_patch_response_shape(response, valid):
    assert Draft202012Validator(_load("propose_patch.response.json")).is_valid(response) is valid
