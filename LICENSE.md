# Licensing

This repository follows ESPHome's split licensing model:

- `firmware/**/*.cpp` and `firmware/**/*.h`: **GPL-3.0-only**.
- firmware Python/YAML, documentation, and repository metadata: **MIT**.
- `gateway/`: **MIT**, as declared in `gateway/Cargo.toml`.
- `ha_voice_hermes_gateway/` source and packaging authored by this project:
  **MIT**, except for bundled third-party artifacts and notices identified in
  that directory.

The complete applicable license texts are included in this repository:

- [`LICENSES/GPL-3.0-only.txt`](LICENSES/GPL-3.0-only.txt)
- [`LICENSES/MIT.txt`](LICENSES/MIT.txt)
- [`LICENSES/Apache-2.0.txt`](LICENSES/Apache-2.0.txt), for the vendored
  microWakeWord model artifacts described in
  [`firmware/models/NOTICE.md`](firmware/models/NOTICE.md) and the patched
  Espressif WebSocket component with its own bundled notice/license.

Unless a more specific notice is present, MIT-licensed repository material is
copyright 2026 the ha-voice-hermes contributors.

External components, XMOS firmware, ESPHome itself, Hermes, and hosted services
retain their own licenses and terms. In particular, distributed ESPHome
firmware must satisfy the GPL source-availability obligations for the linked
C++ runtime and this component.
