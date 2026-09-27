# Contract releases

## contracts-v1.2.0

Add `kubectl` to `create_sandbox.response.json`'s `cluster_backend` enum.
This is the documented, supported backend identity returned by KubectlCluster
for the `kind`, `kubeconfig`, and `kubectl` configuration aliases. Existing
enum values remain valid. No connector-v1 envelope changes are required.
Consumers must update their pinned snapshot before accepting this identity.
