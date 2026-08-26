# Sandbox API contracts

Frozen JSON Schema documents for the Raphael sandbox controller.

Change these **before** changing Rust request/response types or Python fixtures.

## Verbs

| Verb | Request | Response |
|---|---|---|
| create_sandbox | `create_sandbox.request.json` | `create_sandbox.response.json` |
| deploy_revision | `deploy_revision.request.json` | `deploy_revision.response.json` |
| observe_failure | `observe_failure.request.json` | `observe_failure.response.json` |
| run_validation | `run_validation.request.json` | `run_validation.response.json` / `validation_results.json` |
| finalize_result | `finalize_result.request.json` | `finalize_result.response.json` / `validated_fix_record.json` |
| destroy_sandbox | `destroy_sandbox.request.json` | `destroy_sandbox.response.json` |

Shared types: `failure_signature.json`, `fidelity_report.json`, `error_envelope.json`, `validated_fix_record.json`.

Read frozen result: `GET /v1/sandboxes/{sandbox_id}/result` → `validated_fix_record.json`.
