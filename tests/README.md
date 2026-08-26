# Raphael sandbox manual feature tests

These tests are for **you** to run while the controller is up. They exercise each feature, stress it, and try to break it.

**Full install / kind / env / API guide:** [`../README.md`](../README.md) (sandbox detailed README).

## 1. Start the controller

```bash
cd sandbox/controller
RAPHAEL_CLUSTER_BACKEND=mock RAPHAEL_LISTEN=127.0.0.1:8090 cargo run
```

Default listen port is **8090** (not 8080). Port 8080 is kubectl’s fallback when kubeconfig is missing — if the controller binds there, `create` looks like `cluster_unavailable (post namespaces)`.

## 2. Use the tests venv (already created; never commit it)

```bash
# recreate if needed
cd sandbox/tests
python3 -m venv .venv
.venv/bin/pip install httpx
```

`.venv/` is listed in the repo `.gitignore` so Git will not track it.

## 3. Run tests

```bash
# all features (default URL http://127.0.0.1:8090)
sandbox/tests/.venv/bin/python sandbox/tests/test.py

# list everything
sandbox/tests/.venv/bin/python sandbox/tests/test.py --list

# one feature
sandbox/tests/.venv/bin/python sandbox/tests/test.py health
sandbox/tests/.venv/bin/python sandbox/tests/test.py create
sandbox/tests/.venv/bin/python sandbox/tests/test.py observe

# several
sandbox/tests/.venv/bin/python sandbox/tests/test.py deploy observe validate finalize

# stop on first failure
sandbox/tests/.venv/bin/python sandbox/tests/test.py --failfast

# filter by case name substring
sandbox/tests/.venv/bin/python sandbox/tests/test.py observe -k probe
```

Optional: `export RAPHAEL_SANDBOX_URL=http://127.0.0.1:8091` if your controller listens elsewhere.

## Kind (real cluster)

```bash
export PATH="$HOME/.local/bin:$PATH"
./sandbox/kind/bootstrap.sh

# terminal 1 — controller (defaults to kind-raphael-sandbox context)
RAPHAEL_CLUSTER_BACKEND=kind RAPHAEL_LISTEN=127.0.0.1:8090 \
  cargo run --manifest-path sandbox/controller/Cargo.toml

# terminal 2
RAPHAEL_SANDBOX_URL=http://127.0.0.1:8090 \
  sandbox/tests/.venv/bin/python sandbox/tests/test.py
```

## Feature map

| Name | What it tests |
|---|---|
| `health` | `/health` alive + spam |
| `create` | create happy path + bad inputs + duplicate run |
| `destroy` | idempotent destroy, unknown id, ops-after-destroy, x20 stress |
| `deploy` | YAML deploy, missing paths/types, redeploy |
| `observe` | all signature classes + empty observe + match flag |
| `validate` | pass after fix, fail-closed commands, still-broken |
| `finalize` | freeze result, idempotent, early fail, clear on redeploy |
| `helm` | Helm renderer reproduce + missing chart |
| `kustomize` | Kustomize overlay reproduce + missing overlay |
| `policy` | privileged/hostPath blocked, fidelity secret gaps |
| `p0` | clone-at-SHA, fixtures, artifacts |
| `p1` | early liveness, Helm schema fail, Kustomize broken ref, fidelity claim, HTTP svc/ |
| `p2` | durable store, admin force-cleanup, disk artifacts, parallel create/destroy |
| `pipeline` | full e2e, serial x5, parallel x4, re-validate clears finalize |

Each feature file under `features/` starts with a short “What happens” comment.

## When a test fails

1. Read the printed traceback.
2. Note which feature/case failed (`feature/case_name`).
3. Fix the controller (or the test if the expectation is wrong).
4. Re-run just that feature: `python3 sandbox/tests/test.py <feature>`.
