#!/usr/bin/env bash
# Install Docker Engine on Ubuntu for Raphael kind sandboxes.
# Run with: sudo bash sandbox/kind/install-docker.sh
set -euo pipefail

if [[ "${EUID}" -ne 0 ]]; then
  echo "Re-run with sudo: sudo bash sandbox/kind/install-docker.sh" >&2
  exit 1
fi

# Prefer the invoking login user (sudo sets SUDO_USER).
TARGET_USER="${SUDO_USER:-}"
if [[ -z "${TARGET_USER}" || "${TARGET_USER}" == "root" ]]; then
  TARGET_USER="${1:-}"
fi

export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq docker.io containerd ca-certificates curl
systemctl enable --now docker || service docker start

if [[ -n "${TARGET_USER}" ]]; then
  usermod -aG docker "${TARGET_USER}"
  echo "Added '${TARGET_USER}' to the docker group."
else
  echo "WARNING: could not detect non-root user to add to docker group."
  echo "Run: sudo usermod -aG docker YOUR_USERNAME"
fi

docker version
echo
echo "Docker installed."
echo "IMPORTANT: apply the new group (pick one):"
echo "  1) log out and log back in"
echo "  2) run:  newgrp docker"
echo "Then verify without sudo:"
echo "  docker info"
echo "  export PATH=\"\$HOME/.local/bin:\$PATH\""
echo "  ./sandbox/kind/bootstrap.sh"
echo "  RAPHAEL_CLUSTER_BACKEND=kind cargo run --manifest-path sandbox/controller/Cargo.toml"
