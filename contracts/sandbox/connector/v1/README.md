# Connector protocol v1

Public wire schemas for the persistent outbound connection between a customer-hosted Ignis connector and the private `raphael-core` dispatch service. This directory contains schemas only; it contains no connector or dispatch logic.

## Files

| File | Direction | Purpose |
|---|---|---|
| `envelope.schema.json` | both | Outer frame every message uses; `kind` selects the payload schema |
| `job.schema.json` | dispatch → connector | New work offer: repository, commit SHA, narrowed location; no source code |
| `action.schema.json` | dispatch → connector | One instructed step; `verb` is restricted to the six sandbox operations |
| `result.schema.json` | connector → dispatch | Structured outcome of one action |
| `terminal.schema.json` | dispatch → connector | Ends a job and instructs the connector to discard local state |
| `ack.schema.json` | both | Receipt confirmation independent of action success or failure |
| `error.schema.json` | both | Protocol or connection-level failure outside the normal action/result flow |

## Design rules

`action.schema.json` does not redefine the six verb payloads. Its `verb` field is a closed allowlist, while `args` must independently validate against the corresponding request schema in [`../../`](../../):

```text
create_sandbox.request.json
deploy_revision.request.json
observe_failure.request.json
run_validation.request.json
finalize_result.request.json
destroy_sandbox.request.json
```

Likewise, a successful `result` must validate against the corresponding dotted response schema, such as `deploy_revision.response.json` or `finalize_result.response.json`. This avoids competing definitions of the sandbox API.

No reasoning appears on this wire. A `job` identifies where the customer-hosted sandbox should look; an `action` identifies one typed operation to execute. Prompts, hypotheses, ranking, patch logic, and the decision about the next action remain private to `raphael-core`.

Every action carries an `action_id`. Replaying the same identifier must return a cached result rather than execute a side effect twice. This protects against duplicate deploys or finalization after reconnect and retry.

`retain_for_debug` in `terminal.schema.json` is a short-lived, explicitly bounded pilot troubleshooting mode. The normal terminal instruction is `discard_local_copy`.

The `job_lease_expired` error code assumes that private dispatch tracks job leases. A connector implementation must handle lease expiry without continuing work on an abandoned job.

## Versioning

This is protocol `v1`. A breaking change to the envelope or payload shapes requires a new `v2/` directory rather than an in-place edit. `raphael-core` must pin a released connector-contract version.
