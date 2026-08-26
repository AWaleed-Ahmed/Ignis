# Ignis Installation

This guide explains how to run the Ignis sandbox controller locally. The mock backend is the recommended first step because it requires no Docker or Kubernetes. The kind backend is optional and is intended for local sandbox validation only.

## Prerequisites

| Tool | Required for | Notes |
|---|---|---|
| Rust and Cargo | Controller build and tests | Stable toolchain |
| Python 3.11+ | Manual test client | Use a virtual environment |
| Docker | kind backend only | The current user must be allowed to access Docker |
| kubectl, kind, Helm | kind backend only | Install using the official project instructions |

## Run with the mock backend

From the repository root:

```bash
RAPHAEL_CLUSTER_BACKEND=mock \
RAPHAEL_LISTEN=127.0.0.1:8090 \
  cargo run --manifest-path controller/Cargo.toml
```

In a second terminal, run the manual tests:

```bash
python3 -m venv tests/.venv
. tests/.venv/bin/activate
python -m pip install httpx
python tests/test.py
```

The controller exposes health at `GET http://127.0.0.1:8090/health`.

## Run with a local kind cluster

The kind backend is optional and must remain isolated from production infrastructure:

```bash
./kind/bootstrap.sh
kubectl --context kind-raphael-sandbox get ns
```

Start the controller with:

```bash
RAPHAEL_CLUSTER_BACKEND=kind \
RAPHAEL_KUBE_CONTEXT=kind-raphael-sandbox \
RAPHAEL_LISTEN=127.0.0.1:8090 \
  cargo run --manifest-path controller/Cargo.toml
```

Then run the test client against `http://127.0.0.1:8090`.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `RAPHAEL_CLUSTER_BACKEND` | `mock` | `mock`, `kind`, `kubectl`, or `kubeconfig` |
| `RAPHAEL_LISTEN` | `127.0.0.1:8090` | HTTP bind address |
| `RAPHAEL_KUBE_CONTEXT` | `kind-raphael-sandbox` for kind | Local Kubernetes context |
| `KUBECONFIG` | platform default | Local kubeconfig path |
| `RAPHAEL_SANDBOX_URL` | `http://127.0.0.1:8090` | Test client base URL |
| `RAPHAEL_DATA_DIR` | `.raphael-data` | Local metadata directory |
| `RAPHAEL_ARTIFACT_DIR` | `.raphael-artifacts` | Local artifact directory |
| `RAPHAEL_PSA_ENFORCE` | `restricted` | Pod Security Admission level |
| `RAPHAEL_INJECT_RESTRICTED_SC` | `1` | Add restricted pod security context |
| `RAPHAEL_DEFAULT_WORKSPACE` | unset | Optional local workspace fallback |
| `RAPHAEL_FIXTURES_DIR` | `fixtures/secret_fixtures` | Synthetic fixture directory |

Ignis has no GitHub token, LLM key, Supabase credential, or production kubeconfig setting. Those belong outside this public repository.

## API lifecycle

The normal lifecycle is:

```text
create → deploy → observe → deploy candidate → observe → validate → finalize → destroy
```

The typed HTTP endpoints are:

| Operation | Method | Path |
|---|---|---|
| Health | `GET` | `/health` |
| Create | `POST` | `/v1/sandboxes` |
| Deploy | `POST` | `/v1/sandboxes/{id}/deploy` |
| Observe | `POST` | `/v1/sandboxes/{id}/observe` |
| Validate | `POST` | `/v1/sandboxes/{id}/validate` |
| Finalize | `POST` | `/v1/sandboxes/{id}/finalize` |
| Result | `GET` | `/v1/sandboxes/{id}/result` |
| Destroy | `POST` | `/v1/sandboxes/{id}/destroy` |

Schemas are defined in [`../contracts/sandbox/`](../contracts/sandbox/). `finalize` freezes an immutable validated result; it does not open a pull request.

## Verification

Run the Rust checks before publishing a release:

```bash
cargo check --manifest-path controller/Cargo.toml
cargo test --manifest-path controller/Cargo.toml
```

Run the manual test client against the mock backend before trying kind. Destroy test namespaces after local kind validation and never point this installation guide at a production cluster.
