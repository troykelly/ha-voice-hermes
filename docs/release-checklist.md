# Release checklist

An immutable firmware tag is an installation boundary. Do not create or move a
tag merely to make Dashboard Import resolve.

As of 12 July 2026, local automated gateway and firmware gates pass, but no
physical Voice PE, Home Assistant OS App installation, or live
Hermes/ElevenLabs credential run has been completed. The repository therefore
must remain untagged, the factory image must not be presented as a production
release, and the local App must remain experimental until the physical/live
gates below pass.

## Implemented automated gate

- Secret/history scan has no unresolved finding.
- Rust formatting, unit tests and strict Clippy pass with the pinned toolchain.
- The optimized Worker is regenerated and byte-compared with the App copy in a
  pinned Linux/amd64 builder. Workers-runtime Vitest and Wrangler dry run pass
  against that canonical bundle.
- `npm audit` and the selected Rust advisory scanner report no unresolved
  applicable vulnerability.
- Factory ESPHome configuration validates and compiles with the pinned ESPHome
  release; configured firmware also compiles with generated canary credentials.
- The factory image is scanned for every canary credential and contains none.
- Public package, component source and Dashboard Import paths contain no
  `!secret`, local path, mutable branch ref or generated artifact.
- Home Assistant's current App linter validates configuration/build metadata;
  CI separately validates `repository.yaml`, exact schema/translation/port
  coverage, required docs, cold-backup/architecture invariants, and parses the
  custom AppArmor profile with July 2026 tooling.
- App packaging embeds byte-identical optimized Worker artifacts and an exact
  locked `workerd`; CI builds native `amd64` and `aarch64` images from pinned
  dependencies, runs the container smoke suite on each architecture, generates
  an SPDX JSON SBOM, and fails on fixed high/critical Trivy findings or detected
  image secrets.
- App option-validation and container tests cover the standard
  `/ssl/fullchain.pem` and `/ssl/privkey.pem` path, invalid/mismatched/path-escape
  or non-publicly-trusted certificate files, TLS restart/reconnect, Access pairing, unique device
  tokens, explicit session scopes, redacted logs/process state, fail-closed
  option changes, and local Durable Object persistence. Private Hermes requires
  explicit private-network consent plus publicly trusted TLS; no custom Hermes
  CA option exists.
- PII documentation discloses clear resolved values in `/data/options.json` and
  backups, Supervisor DEBUG risk, and the `password` schema's k-anonymous
  Pwned Passwords lookup (local SHA-1; first five hex characters only).

## Manual release-artifact gates

- **Open legal-distribution gate (12 July 2026):** the current App image's
  project MIT and Apache files are not a complete third-party notice bundle.
  The locked WASM runtime includes Unicode-3.0 components and `matchit 0.7.3`
  (`MIT AND BSD-3-Clause`, including its httprouter notice). The Home Assistant
  base also contributes non-dpkg Bashio and s6-overlay/skarnet components whose
  notices are not currently copied into the project bundle. Generate and
  review the exact locked runtime/container inventory, include every required
  verbatim copyright/license notice in the repository and image, and add a CI
  regeneration/diff gate before publishing. Do not infer completeness from the
  two generic license texts or the vulnerability SBOM.
- Review the CI-generated SBOM for each architecture and archive one for each
  exact published digest.
- Re-run the image CVE/secret scan against the exact release digests and resolve
  or explicitly document every applicable finding; the moving advisory database
  can change after CI completes.
- Produce and review a dependency/license inventory and the corresponding
  license/notice bundle.
- Inspect image layers, build logs, and published manifests for secrets,
  unexpected files, mutable dependencies, architecture drift, and source-path
  PII. Existing smoke/privacy checks help, but do not replace this release
  review.

## Physical gate

Run the complete matrix in [`testing.md`](testing.md) on at least two erased
Voice PEs. It includes direct and Bluetooth-proxy Improv, serial recovery,
adoption, hardened OTA, interrupted OTA/safe mode, weak Wi-Fi, provider faults,
barge-in/AEC with Home Assistant media, wake clipping, queue pressure, heap and
stack watermarks, playback drain and a 24-hour soak.

Record device revision, ESPHome/ESP-IDF versions, gateway commit, Hermes commit,
provider models/region and measured latency percentiles. A compile log is not a
substitute for this evidence.

Install the same-repository App on supported real `amd64` and `aarch64` Home
Assistant OS targets. Validate public-CA SAN/chain WSS from a Voice PE,
certificate renewal and reconnect, private HTTPS Hermes with explicit opt-in,
live end-to-end streaming, independent Home Assistant music/announcement media,
protected/AppArmor operation, encrypted cold backup/restore, and safe clone
credential rotation. Record that Cloudflare-to-local cutover begins a new
short-term conversation. App unit/container tests are not a substitute for this
evidence.

## Publish

1. Update firmware, gateway, Home Assistant App and protocol-visible versions
   together; update the App changelog and exact `workerd` pin. The App version is
   the `version` field in `ha_voice_hermes_gateway/config.yaml`.
2. Set the public firmware package's component ref and Dashboard Import ref to
   its exact proposed semantic version tag.
3. Commit the release candidate, merge that exact reviewed commit to `main`, and
   rerun every implemented automated, manual-artifact, and physical gate.
4. Create the signed immutable firmware/source tag required by the package, then
   run `scripts/verify-release-ref.sh <tag>` with ESPHome against an anonymous
   clone.
5. Create and push a signed App tag named
   `app-v<ha_voice_hermes_gateway/config.yaml version>` from the same `main`
   commit, for example `app-v0.1.0`. It must be an annotated tag whose signature
   GitHub verifies. Protect `app-v*` with a repository tag ruleset. The publish
   workflow rejects a mismatched version, a commit outside `main`, or a version
   tag already present in GHCR; never delete or move a release tag.
6. Let the App workflow build, keyless-sign, and publish the architecture images
   under commit-SHA staging tags, create and sign the staging multi-architecture
   digest only after both architectures pass, then promote that exact digest to
   the versioned/`latest` tags and verify digest equality. A first
   publish may create a private package; if so, explicitly change the package
   visibility to public and rerun the failed **Verify anonymous installability**
   job. If `latest` promotion fails after the signed immutable version is
   created, repair only `latest`; never delete/reuse the version.
7. Without registry credentials, inspect/pull the exact versioned GHCR manifest.
   Then add the repository to a clean Home Assistant App store and complete an
   anonymous install/start check on each supported architecture.
8. Publish the factory/WebSerial and multi-architecture App artifacts, checksums,
   signatures, reviewed SBOM, license/notice bundle, source link, hardware test
   record, and App install/backup/restore record together.
9. Re-run clean Dashboard Import and first/hardened OTA from the published URL.

If a release must be withdrawn, publish a new version and mark the old release
unsupported. Never move an existing device-installation tag.
