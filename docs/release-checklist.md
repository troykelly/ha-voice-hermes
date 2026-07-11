# Release checklist

An immutable firmware tag is an installation boundary. Do not create or move a
tag merely to make Dashboard Import resolve.

As of 11 July 2026, local automated gateway and firmware gates pass, but no
physical Voice PE or live Hermes/ElevenLabs credential run has been completed.
The repository therefore must remain untagged and its factory image must not be
presented as a production release until the physical/live gates below pass.

## Automated gate

- Secret/history scan has no unresolved finding.
- Rust formatting, unit tests and strict Clippy pass with the pinned toolchain.
- Optimized Worker build, Workers-runtime Vitest suite and Wrangler dry run
  pass from `npm ci`.
- `npm audit` and the selected Rust advisory scanner report no unresolved
  applicable vulnerability.
- Factory ESPHome configuration validates and compiles with the pinned ESPHome
  release; configured firmware also compiles with generated canary credentials.
- The factory image is scanned for every canary credential and contains none.
- Public package, component source and Dashboard Import paths contain no
  `!secret`, local path, mutable branch ref or generated artifact.

## Physical gate

Run the complete matrix in [`testing.md`](testing.md) on at least two erased
Voice PEs. It includes direct and Bluetooth-proxy Improv, serial recovery,
adoption, hardened OTA, interrupted OTA/safe mode, weak Wi-Fi, provider faults,
barge-in/AEC with Home Assistant media, wake clipping, queue pressure, heap and
stack watermarks, playback drain and a 24-hour soak.

Record device revision, ESPHome/ESP-IDF versions, gateway commit, Hermes commit,
provider models/region and measured latency percentiles. A compile log is not a
substitute for this evidence.

## Publish

1. Update firmware, gateway and protocol-visible versions together.
2. Set the public package's component ref and Dashboard Import ref to the exact
   proposed semantic version tag.
3. Commit the release candidate and rerun every automated and physical gate.
4. Create a signed, immutable tag from that exact commit.
5. With ESPHome installed, run `scripts/verify-release-ref.sh <tag>` against an
   anonymous clone.
6. Publish the factory/WebSerial artifacts, checksums, SBOM, license/notice
   bundle, source link and hardware test record together.
7. Re-run clean Dashboard Import and first/hardened OTA from the published URL.

If a release must be withdrawn, publish a new version and mark the old release
unsupported. Never move an existing device-installation tag.
