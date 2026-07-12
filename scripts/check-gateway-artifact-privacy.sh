#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
artifacts=(
  "${root}/gateway/build/index.js"
  "${root}/gateway/build/index_bg.wasm"
  "${root}/ha_voice_hermes_gateway/gateway-build/index.js"
  "${root}/ha_voice_hermes_gateway/gateway-build/index_bg.wasm"
)

patterns='(/Users/[^/[:space:]]+|/home/[^/[:space:]]+|/root/[^/[:space:]]+|/usr/local/cargo/[^/[:space:]]+|[A-Za-z]:\\Users\\[^\\[:space:]]+)'
for artifact in "${artifacts[@]}"; do
  if [[ ! -f "${artifact}" ]]; then
    echo "Missing release artifact: ${artifact#"${root}/"}" >&2
    exit 1
  fi
  if LC_ALL=C strings "${artifact}" | grep -Eq "${patterns}"; then
    echo "Release artifact contains a developer home-directory path: ${artifact#"${root}/"}" >&2
    exit 1
  fi
  if LC_ALL=C grep -aFq -- "${root}" "${artifact}"; then
    echo "Release artifact contains the local workspace path: ${artifact#"${root}/"}" >&2
    exit 1
  fi
done
