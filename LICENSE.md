# Licensing

This repository follows ESPHome's split licensing model:

- `firmware/**/*.cpp` and `firmware/**/*.h`: **GPL-3.0-only**.
- firmware Python/YAML, documentation, and repository metadata: **MIT**.
- `gateway/`: **MIT**, as declared in `gateway/Cargo.toml`.
- `ha_voice_hermes_gateway/` source and packaging authored by this project:
  **MIT**, except for bundled third-party artifacts and notices identified in
  that directory.

Project-authored code licenses and notice material for artifacts currently
vendored in this repository are provided here:

- [`LICENSES/GPL-3.0-only.txt`](LICENSES/GPL-3.0-only.txt)
- [`LICENSES/MIT.txt`](LICENSES/MIT.txt)
- [`LICENSES/Apache-2.0.txt`](LICENSES/Apache-2.0.txt), for the vendored
  microWakeWord model artifacts described in
  [`firmware/models/NOTICE.md`](firmware/models/NOTICE.md) and the patched
  Espressif WebSocket component with its own bundled notice/license. The same
  text covers the Apache-2.0 `worker-build` shim incorporated into the
  generated gateway JavaScript; the canonical builder fixes worker-build
  `0.8.4` and byte-verifies its native transformation tools.
- [`ha_voice_hermes_gateway/licenses/wasm/THIRD_PARTY_NOTICES.md`](ha_voice_hermes_gateway/licenses/wasm/THIRD_PARTY_NOTICES.md),
  the generated exact locked inventory and verbatim notice bundle for every
  dependency in the optimized gateway's `wasm32-unknown-unknown` Cargo normal
  dependency closure, plus the pinned Rust standard-library/runtime provenance
  and notices for code statically linked into that WASM.
- The App image's [container notice index](ha_voice_hermes_gateway/licenses/container/README.md)
  and locked [component manifest](ha_voice_hermes_gateway/licenses/container/COMPONENTS.json)
  cover non-dpkg software inherited from the exact Home Assistant base or
  copied into the image.
- The generated [workerd Rust notice/source bundle](ha_voice_hermes_gateway/licenses/container/workerd-rust/THIRD_PARTY_NOTICES.md)
  covers every registry, git, and path package in workerd's exact Rust lock,
  including MPL-2.0 source directions and an unresolved count of zero.
- The native workerd [gate status](ha_voice_hermes_gateway/licenses/container/workerd-native/STATUS.md)
  and [locked inventory](ha_voice_hermes_gateway/licenses/container/workerd-native/LOCK.json)
  cover the exact Linux binaries' reviewed C/C++ and static runtime closure;
  they separately disclose two upstream provenance limitations that are not
  known notice or corresponding-source blockers.
- The [Debian binary-to-source lock](ha_voice_hermes_gateway/licenses/container/debian-sources.lock.json)
  maps every installed dpkg binary to its exact source package. A release is
  source-first: before its image becomes public, the immutable GitHub Release
  includes the complete authenticated Debian source archive generated from
  that lock, the embedded licence tree, and per-architecture copyright files.

Unless a more specific notice is present, MIT-licensed repository material is
copyright 2026 the ha-voice-hermes contributors.

External components, XMOS firmware, ESPHome itself, Hermes, and hosted services
retain their own licenses and terms. In particular, distributed ESPHome
firmware must satisfy the GPL source-availability obligations for the linked
C++ runtime and this component.
