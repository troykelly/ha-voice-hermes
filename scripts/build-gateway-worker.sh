#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
gateway="${root}/gateway"

# Rust panic/source-location strings are retained in the optimized WASM. Map
# machine-specific checkout, Cargo, and toolchain paths to stable, anonymous
# prefixes before producing any deployable artifact.
remap="--remap-path-prefix=${gateway}=/build/source --remap-path-prefix=${HOME}=/build/tooling"
export RUSTFLAGS="${RUSTFLAGS:+${RUSTFLAGS} }${remap}"

cd "${gateway}"
exec worker-build --release
