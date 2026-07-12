# Home Assistant Voice Hermes Gateway App

This experimental Home Assistant App (the July 2026 name for an add-on) runs
the repository's optimized Rust/WASM gateway locally. It is an alternative
deployment target for the same realtime v2 protocol used by the Cloudflare
Worker; it is not a different or buffered voice implementation.

```text
Voice PE ── WSS/TLS ── Home Assistant App (`workerd` + local Durable Object)
                             ├─ ElevenLabs Scribe v2 Realtime WSS
                             ├─ Hermes /v1/responses SSE
                             └─ ElevenLabs Flash v2.5 multi-context TTS WSS
```

Microphone PCM leaves the device while the user is speaking. Hermes begins
only after Scribe commits a final transcript, because tool execution from a
mutable partial transcript is unsafe. Complete spoken phrases are sent to TTS
before Hermes finishes, and TTS PCM is sent to the Voice PE before TTS finishes.
This is the same streaming pipeline and conversation logic described in the
root [architecture](../docs/architecture.md) and [protocol](../docs/protocol.md).

## Status and trust boundary

The App is deliberately marked **experimental**:

- the gateway and its automated Workers-runtime tests pass, but this App has
  not been validated on Home Assistant OS or against a physical Voice PE;
- `workerd` is a self-hosting-capable runtime, but Cloudflare warns that it is
  [not a hardened sandbox](https://github.com/cloudflare/workerd#warning-workerd-is-not-a-hardened-sandbox);
  this App therefore runs it inside Home Assistant's protected container with a
  custom AppArmor profile;
- the App uses `workerd`'s single-machine `localDisk` Durable Object backend.
  Treat that self-hosted backend as experimental; it does not provide the
  managed replication, placement, or availability of Cloudflare Durable
  Objects; and
- `workerd` is exactly pinned to `1.20260708.1` by the App package lock.
  Upgrading it is a tested release action, never a floating install.

The App follows current [Home Assistant App configuration](https://developers.home-assistant.io/docs/apps/configuration/)
and [security](https://developers.home-assistant.io/docs/apps/security/)
guidance. It does not request host networking, ingress, the Home Assistant API,
the Supervisor API, Docker, audio, devices, privileged capabilities, or a
writable Home Assistant directory. Only the App's `/data`, a tmpfs, outbound
network access, and the read-only `/ssl` mount are used. Port 8443 is published
solely so Voice PEs can reach the realtime WSS endpoint.

## Prerequisites

- Home Assistant 2026.7.0 or newer on OS or Supervised, on `amd64` or
  `aarch64`.
- A DNS name reachable from every Voice PE.
- A complete TLS chain and private key in Home Assistant's `ssl` directory.
  The defaults are `fullchain.pem` and `privkey.pem`.
- A dedicated ElevenLabs API key, ElevenLabs voice ID, and access to Scribe v2
  Realtime plus Flash v2.5 TTS.
- An audited Hermes Agent API with a dedicated voice-safe profile and bearer.
  The gateway's contract baseline is commit
  [`5ecc079`](https://github.com/NousResearch/hermes-agent/tree/5ecc07986f46463ca3096679b03a46402eb19cee).
- One unique random gateway token and one intentional Hermes long-term-memory
  scope for every Voice PE.

The Voice PE validates server hostname and certificate time. Its gateway URL
must therefore use a certificate issued by a public CA trusted by the firmware,
with a SAN matching that exact DNS name, and `fullchain.pem` must include every
needed intermediate. The App also accepts only publicly trusted TLS for outbound
Hermes connections; it does not add a private CA to the global provider trust
store.

## Install from this repository

This repository is both the firmware/gateway source and a Home Assistant App
repository. Home Assistant requires an App repository to carry
[`repository.yaml`](https://developers.home-assistant.io/docs/apps/repository/)
at its root, while each App lives in its own directory, so a second repository
is not required.

**These installation steps are not available yet.** As of 12 July 2026 the App
exists only on draft PR
[#1](https://github.com/troykelly/ha-voice-hermes/pull/1): `main` has no App
metadata, no signed `app-v0.1.0` tag has published the image, and the GHCR
package is unavailable. Continue only after that PR is reviewed and merged,
the signed release workflow succeeds, and the package passes its anonymous
public-pull check.

1. In Home Assistant, open **Settings → Apps → App store**.
2. Open the repository menu, add
   `https://github.com/troykelly/ha-voice-hermes`, and refresh the store.
3. Select **Home Assistant Voice Hermes Gateway** and install it.
4. Do not start it until TLS, Hermes, ElevenLabs, and at least one device entry
   are configured.

The configured `image:` makes Supervisor installation depend on the published
GHCR image; adding a development branch to the App store is not a local-build
workaround. A source checkout can run the documented Docker smoke tests, but a
Home Assistant installation remains gated on the immutable published image.

### Maintainer publication

The App publisher reads `version` from `ha_voice_hermes_gateway/config.yaml`.
Before tagging, create the Actions repository secret `RELEASE_POLICY_TOKEN`
from a fine-grained personal access token restricted to this repository, with
only **Administration: read** (and GitHub's implicit metadata read). Give it a
short expiry and rotate it through repository settings. GitHub's ephemeral
`GITHUB_TOKEN` cannot request that permission, so the dedicated token is used
only for read-only ruleset and immutable-Release checks; package and Release
writes continue to use the job-scoped `GITHUB_TOKEN`. A missing or expired
policy token fails closed before any registry write.

PR #1 must be **squash-merged**, and its feature branch must then be deleted.
The repository is configured for squash-only merges. This is a security
boundary: a tag executes the workflow stored at the tagged commit, so the older
feature-branch workflow snapshots must never become `main` ancestors. After all
release gates pass, push a signed tag named `app-v<version>` from the exact
current `main` head, for example `app-v0.1.0`. The tag must be annotated and
GitHub-verified as signed. The hardened workflow requires the tag target to
equal `origin/main` exactly; it rejects an older main commit, a tag/version
mismatch, or an already published version.

Before receiving package-write permission the workflow regenerates and checks
the locked WASM, native `workerd`, and base-container third-party notice
inventories, including the release-blocking native `workerd` closure. It then
builds and keyless-signs native `amd64` and `aarch64` images in three dedicated
run-ID-namespaced GHCR staging packages. Preflight accepts only absent or
private staging packages, and the workflow rechecks all three remain private
after upload. No staging package is an installation endpoint.

While those images are private, the evidence job verifies every signature and
attestation, compares each all-layer SPDX SBOM to its signed predicate,
packages the complete embedded license tree and each architecture's Debian
copyright files, verifies both images against the Debian source lock, builds
the corresponding-source archive from pinned Debian snapshots, and signs
`SHA256SUMS`. Only then does the source-first job create a draft, use GitHub's
Release uploader for every asset, byte-verify the complete set, and publish a
non-draft, non-prerelease
[`app-v*` GitHub Release](https://github.com/troykelly/ha-voice-hermes/releases).
GitHub's repository-level
[immutable Releases](https://docs.github.com/en/code-security/concepts/supply-chain-security/immutable-releases)
must remain enabled. The workflow requires the published Release's `immutable`
result, verifies its native release attestation and every local asset with
`gh release verify[-asset]`, and only then allows public-image promotion.

The separately permissioned promotion job copies that exact digest from private
staging to the public versioned and `latest` tags, verifies both digests, then
keyless-signs and attests the public repository reference. A fresh, uncredentialed
job must finally resolve both public tags and pull `amd64` and `arm64`, verify the
public signature, and verify promotion provenance. Source and notices therefore
become publicly available before any App binary; public-image publication and
the Release are deliberately **not** claimed to be atomic.

The policy setting and exact no-bypass `refs/tags/app-v*` update/deletion
ruleset are checked before private staging writes, before the source Release,
and before public promotion. Do not grant `RELEASE_POLICY_TOKEN` write access
or reuse an App, provider, device, or local CLI credential for it. Never delete
or move an `app-v*` tag or replace a versioned image.

After any write boundary, use GitHub's **Re-run failed jobs** for the same
workflow run; do not re-run all jobs or start a fresh build. The run-specific
staging names and 90-day evidence artifacts make partial draft upload,
Release-attestation delay, partial tag promotion, signature/provenance failure,
and an initially private public package resumable without rebuilding. A retry
accepts only the same Release assets and version digest. If those artifacts
expire, publish a new version rather than recreating immutable evidence.
Immediately before writing the public tags, the promotion job also requires its
immutable `app-v*` Release to be the newest published App Release. Consequently,
a delayed retry from an older run fails closed instead of moving `latest`
backward after a newer Release.

A first public-package promotion may create a private GHCR package. In that
case the source/evidence Release is already safely public; make only
`ha-voice-hermes-gateway` public in package settings, leave every
`*-staging-<run-id>` package private, and re-run the failed **Verify anonymous
public installability** job. Before announcing the App, independently
run `gh release verify app-v<version>` and `gh release verify-asset` for every
downloaded asset, verify its signed checksums/attestations and exact versioned
manifest without registry credentials, then install it from a clean Home
Assistant App store.
Factory/WebSerial firmware is a separately signed and physically validated
firmware release; the App workflow does not invent or attach a factory binary.
The full source, artifact, physical, and anonymous installation gates are in the
root [release checklist](../docs/release-checklist.md).

## TLS setup

Place the certificate through your normal Home Assistant certificate workflow:

```text
/ssl/fullchain.pem
/ssl/privkey.pem
```

These are host paths. The Supervisor maps the `ssl` directory read-only at the
same `/ssl` path inside the App. In the Configuration tab use relative names:

```yaml
certfile: fullchain.pem
keyfile: privkey.pem
certificate_poll_seconds: 60
```

Absolute paths, path separators, `..`, symlinks, and missing/empty files are
rejected. Put the configured files directly in `/ssl`. Keep the source private
key restricted; the App copies its runtime snapshot with owner-only access. The
chain must validate for TLS server use against the App's public CA bundle;
self-signed/private-CA chains fail before the runtime starts. The Voice PE still
performs the decisive hostname/SAN and time validation for its configured WSS
URL.

The App validates a complete certificate/key/options snapshot, requires the same
fingerprint on two consecutive polls so sequential renewal writes cannot trigger
a partial reload, and then restarts `workerd`. Allow two
`certificate_poll_seconds` intervals plus startup time. Existing Voice PE sockets
disconnect and reconnect automatically. A normal reconnect retains the durable
conversation head, but renewing during a Hermes/tool turn can create an ambiguous
turn. Renew or restart while devices are idle when possible; if a device reports
`conversation_ambiguous`, use **New Hermes Conversation** before speaking again.

Reload failure behavior is deliberately asymmetric. Invalid changed options or
runtime configuration stop the App, so revoked or replaced credentials cannot
remain active in an old process. An invalid or incomplete certificate/key update
retains the active validated certificate only when the normalized options are
byte-for-byte unchanged; this narrow exception lets an atomic certificate
renewal recover on a later poll without taking down the current TLS endpoint. If
options changed at the same time, the App stops instead of retaining either old
credentials or ambiguous configuration. While that exception is active, the
container health check reports degraded. Retention is bounded to the earlier of
the active certificate's expiry or a grace period of three poll intervals (with
a 10-minute minimum); persistent invalid source files stop the App.

Test the externally reachable endpoint, including its chain and name:

```sh
curl --fail https://voice.example.com:8443/healthz
openssl s_client -connect voice.example.com:8443 \
  -servername voice.example.com -showcerts </dev/null
```

Keep the published App port on a trusted LAN or VPN and use host/router firewall
rules to deny untrusted segments. If a reverse proxy publishes the App on 443,
preserve WebSocket upgrades, proxy to the App's TLS port, and apply sensible
handshake rate, concurrent-connection, and idle-connection limits without
buffering or timing out active audio streams. Do not expose 8443 directly to the
public Internet. Do not terminate to plaintext unless you have also designed and
tested an explicit trusted inner transport; the shipped Voice PE configuration
accepts `wss://` only.

## Hermes setup

Hermes must expose its Responses API over HTTPS. `http://`, credentials embedded
in the URL, fragments, and unsafe redirects are rejected. Because the App does
not use host networking, `127.0.0.1` refers to the App container, not the Home
Assistant host. Use a DNS name/IP reachable from the container and put TLS in
front of Hermes.

For a public Hermes origin or Cloudflare Tunnel/Access:

```yaml
hermes_base_url: https://hermes-api.example.com
allow_private_upstreams: false
```

For an intentionally private LAN Hermes origin:

```yaml
hermes_base_url: https://hermes.internal.example
allow_private_upstreams: true
```

The internal hostname still needs a certificate chaining to a public CA; DNS-01
ACME issuance is suitable for a split-DNS/private service. There is no custom
Hermes CA option: changing workerd's global trust store would also affect
ElevenLabs. `allow_private_upstreams` is an explicit global network capability
expansion: when `false`, all private upstream resolution is denied; when `true`,
every gateway fetch/WebSocket may resolve to private ranges.
Loopback and workerd-local destinations remain denied. Turn it on only for an
isolated network where Hermes genuinely needs it, and use a reachable LAN
hostname/address rather than `127.0.0.1`. Keep the Hermes API authenticated even
on a trusted LAN. The gateway rejects HTTPS redirects rather than replaying its
bearer or Access credentials to another location.

Use a dedicated Hermes profile with constrained tools, sandbox and approval
policy. A Voice PE bearer identifies hardware, not the person speaking, and this
Responses streaming path cannot conduct Hermes Runs API approval exchanges.
Never auto-approve dangerous tools for a room microphone.

The following settings select and fence the agent binding:

```yaml
hermes_model: hermes-agent
hermes_profile_id: voice-safe
hermes_binding_revision: v1
```

`hermes_model` is API compatibility metadata, not a generic agent selector. The
base URL/API key select the actual Hermes API/profile. Give the profile a stable
ID and bump `hermes_binding_revision` whenever its SOUL, tools, sandbox, memory,
approval policy, or spoken instructions change. The gateway starts a new
short-term response chain before crossing a changed binding.

If Hermes is protected by a Cloudflare Access service-token policy, set both
`cf_access_client_id` and `cf_access_client_secret`. Leave both empty otherwise;
exactly one configured value is rejected.

## ElevenLabs setup and realtime behavior

Create a dedicated, least-privilege ElevenLabs key and choose a voice ID:

```yaml
elevenlabs_api_key: !secret hvh_elevenlabs_api_key
elevenlabs_voice_id: your-elevenlabs-voice-id
realtime_stt_model: scribe_v2_realtime
tts_model: eleven_flash_v2_5
elevenlabs_enable_logging: true
```

Realtime v2 always uses ElevenLabs Scribe Realtime for streaming input and the
multi-context TTS WebSocket for streaming output. The disabled buffered
diagnostic can use another STT implementation, but it is not a realtime fallback
and must not be counted in latency results. `eleven_v3` is not supported by the
multi-context TTS socket.

`elevenlabs_enable_logging: false` requests ElevenLabs Zero Retention Mode.
That mode is account/contract dependent; verify the request succeeds and that
provider history is absent before making a zero-retention claim.

## Device enrollment and conversation scopes

Read **Hermes Device ID** from each adopted Voice PE. Its default is the
hardware-derived `voice-pe-<wifi-mac>`, independent of the ESPHome node name.
Generate a different 32-byte token for each unit:

```sh
openssl rand -hex 32
```

Add every device to the App Configuration:

```yaml
devices:
  - device_id: voice-pe-a1b2c3d4e5f6
    device_token: !secret hvh_device_01_token
    hermes_session_key: agent:main:voice:room:kitchen
  - device_id: voice-pe-112233445566
    device_token: !secret hvh_device_02_token
    hermes_session_key: agent:main:voice:room:office
```

Device IDs and tokens must be unique. Every device must have an explicit
`hermes_session_key`; there is no shared-token or implicit-memory option in the
App's easy configuration. Reuse a session key only when those devices are
intentionally allowed to share Hermes long-term memory. A session key is not a
bearer, but it can disclose a person/room label and should still be treated as
sensitive. Provider keys, Access credentials, device tokens, and session keys
must use visible ASCII without spaces because they become protocol/HTTP header
values; use an opaque or colon-delimited scope instead of a Unicode room name.

After saving and starting the App, configure the adopter-owned ESPHome YAML:

```yaml
substitutions:
  hermes_gateway_url: "wss://voice.example.com:8443/v2/realtime"
  hermes_device_token: !secret hermes_device_token
```

The token must match that exact device entry. Install the enrollment image over
ESPHome OTA. Hermes and ElevenLabs keys never go into firmware. See the root
[deployment guide](../docs/deployment.md#6-configure-firmware) and
[adoption guide](../docs/esphome-adoption.md) for factory, Bluetooth-proxy,
Native API, OTA-hardening, and recovery steps.

The App creates one local Durable Object per device. A completed Hermes response
ID is promoted only after its terminal success, then supplied as
`previous_response_id` on the next utterance. Reconnect, reboot, ordinary wake,
cancellation, and barge-in retain the chain. A 2–5 second center-button hold or
the encrypted Home Assistant **New Hermes Conversation** button rotates the
short-term chain. `conversation_idle_seconds` defaults to 900 seconds and is an
additional shared-room boundary. The explicit `hermes_session_key` survives
these short-term resets and continues Hermes long-term memory.

## Complete configuration example

```yaml
certfile: fullchain.pem
keyfile: privkey.pem
allow_private_upstreams: false
certificate_poll_seconds: 60

hermes_base_url: https://hermes-api.example.com
hermes_api_key: !secret hvh_hermes_api_key
hermes_model: hermes-agent
hermes_profile_id: voice-safe
hermes_binding_revision: v1

elevenlabs_api_key: !secret hvh_elevenlabs_api_key
elevenlabs_voice_id: your-elevenlabs-voice-id
realtime_stt_model: scribe_v2_realtime
tts_model: eleven_flash_v2_5
elevenlabs_enable_logging: true

conversation_idle_seconds: 900
realtime_max_audio_seconds: 30
realtime_max_output_seconds: 30
max_connection_audio_seconds: 900
max_turns_per_connection: 256
max_messages_per_connection: 16384
diagnostic_v1_enabled: false

devices:
  - device_id: voice-pe-a1b2c3d4e5f6
    device_token: !secret hvh_device_01_token
    hermes_session_key: agent:main:voice:room:kitchen
```

Omit unused optional fields. Add `hermes_voice_instructions`, both
`cf_access_client_*` fields, or
`stt_language_code` only when supplying non-empty values.

Save the configuration and start the App. The log should report configuration
validation and readiness without printing any credential, transcript, spoken
text, or provider body. Then verify:

```sh
curl --fail https://voice.example.com:8443/healthz
```

Use a WebSocket client for an authenticated protocol smoke test as shown in the
root [deployment guide](../docs/deployment.md#5-build-deploy-and-inspect-health).

## Option reference

| Option | Default | Meaning |
| --- | --- | --- |
| `certfile` | `fullchain.pem` | Relative server certificate-chain path under `/ssl`. |
| `keyfile` | `privkey.pem` | Relative server private-key path under `/ssl`. |
| `allow_private_upstreams` | `false` | Expand all gateway egress to private ranges; loopback remains denied. |
| `certificate_poll_seconds` | `60` | Options/TLS snapshot interval; a valid changed generation must be stable across two polls before restart. |
| `hermes_base_url` | required | HTTPS base URL for the actual Hermes profile/API. |
| `hermes_api_key` | required, masked | Hermes bearer. |
| `hermes_model` | `hermes-agent` | Advertised Responses API model/route label. |
| `hermes_profile_id` | `voice-safe` | Stable identity of the selected server-side profile. |
| `hermes_binding_revision` | `v1` | Operator-controlled behavior revision used to fence conversations. |
| `hermes_voice_instructions` | empty | Optional spoken-answer instructions; potentially sensitive. |
| `cf_access_client_id` | empty | Optional Access service-token ID; requires its secret. |
| `cf_access_client_secret` | empty | Optional Access service-token secret; requires its ID. |
| `elevenlabs_api_key` | required, masked | Realtime STT/TTS credential. |
| `elevenlabs_voice_id` | required | Voice used for replies. |
| `realtime_stt_model` | `scribe_v2_realtime` | Realtime ElevenLabs transcription model. |
| `tts_model` | `eleven_flash_v2_5` | Low-latency multi-context TTS model. |
| `elevenlabs_enable_logging` | `true` | Provider logging request; `false` requires eligible Zero Retention Mode. |
| `stt_language_code` | empty | Optional language hint; empty enables automatic detection. |
| `conversation_idle_seconds` | `900` | Idle boundary for a new short-term conversation. |
| `realtime_max_audio_seconds` | `30` | Maximum input audio per turn. |
| `realtime_max_output_seconds` | `30` | Maximum decoded reply PCM per turn. |
| `max_connection_audio_seconds` | `900` | Cumulative per-socket and durable 24-hour audio allowance. |
| `max_turns_per_connection` | `256` | Per-socket and durable 24-hour attempted-turn allowance. |
| `max_messages_per_connection` | `16384` | Per-socket and durable 24-hour application-message allowance. |
| `diagnostic_v1_enabled` | `false` | Exposes the stateless buffered diagnostic only; never a realtime fallback. |
| `devices` | required | Device ID, unique masked token, and explicit Hermes memory scope for every unit. |

The App validates the gateway's hard bounds as well as the Supervisor schema.
Do not increase the capture/output limits to disguise a queue, latency, or tool
policy problem.

## Secret management

The credential fields use Home Assistant's `password` schema type, which masks
them in ordinary App forms. That is display protection, not a hardware secret
store or encryption-at-rest guarantee.

There is one deliberate Supervisor network disclosure attached to that schema
type. The current July 2026 Supervisor locally computes SHA-1 for every non-empty
App option declared as `password`, and its throttled App-pwned check sends the
first five hexadecimal hash characters to
`api.pwnedpasswords.com/range/<prefix>` for a k-anonymous breach lookup. It does
not send the clear value or complete hash, but the service sees the prefix,
request time, and source IP. This applies to high-entropy API/device tokens too,
because they are masked with the recommended `password` type. The behavior is
visible in Supervisor's pinned [option hashing](https://github.com/home-assistant/supervisor/blob/1e81816c855310f3217b7f5113573b1b661d4f4d/supervisor/apps/options.py#L150-L153),
[daily check](https://github.com/home-assistant/supervisor/blob/1e81816c855310f3217b7f5113573b1b661d4f4d/supervisor/resolution/checks/app_pwned.py#L31-L52),
and [range request](https://github.com/home-assistant/supervisor/blob/1e81816c855310f3217b7f5113573b1b661d4f4d/supervisor/utils/pwned.py#L10-L29)
source. Keep the masked schema, but include this metadata disclosure in the
deployment's privacy decision.

For convenience, put uniquely named values in Home Assistant's
`/config/secrets.yaml` and use `!secret name` in the App's YAML configuration.
Home Assistant documents the general [`!secret` mechanism](https://www.home-assistant.io/docs/configuration/secrets/).
For example:

```yaml
# /config/secrets.yaml (clear text; never commit or share this file)
hvh_hermes_api_key: "replace-with-the-Hermes-bearer"
hvh_elevenlabs_api_key: "replace-with-the-ElevenLabs-key"
hvh_device_01_token: "replace-with-64-random-hex-characters"
```

Then use `hermes_api_key: !secret hvh_hermes_api_key`,
`elevenlabs_api_key: !secret hvh_elevenlabs_api_key`, and the corresponding
nested `device_token: !secret hvh_device_01_token` in the App's YAML
configuration.

The separation reduces accidental copying, but it does not keep App credentials
opaque to Supervisor: Supervisor resolves the reference and supplies clear
values in the App's persistent `/data/options.json`. Those resolved values and
the App configuration are included in App backups. A UI save may also materialize
resolved values rather than preserve the reference. Check the Configuration tab
after edits and never paste it into an issue. Supervisor's current resolver logs
the requested **secret name**, not its value, at info level; use neutral names if
a room/person label in logs would itself be PII. See the pinned
[resolver source](https://github.com/home-assistant/supervisor/blob/1e81816c855310f3217b7f5113573b1b661d4f4d/supervisor/homeassistant/secrets.py#L24-L32).

`/data/options.json` and App backups also contain the stable
`voice-pe-<Wi-Fi-MAC>` identifier and unmasked model/profile/revision labels.
Treat them as linkable operational metadata, use non-identifying labels where
possible, and redact them from diagnostics before sharing.

Do not enable Supervisor DEBUG logging while entering or rotating these values.
Treat Supervisor/App debug logs and diagnostics as potentially containing
resolved option data; return to normal logging, inspect locally, and redact before
sharing. The launcher snapshots and validates the Supervisor options and `/ssl`
files without following symlinks, then hands a mode-restricted tmpfs generation
under `/tmp/ha-voice-hermes` to the dedicated unprivileged `gateway` account.
Reload probes stay root-only until selected; the old runtime is stopped before
ownership of the replacement generation is handed over. After `/healthz`
succeeds the launcher deletes that generation. Core dumps are disabled, and it
does not pass secrets in process arguments or exported environment variables.
Option values must never be logged.

Use independent keys for this service, keep the Hermes profile least-privileged,
scope the ElevenLabs key, and rotate immediately after suspected exposure. Never
commit `secrets.yaml`, `/data/options.json`, a generated runtime config, a backup,
or a diagnostic bundle to this repository.

For an emergency credential rotation, stop the App first, edit and save the new
Hermes, ElevenLabs, Access, or device credential, and only then start the App and
verify readiness. This prevents a polling interval from leaving the old
credential active. Revoke the exposed credential at its provider as soon as the
replacement is ready; device-token rotation also requires installing the
matching token on that one Voice PE before it can reconnect.

For a compromised TLS private key, also stop the App first. Replace both the key
and certificate chain through the normal Home Assistant certificate workflow,
revoke the compromised certificate with its issuer, then start the App and
verify the served fingerprint/SAN from another machine. Do not rely on the
renewal watcher for emergency key revocation: its narrow grace behavior is for
incomplete routine renewal files, not compromise response.

## Backups, restore, and cloning

The App declares a **cold backup**, so Supervisor stops it before copying local
Durable Object state. This prevents a SQLite/WAL snapshot during a write. `/data`
contains the local response pointers, conversation UUIDs, recovery journal,
quotas, and the resolved App options; a full backup should also include `ssl` if
you expect to restore the certificate files.

Home Assistant's current [backup guidance](https://www.home-assistant.io/common-tasks/general/#backups)
supports encrypted backups and recommends an off-device/off-site copy. Enable
encryption, download the [backup emergency kit](https://www.home-assistant.io/more-info/backup-emergency-kit/),
and store it outside Home Assistant. Losing the matching emergency-kit key can
make an encrypted backup unrestorable. A backup containing these options has
Hermes, ElevenLabs, Access, and device credentials; handle it like a credential
vault even when encrypted.

For an in-place replacement restore:

1. stop the old App and ensure it cannot return;
2. restore the App and its `ssl` files from a cold encrypted backup;
3. verify DNS, certificate SAN/chain, `/healthz`, and the effective WSS port;
4. start the restored App once, then allow devices to reconnect; and
5. explicitly start a new Hermes conversation if the saved completed response
   no longer exists at Hermes.

Never run a restored clone and its source at the same time with the same WSS
hostname and device tokens. The copies have divergent local Durable Object
stores and can both spend provider credentials. For a deliberate clone or lab
restore, change the hostname/certificate, rotate Hermes/ElevenLabs/Access keys,
generate new device tokens and session scopes, and OTA the matching tokens to
the lab devices before exposing the clone. Decommission the old copy and revoke
its credentials.

Cloudflare Durable Object state and this App's local-disk state are separate and
there is no supported state converter. Moving a Voice PE from Cloudflare to the
local App, or back, therefore begins a new **short-term** conversation even if
the same `hermes_session_key` intentionally preserves Hermes long-term memory.
Make that cutover while idle, update the firmware WSS URL/token, and do not copy
opaque response/storage files between backends.

## Home Assistant media remains independent

The App is not an ingress UI and does not call Home Assistant. Optional Home
Assistant music, announcements, volume control, and the **New Hermes
Conversation** button remain available through the Voice PE's encrypted ESPHome
Native API and `speaker_source` media-player pipeline. Those media inputs are
mixed and ducked on the device; they never traverse this App and never receive
Hermes/ElevenLabs credentials. Stopping Home Assistant media must not stop
Hermes voice, and stopping the gateway must not invalidate normal ESPHome device
management or OTA.

## Operations and troubleshooting

- `https://<gateway-host>:<mapped-port>/healthz` is redacted readiness. A failure
  means configuration/runtime readiness failed; it is not a transcript endpoint.
- Confirm the Voice PE URL is exactly
  `wss://<certificate-SAN>:<mapped-port>/v2/realtime` and that local DNS resolves
  it from the device's network.
- `tls_failed` usually means an untrusted CA, missing intermediate, wrong SAN,
  expired/not-yet-valid certificate, or an unsynchronized device clock.
- Hermes on a private address additionally requires
  `allow_private_upstreams: true` and a publicly trusted certificate for its
  internal hostname.
- A stable valid configuration change restarts `workerd` and reconnects devices.
  Invalid changed options stop the App. Only an invalid TLS-file renewal with
  otherwise unchanged options retains the active validated certificate; fix the
  files and let a later poll retry.
- `conversation_ambiguous` is a human safety boundary: a Hermes tool may have
  run. Do not replay automatically; start a new conversation.
- Keep `diagnostic_v1_enabled: false` except for a short synthetic operator test.
  It is buffered, stateless, and excluded from realtime latency claims.

Use the root [test plan](../docs/testing.md) before production. App packaging,
TLS reload and local persistence tests do not substitute for physical AEC,
barge-in, audio quality, Wi-Fi fault, OTA, live Hermes/ElevenLabs, or measured
latency validation.

## Future work, not current behavior

Hermes-initiated announcements/conversation invitations and multiple wake words
that route to distinct Hermes agents remain documented future protocol work.
This App does not add a proactive ingress, group delivery, route-specific keys,
or wake-agent routing. See [proactive delivery](../docs/proactive-delivery.md),
[multi-agent wake routing](../docs/multi-agent-wake-routing.md), and the shared
[protocol v3 roadmap](../docs/protocol-v3-roadmap.md).

## References

- [Home Assistant App configuration (updated June 2026)](https://developers.home-assistant.io/docs/apps/configuration/)
- [Home Assistant App security](https://developers.home-assistant.io/docs/apps/security/)
- [Home Assistant App communication](https://developers.home-assistant.io/docs/apps/communication/)
- [Home Assistant App repository format](https://developers.home-assistant.io/docs/apps/repository/)
- [Home Assistant encrypted backups](https://www.home-assistant.io/common-tasks/general/#backups)
- [`workerd` self-hosting and security warning](https://github.com/cloudflare/workerd#readme)
