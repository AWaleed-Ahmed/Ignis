# Contract releases

## contracts-v1.5.0

Add connector-local `propose_patch` for customer-key LLM patch proposals.

- **Schemas:** new `propose_patch.request.json` and `propose_patch.response.json`. `propose_patch` is added to the connector `action` and `result` verb allowlists.
- **Request:** typed incident context only, with no prompt field: failure, redacted evidence, up to 5 targets with 60–200 line windows, writable paths, attempt (maximum 3) and per-call budget (up to 4096 output tokens and 60 s).
- **Response:** `proposed` files reuse `deploy_revision` `patch.files`. `declined` and `unavailable` carry no files, and usage metadata is always included.
- **Secrets:** keys, prompts and completions never appear on the wire.

**Compatibility:** connectors older than this release reject the unknown verb. Consumers must treat that result as `unavailable` and fall back to deterministic templates. Older closed consumer schemas also reject the new verb, so update pinned snapshots and runtimes together. The existing six verbs are unchanged.

## contracts-v1.3.0

Add optional `secret_coverage` to fidelity reports: versioned, bounded reference
metadata with covered, missing-object, missing-key and unknown statuses.
Complete reports cannot be truncated or contain unknown references. Applied
fixture inventory is private controller state containing names and keys only.
Older closed consumer schemas may reject this new field; update snapshots and
runtime pins together. Existing fidelity fields and connector envelopes remain.

## contracts-v1.2.0

Add `kubectl` to `create_sandbox.response.json`'s `cluster_backend` enum.
This is the documented, supported backend identity returned by KubectlCluster
for the `kind`, `kubeconfig`, and `kubectl` configuration aliases. Existing
enum values remain valid. No connector-v1 envelope changes are required.
Consumers must update their pinned snapshot before accepting this identity.
