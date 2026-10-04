# Contract releases

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
