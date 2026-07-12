#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source_dir="${root}/gateway/build"
target_dir="${root}/ha_voice_hermes_gateway/gateway-build"
mode="${1:---check}"

hash_files() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum index.js index_bg.wasm
  else
    shasum -a 256 index.js index_bg.wasm
  fi
}

for artifact in index.js index_bg.wasm; do
  if [[ ! -f "${source_dir}/${artifact}" ]]; then
    echo "Missing gateway/build/${artifact}; run 'npm run build' in gateway first." >&2
    exit 1
  fi
done

case "${mode}" in
  --check)
    for artifact in index.js index_bg.wasm; do
      if ! cmp -s "${source_dir}/${artifact}" "${target_dir}/${artifact}"; then
        echo "Vendored App artifact ${artifact} is stale; run this script with --update." >&2
        exit 1
      fi
    done
    (
      cd "${target_dir}"
      hash_files | cmp -s - SHA256SUMS
    ) || {
      echo "Vendored App artifact checksums are stale." >&2
      exit 1
    }
    ;;
  --update)
    install -d -m 0755 "${target_dir}"
    install -m 0644 "${source_dir}/index.js" "${target_dir}/index.js"
    install -m 0644 "${source_dir}/index_bg.wasm" "${target_dir}/index_bg.wasm"
    (
      cd "${target_dir}"
      hash_files > SHA256SUMS
    )
    ;;
  *)
    echo "Usage: $0 [--check|--update]" >&2
    exit 2
    ;;
esac
