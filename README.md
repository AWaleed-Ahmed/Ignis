<div align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo-white.png" />
    <source media="(prefers-color-scheme: light)" srcset="assets/logo-dark.png" />
    <img src="assets/logo-dark.png" alt="Ignis flame and arch emblem" width="160" />
  </picture>
  <h1>Ignis</h1>
  <p>Reproduce deployment failures and validate repairs in a controlled sandbox.</p>
  <p>
    <a href="https://github.com/AWaleed-Ahmed/Ignis/actions/workflows/ci.yml"><img src="https://github.com/AWaleed-Ahmed/Ignis/actions/workflows/ci.yml/badge.svg" alt="CI" /></a>
    <img src="https://img.shields.io/badge/Rust-2021-000000?logo=rust&logoColor=white" alt="Rust 2021 edition" />
    <a href="LICENSE"><img src="https://img.shields.io/badge/License-Apache%202.0-blue.svg" alt="Apache License 2.0" /></a>
  </p>
</div>

Ignis is the sandbox executor for [Raphael](https://github.com/AWaleed-Ahmed/Raphael).
It gives platform engineers and orchestration services a small HTTP API for
deploying a revision, observing its failure, validating a candidate change and
returning a frozen result.

The two projects have distinct jobs: Raphael owns diagnosis, patch policy and
GitHub delivery; Ignis executes the sandbox lifecycle. It supports a local mock
backend for development and a kubectl-backed Kubernetes sandbox. Production
credentials and model keys belong outside this executor.

## Features

- **Six lifecycle operations:** create, deploy, observe, validate, finalize and
  destroy, with published request/response schemas.
- **Manifest rendering:** plain YAML, Helm charts and Kustomize overlays, with
  tool-version and rendered-manifest artifacts.
- **Failure signatures:** structured observations for known deployment problems,
  including probe mismatches, missing configuration and image-pull failures.
- **Sandbox controls:** per-run namespaces, resource limits, service accounts,
  network policies and restricted pod security on the Kubernetes backend.
- **Policy and validation:** reject privileged/host-access manifests; expose
  unavailable mandatory checks as failed validation; freeze validated results.
- **Cleanup and records:** idempotent destruction, TTL reconciliation, local
  metadata and filesystem artifacts.
- **Optional outbound connector:** poll Raphael dispatch, clone a requested
  revision, execute allowlisted actions and cache action results for replay.

The mock backend simulates cluster operations. It is useful for API development
and tests, but does not establish real Kubernetes behavior or network isolation.

## Demo

> Terminal recording or GIF pending: capture the mock lifecycle below, or a
> separately verified Kubernetes failure-to-fix run. Label the backend shown.

<!-- TODO: Add a real terminal recording or GIF after capturing and reviewing it. -->

The [usage example](#usage) creates and destroys a mock sandbox without Docker,
a Kubernetes cluster or an external Raphael service.

## Tech stack

| Component | Technology |
| --- | --- |
| Controller | Rust 2021 edition, Axum 0.8, Tokio |
| HTTP connector | Reqwest with rustls |
| Data and contracts | Serde, JSON Schema, YAML |
| Kubernetes execution | kubectl; optional kind for local clusters |
| Rendering | YAML parser, Helm CLI, Kustomize CLI |
| Persistence in this checkout | JSON document records and filesystem artifacts |
| Tests | Rust unit/integration tests; optional Python HTTP harness |

## Getting started

### Prerequisites

- Stable Rust and Cargo, a native linker/toolchain, and Git.
- curl for the HTTP examples; Python 3 for extracting the returned sandbox ID.
- For the optional Kubernetes backend: kubectl and a dedicated sandbox context.
  Local kind setup also needs Docker and kind.
- Helm or Kustomize only when using those renderers.

### Install

```bash
git clone https://github.com/AWaleed-Ahmed/Ignis.git
cd Ignis
cargo build --locked --manifest-path controller/Cargo.toml
```

For an existing clone, run the build from its root. Commands below use Bash.
The executable is `controller/target/debug/raphael-sandbox-controller`.
Add `--release` to build an optimized binary under `controller/target/release/`.

### Start the controller

Run from the repository root:

```bash
export RAPHAEL_CLUSTER_BACKEND=mock
export RAPHAEL_LISTEN=127.0.0.1:8090
unset RAPHAEL_CONNECTOR_DISPATCH_URL
./controller/target/debug/raphael-sandbox-controller
```

Keep the process running while trying the HTTP examples. Stop it with Ctrl+C.
Mock mode creates local records, not Kubernetes namespaces. See
[docs/install.md](docs/install.md) for backend setup, and
[docs/security.md](docs/security.md) for the trust boundary.

The controller API has no bearer-auth middleware in this checkout. Keep it on
loopback or a reviewed private network; connector authentication protects its
outbound dispatch requests, not the controller's inbound API.

## Usage

### Create and destroy a mock sandbox

In another terminal, run from the Ignis repository root:

```bash
curl --fail http://127.0.0.1:8090/health

IGNIS_SANDBOX_ID="$(
  curl --fail --silent --show-error http://127.0.0.1:8090/v1/sandboxes \
    -H 'Content-Type: application/json' \
    --data "{\"run_id\":\"readme-$(date +%s)\",\"tenant_id\":\"local-dev\",\"repository\":{\"owner\":\"AWaleed-Ahmed\",\"name\":\"Ignis\"},\"commit_sha\":\"$(git rev-parse HEAD)\",\"timeout_minutes\":5}" \
    | python3 -c 'import json, sys; print(json.load(sys.stdin)["sandbox_id"])'
)"
printf 'Created %s\n' "$IGNIS_SANDBOX_ID"

curl --fail --show-error \
  "http://127.0.0.1:8090/v1/sandboxes/$IGNIS_SANDBOX_ID/destroy" \
  -H 'Content-Type: application/json' \
  --data '{}'
```

Creation records repository identity; this example does not clone or deploy it.
Deployment and validation use the lifecycle endpoints below.

| Operation | Method | Path |
| --- | --- | --- |
| `create_sandbox` | POST | `/v1/sandboxes` |
| `deploy_revision` | POST | `/v1/sandboxes/{sandbox_id}/deploy` |
| `observe_failure` | POST | `/v1/sandboxes/{sandbox_id}/observe` |
| `run_validation` | POST | `/v1/sandboxes/{sandbox_id}/validate` |
| `finalize_result` | POST | `/v1/sandboxes/{sandbox_id}/finalize` |
| Read finalized result | GET | `/v1/sandboxes/{sandbox_id}/result` |
| `destroy_sandbox` | POST | `/v1/sandboxes/{sandbox_id}/destroy` |

Request shapes live in [contracts/sandbox/](contracts/sandbox/). A typical repair
reproduces the original failure before deploying and validating a candidate.
Finalization records the result; GitHub publication belongs to Raphael.

<details>
<summary>Tests and a longer mock walkthrough</summary>

From the repository root:

```bash
cargo fmt --manifest-path controller/Cargo.toml --all -- --check
cargo check --locked --manifest-path controller/Cargo.toml
cargo test --locked --manifest-path controller/Cargo.toml
```

With a mock controller running, the Python manual runner can exercise create,
deploy, observe, validate and finalize. Its client uses HTTPX:

```bash
python3 -m venv .venv
source .venv/bin/activate
python -m pip install httpx
python tests/test.py --list
RAPHAEL_SANDBOX_URL=http://127.0.0.1:8090 \
  python tests/test.py pipeline --failfast
```

These paths are relative to the Ignis root. Some older manual-test documentation
still uses the former `sandbox/` prefix. The optional contract/e2e harness has
its own [Python manifest](harness/pyproject.toml) and tests in `harness/tests/`.

</details>

## Project structure

```text
Ignis/
├── controller/
│   └── src/
│       ├── main.rs    # Controller startup and optional connector
│       ├── api/       # HTTP routes and error responses
│       ├── domain/    # Lifecycle, request types and sandbox rules
│       ├── k8s/       # Mock and kubectl backends
│       ├── render/    # YAML, Helm and Kustomize rendering
│       ├── observe/   # Failure-signature extraction
│       ├── validate/  # Static, rollout and HTTP checks
│       └── cleanup/   # TTL reconciliation
├── contracts/        # Sandbox and connector JSON schemas
├── harness/          # Synthetic scenarios and Python contract/e2e tests
├── tests/            # Manual HTTP feature runner
├── fixtures/         # Approved synthetic Secret fixtures
├── kind/             # Local cluster bootstrap and baseline manifests
└── docs/             # Installation and security guides
```

## Configuration

Ignis reads exported environment variables. The controller does not load an
`.env` file automatically.

| Variable | Default / purpose |
| --- | --- |
| `RAPHAEL_CLUSTER_BACKEND` | `mock`; also accepts `kind`, `kubectl`, `kubeconfig` |
| `RAPHAEL_LISTEN` | `127.0.0.1:8090` |
| `KUBECONFIG`, `RAPHAEL_KUBE_CONTEXT` | Dedicated sandbox kubeconfig/context; `kind` selects `kind-raphael-sandbox` when context is unset |
| `RAPHAEL_DATA_DIR` | `.raphael-data` metadata root |
| `RAPHAEL_SQLITE_PATH` | Legacy store-path setting; this checkout writes JSON documents, despite the name |
| `RAPHAEL_ARTIFACT_DIR` | `.raphael-artifacts` |
| `RAPHAEL_ARTIFACT_RETENTION_HOURS` | `48` |
| `RAPHAEL_PSA_ENFORCE` | `restricted` pod-security enforcement |
| `RAPHAEL_INJECT_RESTRICTED_SC` | `1`, adds restricted pod security context |
| `RAPHAEL_FIXTURES_DIR` | `fixtures/secret_fixtures`; synthetic fixtures only |
| `RAPHAEL_CONNECTOR_DISPATCH_URL` | Unset disables the connector; otherwise the dispatch HTTP(S) base URL |
| `RAPHAEL_CONNECTOR_TOKEN` | Required bearer credential when the connector is enabled |
| `RAPHAEL_CONNECTOR_CONTROLLER_URL` | `http://127.0.0.1:8090` |
| `RAPHAEL_CONNECTOR_TENANT_ID` | `connector`; must match the dispatch token's tenant |
| `RAPHAEL_CONNECTOR_POLL_INTERVAL_MS` | `1000` |
| `RAPHAEL_CONNECTOR_HTTP_TIMEOUT_SECONDS` | `30`, positive integer |

The connector runs inside the controller process. It polls
`/v1/tenants/{tenant_id}/jobs/next` and posts results to `/v1/results` on the
dispatch service. Pair it with Raphael using the
[pilot installation guide](https://github.com/AWaleed-Ahmed/Raphael/blob/main/docs/pilot-install.md)
and select compatible revisions before starting both services.

Git cloning uses the executor environment's configured Git access. Never place
credentials in repository URLs. Keep provider keys, Supabase keys and production
kubeconfig out of Ignis.

## Roadmap

Two limits are explicit in this checkout's code: patch application expects
replacement `files[]` rather than applying `unified_diff`, and the legacy store
module identifies a future database migration. These are implementation limits,
not a dated release commitment. See [open issues](https://github.com/AWaleed-Ahmed/Ignis/issues)
for current work.

## Contributing

Keep changes focused and run the Rust checks above. Update the relevant schemas
and tests when lifecycle behavior changes. Contract consumers should pin a
reviewed release; coordinate changes with Raphael rather than editing its
vendored snapshot independently. Read the [security boundary](docs/security.md)
before changing connector, fixture or cluster access.

## License

[Apache License 2.0](LICENSE).

## Author

[AWaleed-Ahmed](https://github.com/AWaleed-Ahmed)

## Contributors

| Contributor | Role |
| --- | --- |
| [AmazingDude](https://github.com/AmazingDude) | Core contributor and maintainer |
