#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
gateway="${root}/gateway"

# Rust panic/source-location strings are retained in the optimized WASM. Cargo
# is not required to live below HOME (the official Rust container uses
# /usr/local/cargo), so map each relevant root explicitly. Besides preventing
# path disclosure, this removes path variance inside one build environment.
# Native wasm-bindgen/Binaryen output still varies by host OS and architecture;
# release artifacts must use build-gateway-worker-canonical.sh.
cargo_home="${CARGO_HOME:-${HOME}/.cargo}"
rust_sysroot="$(rustc --print sysroot)"
remap="--remap-path-prefix=${gateway}=/build/source"
remap+=" --remap-path-prefix=${HOME}=/build/home"
remap+=" --remap-path-prefix=${cargo_home}=/build/cargo"
remap+=" --remap-path-prefix=${rust_sysroot}=/build/rust"
export RUSTFLAGS="${RUSTFLAGS:+${RUSTFLAGS} }${remap}"

cd "${gateway}"
exec worker-build --release
