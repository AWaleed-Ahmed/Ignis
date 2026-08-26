# Ignis Sandbox

Ignis is Raphael’s isolated sandbox executor. It runs customer-approved deployment and validation steps in a local or customer-controlled Kubernetes sandbox and returns structured results to an external orchestrator.

Ignis is intentionally **not** the Raphael reasoning agent. It does not diagnose failures, generate patches, call an LLM, open GitHub pull requests, or access production credentials. Those capabilities remain in the private `raphael-core` repository.

## What Ignis provides

The controller exposes six typed lifecycle operations:

| Operation | Purpose |
|---|---|
| `create_sandbox` | Create an isolated namespace for a run |
| `deploy_revision` | Deploy YAML, Helm, or Kustomize at a specified revision |
| `observe_failure` | Collect a deterministic failure signature |
| `run_validation` | Validate a candidate using bounded checks |
| `finalize_result` | Freeze an immutable validated result |
| `destroy_sandbox` | Remove the run namespace idempotently |

The public contract is stored under [`contracts/sandbox/`](contracts/sandbox/). Contract changes are released and versioned; consumers must pin a released version rather than rely on an uncommitted shared directory.

## Trust boundary

Ignis runs next to the customer’s sandbox infrastructure. It receives typed execution instructions and returns structured results. The reasoning service chooses the next action; Ignis only executes an allowed operation and reports the result.

The customer’s source repository is cloned locally at the requested commit when needed. Source code, production kubeconfig, GitHub credentials, model keys, and private reasoning context do not belong in this repository or in the public connector protocol.

If the private Raphael service uses a customer-provided LLM key, that key is configured only in the private service boundary agreed with the customer. It is never placed in Ignis, the connector, a public contract, or a browser bundle.

## Quick start

See [`docs/install.md`](docs/install.md) for mock-backend and local Kubernetes setup. See [`docs/security.md`](docs/security.md) for the data-flow and security boundary.

```bash
cargo check --manifest-path controller/Cargo.toml
cargo test --manifest-path controller/Cargo.toml
```

The controller defaults to `127.0.0.1:8090` and uses the mock backend unless configured otherwise.

## Repository layout

```text
controller/       Rust HTTP controller
harness/          Synthetic failure scenarios and contract tests
kind/             Optional local kind-cluster bootstrap
fixtures/         Synthetic fixtures only
tests/            Manual controller tests
contracts/sandbox Public versioned request/response schemas
docs/             Customer installation and security documentation
```

## Safety principles

Ignis uses namespace-per-run isolation, bounded operations, synthetic fixtures, and restricted pod security settings by default. It must not be granted production write access, Secret payload access, arbitrary shell execution, or arbitrary `kubectl` access.

The public repository does not provide a production remediation path. Human review remains required before any downstream system creates or merges a GitHub change.

## License

The repository license will be selected by the project owner before the first public release. Do not treat an unlicensed checkout as granting permission to redistribute or use the code.
