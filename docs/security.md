# Ignis Security Boundary

Ignis is an isolated executor, not a reasoning service. Its security model is based on minimizing trust: the local controller performs a small set of typed sandbox operations, while the private Raphael service decides which operation is appropriate.

## What crosses the boundary

| Data | Direction | Notes |
|---|---|---|
| Repository identifier and commit SHA | Private service to customer sandbox | The sandbox clones the requested revision locally |
| Narrowed file or line location | Private service to customer sandbox | Used to scope the local job |
| One typed action | Private service to connector/controller | Must be one of the versioned sandbox operations |
| Structured execution result | Customer sandbox to private service | Includes bounded signatures and validation output |
| Final validated diff | Customer sandbox to private service | Sent briefly for downstream review, then discarded locally after acknowledgement |

Source code is not uploaded to the private service merely to execute the sandbox. The customer-controlled environment uses its own repository access to obtain the requested commit.

## What does not belong in Ignis

Ignis must never receive or store:

- GitHub tokens, GitHub App private keys, or webhook secrets.
- LLM API keys, model prompts, private skills, or reasoning traces.
- Supabase service-role credentials or private database configuration.
- Production Kubernetes kubeconfig files.
- Production Kubernetes Secret payloads.
- Arbitrary shell commands or arbitrary `kubectl` requests.
- Private customer data unrelated to the requested sandbox run.

If a customer uses a bring-your-own-model arrangement, the model key is configured in the separately controlled Raphael core deployment or another explicitly approved private boundary. It is not placed in Ignis or its public connector.

## Kubernetes isolation

The optional kind deployment is for a local or customer-controlled sandbox only. Use namespace-per-run isolation, restricted Pod Security Admission, bounded logs and observation, and default-deny network policy where supported.

The Ignis service must not be granted production mutation permissions. In particular, it must not create, update, patch, delete, scale, exec into, or port-forward to production workloads. It must not read Kubernetes Secret payloads. Production evidence collection, when approved, belongs to a separately designed private integration with an explicit read-only permission matrix.

## Connector expectations

A future connector may maintain an outbound authenticated session to the private dispatch service. It must not open an inbound firewall port, retain a completed fix after acknowledgement, or choose its own next action. The connector protocol must use versioned envelopes, replay protection, per-customer scope, acknowledgements, bounded retries, and token rotation/revocation.

The connector may invoke only typed sandbox operations. It must reject unknown protocol versions, unknown action names, oversized payloads, invalid job ownership, and requests outside the customer’s assigned sandbox scope.

## Release checks

Before each public release, inspect the complete tree and history for credentials and private implementation details. At minimum, verify that the repository contains no `.env` files, service-role keys, GitHub tokens, internal API URLs, `contracts/agent`, private reasoning documents, or production write permissions.

A clean working tree is not sufficient. Public history must also be fresh and must not be derived from the private monorepo’s internal commit history.

## Incident response

If a credential or private payload is ever copied into the public repository or transmitted outside the approved boundary, stop the release, revoke or rotate the affected credential, preserve the audit details, and review the complete Git history and connector logs before resuming.
