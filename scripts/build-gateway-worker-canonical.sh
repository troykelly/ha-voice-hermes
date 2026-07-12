#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
temporary="$(mktemp -d)"

cleanup() {
  rm -rf "${temporary}"
}
trap cleanup EXIT

docker build \
  --platform linux/amd64 \
  --file "${root}/scripts/Dockerfile.gateway-worker" \
  --target artifact \
  --output "type=local,dest=${temporary}" \
  "${root}"

install -d -m 0755 "${root}/gateway/build"
install -m 0644 "${temporary}/index.js" "${root}/gateway/build/index.js"
install -m 0644 "${temporary}/index_bg.wasm" "${root}/gateway/build/index_bg.wasm"
