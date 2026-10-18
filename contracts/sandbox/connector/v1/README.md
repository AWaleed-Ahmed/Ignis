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
| `lease-renewal-request.schema.json` | connector → dispatch | Monotonic renewal request for the active action claim |
| `lease-claim.schema.json` | dispatch → connector | Claim and absolute job/action deadlines returned by polling or renewal |

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


## HTTP transport and execution deadlines

`http.openapi.yaml` defines polling, result transition responses, and authenticated
lease renewal. Claims are credentials; never include them in public evidence or
application logs. An identical result already being processed returns HTTP 409;
the connector retries its cached result while keeping the heartbeat running.
The endpoint additions do not change the seven existing
connector envelope/payload schemas.

The connector sends the optional RFC3339 `x-raphael-action-deadline-at` header
to controller execution endpoints (create, deploy, observe, validate, finalize).
A compatible controller checks it before execution, bounds the server future,
and rejects an expired operation with HTTP 504. Validation carries one remaining
budget through lock acquisition, observation, tool collection, static commands,
health checks, and its final registry update. An expired execution does not save
a success receipt. Malformed deadline headers return HTTP 400.

The connector reserves 72 seconds from the fixed job/action deadline: 70 seconds
for the bounded destroy RPC and two seconds for transport/report work. It refuses
new execution when that reserve cannot fit. Cleanup is journaled, serialized
behind the controller's sandbox operation lock, and retried from the journal if
it fails. This is a finite server-deadline guarantee; dropping a client request
alone is not a server cancellation signal. Use a compatible released controller;
older controllers may ignore the new header.

### Result delivery and claim handover

An old heartbeat can arrive after dispatch commits a result but before its
HTTP response reaches the connector. Renewal still returns HTTP409. If the
request matches a persisted completed action/claim and the same authenticated
owner, the response includes `code: action_completed` plus the already-active
next claim. Neither expiry nor sequence is extended by this response. Wrong
claims/owners and expired/terminal jobs receive no handover credentials. The
connector adopts and renews that confirmed claim while finishing result
delivery, and keeps its latest sequence if the delayed result body contains
an older snapshot. Unknown409 responses still mean lease loss.

The entire result exchange, including response-body decoding, is selected
against lease loss and bounded by its execution/transport budget. Failed
preparation is journaled for cleanup; missing preparation workspaces at
restart are cleaned safely rather than preventing other jobs from recovering.
