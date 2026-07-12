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
- The App tag workflow fails before any package write unless the locked
  Rust/WASM notice closure, network-refreshed release-complete native `workerd`
  inventory, exact base-container component inventory, exact-main-head tag,
  immutable-tag ruleset, and GitHub native immutable-Release setting all
  validate without drift. It writes only to new private run-specific staging
  packages until the complete `ha_voice_hermes_gateway/licenses` tree and
  corresponding source are published in the immutable Release.
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

- Review the generated Rust/WASM dependency closure, base-container component
  inventory and corresponding verbatim notice files. Their regeneration/diff
  checks are release-blocking, but machine consistency does not replace the
  maintainer's legal-distribution review.
- Review the CI-generated SBOM for each architecture and archive one for each
  exact published digest.
- Re-run the image CVE/secret scan against the exact release digests and resolve
  or explicitly document every applicable finding; the moving advisory database
  can change after CI completes.
- Produce and review a dependency/license inventory and the corresponding
  license/notice bundle. Verify the Debian binary-to-source lock against both
  exact images and review the corresponding-source archive and its signed
  checksum.
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
3. Commit the release candidate and **squash-merge** PR #1 using the repository's
   enforced squash-only merge policy, then delete the feature branch. Do not
   merge-commit or rebase-merge it: a tag executes the workflow at the tagged
   commit, and older feature-branch publication workflows must never become
   `main` ancestors. Re-run every implemented automated, manual-artifact, and
   physical gate on the resulting single `main` commit.
4. Create the signed immutable firmware/source tag required by the package, then
   run `scripts/verify-release-ref.sh <tag>` with ESPHome against an anonymous
   clone.
5. Add the Actions repository secret `RELEASE_POLICY_TOKEN` using a short-lived
   fine-grained token restricted to this repository with only
   **Administration: read**; never reuse a broad CLI, App, provider, or device
   credential. The normal job-scoped `GITHUB_TOKEN` retains all write duties.
   Enable GitHub's repository-level [immutable Releases](https://docs.github.com/en/code-security/concepts/supply-chain-security/immutable-releases),
   and activate a no-bypass repository tag ruleset whose only include is
   `refs/tags/app-v*`, with no excludes and both **Restrict updates** and
   **Restrict deletions**. Then create and push a signed App tag named
   `app-v<ha_voice_hermes_gateway/config.yaml version>` from the exact current
   `main` head, for example `app-v0.1.0`. It must be an annotated tag whose
   signature GitHub verifies. The hardened workflow requires its target to equal
   `origin/main` exactly and rejects an older main commit, a mismatched version,
   or a version tag already present in GHCR; never delete or move a release tag.
6. Let the App workflow build and keyless-sign the architecture images in new
   run-ID-namespaced GHCR staging packages. The preflight must see each main/arch
   staging package as absent or private, and post-push checks must prove all
   three remain private. Generate/attest exact-digest all-layer SBOMs and the
   private staging multi-architecture digest. Never make a staging package
   public.
7. While staging remains private, verify every image signature,
   provenance/SBOM attestation and standalone SBOM; create deterministic
   embedded-license and per-architecture Debian-copyright archives; verify both
   architectures against the Debian source lock; build the corresponding-source
   archive from immutable Debian snapshots; and keyless-sign `SHA256SUMS`.
8. Publish the source/evidence GitHub Release **before** distributing the App
   binary. The job must create a draft, upload through GitHub's Release uploader,
   byte-verify the complete set, publish it non-draft/non-prerelease, require
   GitHub to report it immutable, then verify the native release attestation and
   every local asset. At this point the Release is a complete public source
   offer; it is not yet an image-installability claim.
9. Only after that immutable Release succeeds, let the separately permissioned
   promotion job copy the exact digest from private staging to the public
   versioned and `latest` tags, verify both, then keyless-sign and attest the
   public reference. A fresh job with no registry login must resolve both tags,
   pull `amd64` and `arm64`, and verify the public signature and promotion
   provenance. If the new public package is private by default, make only
   `ha-voice-hermes-gateway` public and re-run **failed jobs** in this same run;
   never expose the staging packages. Source-first ordering is deliberate, and
   no atomic image-plus-Release publication claim is made.
10. For any partial write/upload/sign/attest/visibility failure, use **Re-run
    failed jobs**, not **Re-run all jobs**, on the same workflow run. Its
    run-specific private staging packages and 90-day evidence artifact are the
    recovery boundary. A retry may accept only the exact Release asset set and
    version digest. Promotion must also prove its immutable `app-v*` Release is
    still the newest published App Release, so a delayed older retry cannot
    roll `latest` backward. If that recovery state expires, or a newer App
    Release now exists, publish a new version rather than rebuilding behind an
    immutable Release.
11. Independently run `gh release verify app-v<version>`, download the Release
    assets, and run `gh release verify-asset app-v<version> <path>` for every
    asset. Also verify `SHA256SUMS` and its Sigstore bundle, inspect/pull the
    image digest, then add the repository to a clean Home Assistant App store
    and complete an anonymous install/start check on each supported
    architecture.
12. Publish the separately built and physically validated factory/WebSerial
    assets with the signed firmware/source release. The App Release deliberately
    contains no fabricated factory binary. Publish the hardware test and App
    install/backup/restore records, then re-run clean Dashboard Import and
    first/hardened OTA from the published firmware URL.

If a release must be withdrawn, publish a new version and mark the old release
unsupported. Never move an existing device-installation tag.
