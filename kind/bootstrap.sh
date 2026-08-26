#!/usr/bin/env bash
# Bootstrap a shared local kind cluster for Raphael sandbox development.
# Idempotent: safe to re-run. Does NOT create a cluster per sandbox run.
set -euo pipefail

CLUSTER_NAME="${RAPHAEL_KIND_CLUSTER:-raphael-sandbox}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export PATH="${HOME}/.local/bin:${PATH}"

if ! command -v docker >/dev/null 2>&1; then
  echo "docker not found. Run: sudo bash sandbox/kind/install-docker.sh" >&2
  exit 1
fi
if ! docker info >/dev/null 2>&1; then
  echo "docker is installed but not usable." >&2
  echo "Try:" >&2
  echo "  sudo systemctl start docker" >&2
  echo "  sudo usermod -aG docker \"\$USER\"" >&2
  echo "  newgrp docker" >&2
  echo "  docker info" >&2
  exit 1
fi
if ! command -v kind >/dev/null 2>&1; then
  echo "kind not found on PATH. Expected in ~/.local/bin or /usr/local/bin" >&2
  exit 1
fi
if ! command -v kubectl >/dev/null 2>&1; then
  echo "kubectl not found on PATH." >&2
  exit 1
fi

if kind get clusters 2>/dev/null | grep -qx "${CLUSTER_NAME}"; then
  echo "kind cluster '${CLUSTER_NAME}' already exists"
else
  echo "creating kind cluster '${CLUSTER_NAME}'"
  kind create cluster --name "${CLUSTER_NAME}" --config "${SCRIPT_DIR}/cluster.yaml"
fi

kubectl cluster-info --context "kind-${CLUSTER_NAME}"
kubectl apply -f "${SCRIPT_DIR}/namespace-baseline.yaml" || true

echo
echo "OK. Use this controller command (note port 8090 — not 8080):"
echo "  export PATH=\"\$HOME/.local/bin:\$PATH\""
echo "  export KUBECONFIG=\"\${KUBECONFIG:-\$HOME/.kube/config}\""
echo "  export RAPHAEL_KUBE_CONTEXT=kind-${CLUSTER_NAME}"
echo "  cd sandbox/controller"
echo "  RAPHAEL_CLUSTER_BACKEND=kind RAPHAEL_LISTEN=127.0.0.1:8090 cargo run"
echo
echo "Then run tests with:"
echo "  RAPHAEL_SANDBOX_URL=http://127.0.0.1:8090 sandbox/tests/.venv/bin/python sandbox/tests/test.py"
