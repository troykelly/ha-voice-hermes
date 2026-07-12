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

rm -rf "${root}/gateway/build"
install -d -m 0755 "${root}/gateway/build"
cp -R "${temporary}/." "${root}/gateway/build/"
