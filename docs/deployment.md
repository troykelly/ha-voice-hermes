# Deployment

This deployment keeps Hermes private, runs the realtime coordinator as the same
Rust/WASM gateway either on Cloudflare or in a protected Home Assistant App, and
gives each Voice PE one outbound authenticated WebSocket. Cloudflare provides a
managed Durable Object; the App uses experimental single-machine local-disk
Durable Object storage under its backed-up `/data` volume.

## 1. Prepare an audited Hermes build

Install or upgrade Hermes normally, then verify:

```sh
hermes --version
```

The audited baseline is commit [`5ecc079`](https://github.com/NousResearch/hermes-agent/tree/5ecc07986f46463ca3096679b03a46402eb19cee). It supplies Responses SSE, structured function-call events, persistent response chains, response retrieval, and the stable session-key header used by realtime v2. A semantic version is not a reproducible compatibility boundary: pin this exact commit/container digest or re-audit and pin another exact build.

Enable the API server in `~/.hermes/.env`:

```dotenv
API_SERVER_ENABLED=true
API_SERVER_HOST=127.0.0.1
API_SERVER_PORT=8642
API_SERVER_KEY=replace-with-at-least-32-random-bytes
API_SERVER_MODEL_NAME=hermes-agent
```

Keep the loopback bind. `API_SERVER_KEY` protects an endpoint that can invoke the agent's host tools.

Start the API adapter:

```sh
hermes gateway
```

Verify health, capabilities, and an SSE turn locally:

```sh
curl --fail http://127.0.0.1:8642/health

curl --fail http://127.0.0.1:8642/v1/capabilities \
  -H "Authorization: Bearer $API_SERVER_KEY"

curl --include --no-buffer --fail-with-body \
  http://127.0.0.1:8642/v1/responses \
  -H "Authorization: Bearer $API_SERVER_KEY" \
  -H "Content-Type: application/json" \
  -H "X-Hermes-Session-Key: voice:deployment-test" \
  -d '{
    "model":"hermes-agent",
    "input":"Reply with one short sentence.",
    "instructions":"Use plain spoken text.",
    "stream":true,
    "store":true
  }'
```

The headers should include `X-Hermes-Session-Id`. The stream must contain `response.created`, one or more `response.output_text.delta` events, and terminal `response.completed` with `response.status` equal to `completed`. Responses SSE does not end with `[DONE]`. Prefer the repository's executable contract check:

```sh
HERMES_BASE_URL=http://127.0.0.1:8642 \
HERMES_API_KEY="$API_SERVER_KEY" \
python3 scripts/verify-hermes-contract.py
```

Run it only against a restricted test profile: it creates stored test responses and exercises chaining, retrieval, exact missing-response errors, and best-effort cleanup. It does not make a replayed tool call or prove streaming idempotency; the separate source audit establishes that the pinned streaming branch bypasses the cache.

Copy the completed `resp_…` ID and verify an actual reply, not merely two successful prompts:

```sh
FIRST_RESPONSE_ID='resp_replace_from_completed_event'
curl --no-buffer --fail-with-body \
  http://127.0.0.1:8642/v1/responses \
  -H "Authorization: Bearer $API_SERVER_KEY" \
  -H "Content-Type: application/json" \
  -H "X-Hermes-Session-Key: voice:deployment-test" \
  -d "{\"model\":\"hermes-agent\",\"input\":\"What did I ask you to do in my previous message?\",\"stream\":true,\"store\":true,\"previous_response_id\":\"$FIRST_RESPONSE_ID\"}"
```

The answer should reflect the first turn and the returned `X-Hermes-Session-Id` should identify the same Hermes transcript session. Do not add a named `conversation`; realtime v2 owns and promotes its explicit completed pointer.

Hermes' actual model, tools, memory, and profile remain server-configured. `HERMES_BASE_URL` and `HERMES_API_KEY` must address the intended Hermes profile/API server; the request's `model`/Worker `HERMES_MODEL` is cosmetic compatibility metadata rather than a generic agent selector. Test representative tools before involving audio.

The Responses endpoint is the conversation interface. With `store: true`, a later request's `previous_response_id` reconstructs the complete prior transcript, tool calls, and tool results. The Worker also records the response's `X-Hermes-Session-Id` header, but continuation is driven by the completed response ID. `X-Hermes-Session-Key` is a separate operator-owned long-term-memory scope and intentionally survives explicit, idle, and confirmed-expiry short-term rotations; changing the configured scope fences the old chain and adopts the new key.

The audited streaming branch does **not** use Hermes' non-streaming `Idempotency-Key` cache. The Worker still sends a deterministic key and persists it for forward compatibility, but it never treats the key as proof that retrying is safe. A response that is still starting, missing, or incomplete after an interruption makes the device conversation ambiguous and blocks later turns until an explicit reset. Restrict the voice profile's tools accordingly; this is a Hermes-side contract limitation, not something Cloudflare can repair transparently.

### Voice-safe Hermes policy

A device bearer authenticates hardware, not the speaker. Configure a dedicated narrow voice profile/toolset or sandbox and fail closed for dangerous actions. The Responses request cannot impose a trustworthy per-caller tool allowlist on the current Hermes server, and the selected stream has no interactive approval-response channel. Hermes' structured approval flow belongs to the Runs API, so this gateway never auto-approves a pending command. See [Hermes security and approvals](https://hermes-agent.nousresearch.com/docs/user-guide/security/).

## 2A. Run the local Home Assistant App

Choose this path when the gateway should be self-hosted with Home Assistant. The
same repository is a valid [Home Assistant App repository](https://developers.home-assistant.io/docs/apps/repository/),
so a second repository is not required:

**This path is not installable yet.** As of 12 July 2026 the App is on draft PR
[#1](https://github.com/troykelly/ha-voice-hermes/pull/1), not `main`; no signed
`app-v0.1.0` tag or public GHCR image exists. Use the steps below only after the
reviewed merge, signed multi-architecture publish, and anonymous image-pull
gate succeed. The App's `image:` setting means adding a development branch to
the store cannot substitute for that published image.

1. Open **Settings → Apps → App store**, add
   `https://github.com/troykelly/ha-voice-hermes` as a repository, and install
   **Home Assistant Voice Hermes Gateway**.
2. Put a public-CA server chain and private key at the standard Home Assistant
   paths `/ssl/fullchain.pem` and `/ssl/privkey.pem`. The chain's SAN must match
   the DNS hostname in the Voice PE's WSS URL.
3. Configure an HTTPS Hermes origin, Hermes/ElevenLabs credentials, and one
   unique token plus explicit Hermes session scope for every hardware-derived
   device ID. A private Hermes address additionally requires the explicit
   `allow_private_upstreams: true` and a publicly trusted certificate for its
   internal hostname.
4. Save, start the App, and check
   `https://<gateway-host>:<mapped-port>/healthz` (8443 by default).
5. Put `wss://<certificate-SAN>:<mapped-port>/v2/realtime` and only that unit's
   matching device token in the adopter-owned ESPHome YAML, then install it over
   authenticated OTA.

`allow_private_upstreams` is a gateway-wide egress expansion, not a Hermes-only
exception. When enabled, every gateway fetch and WebSocket may resolve to
private network ranges; loopback/workerd-local destinations remain denied.
There is no custom Hermes CA option, so even a private/split-DNS Hermes hostname
must present publicly trusted TLS (DNS-01 ACME is suitable).

Keep the App's WSS port on a firewalled LAN or VPN. If a reverse proxy exposes
443, preserve WebSocket upgrades and streaming, keep TLS on the inner hop unless
an explicitly trusted alternative has been tested, and apply handshake rate,
concurrent-connection, and idle-connection limits. Do not expose 8443 directly
to the public Internet.

Valid changed options or TLS files are applied only after two identical polls.
Invalid changed options stop the App so stale credentials cannot remain active.
Only an invalid/incomplete certificate renewal with otherwise unchanged options
retains the active validated certificate and retries later. That state marks the
container unhealthy and is bounded by certificate expiry and a three-poll grace
period (minimum 10 minutes). For emergency secret or TLS-key rotation, stop the
App first, edit and save the replacement, revoke the exposed provider
credential/certificate, then start it and verify `/healthz` plus the served TLS
fingerprint before reconnecting devices.

The App consumes ElevenLabs Scribe v2 Realtime audio while the user is still
speaking and streams Flash v2.5 TTS audio before Hermes/TTS completion. It
retains the same completed-response conversation chain, reset rules, ambiguous
turn fence, unique device authentication, and explicit long-term-memory scope as
the Cloudflare deployment.

Home Assistant's `password` option type masks credentials in the ordinary App
form. `!secret` references are a useful editing convenience, but Supervisor
resolves them into clear values in `/data/options.json`; App backups contain
those options. Use encrypted backups, store the backup emergency kit off-device,
avoid Supervisor DEBUG logging during secret work, and inspect/redact diagnostics
before sharing them. The App requests no ingress, Home Assistant/Supervisor API,
host network, audio, device, or privileged access. Home Assistant music and
announcements remain on the independent encrypted ESPHome Native API media path.

The App declares cold backup so local SQLite state is copied while stopped.
Restoring a clone also restores credentials and response pointers: never run
source and clone together, and rotate provider, Hermes, Access and per-device
credentials/session scopes before using a lab clone. Cloudflare and local
Durable Object storage cannot be converted; cutting a device between targets
starts a new short-term conversation even if the same session scope preserves
intentional Hermes long-term memory.

The exact options, certificate polling/reconnect behavior, private-network
boundary, secret caveats, backup/restore procedure, and current experimental
status are in the App's [complete documentation](../ha_voice_hermes_gateway/DOCS.md).
Current [Home Assistant App configuration guidance](https://developers.home-assistant.io/docs/apps/configuration/)
documents `/data/options.json`, the read-only `/ssl` map, `password` schema type,
and cold backups. No Home Assistant OS or physical Voice PE validation has yet
been recorded for this App.

## 2B. Publish Hermes through Tunnel and Access for Cloudflare

Do not bind Hermes to a public interface. Point a Cloudflare Tunnel hostname at loopback:

```yaml
tunnel: YOUR_TUNNEL_ID
credentials-file: /path/to/YOUR_TUNNEL_ID.json

ingress:
  - hostname: hermes-api.example.com
    service: http://127.0.0.1:8642
  - service: http_status:404
```

Create a Cloudflare Access self-hosted application for `hermes-api.example.com`, then create a service token and an Access policy permitting that token. The Worker sends:

```http
CF-Access-Client-Id: <service-token-id>
CF-Access-Client-Secret: <service-token-secret>
Authorization: Bearer <Hermes API key>
```

Verify all layers from a controlled machine:

```sh
curl --fail https://hermes-api.example.com/v1/capabilities \
  -H "CF-Access-Client-Id: $CF_ACCESS_CLIENT_ID" \
  -H "CF-Access-Client-Secret: $CF_ACCESS_CLIENT_SECRET" \
  -H "Authorization: Bearer $API_SERVER_KEY"
```

Also confirm that omitting either the Access token or Hermes bearer fails. Tunnel keeps the origin outbound-only; Access and Hermes provide independent authorization.

## 3. Prepare the Cloudflare Worker

Install the build prerequisites:

```sh
rustup target add wasm32-unknown-unknown
cargo install worker-build --version 0.8.4 --locked
cd gateway
npm ci
```

Rust 1.96.0 and Node.js 22.22.3 are pinned at the repository root. Do not silently build a release with floating toolchains.

Realtime v2 requires this Durable Object configuration, already present in `gateway/wrangler.toml`:

```toml
compatibility_date = "2026-07-11"
compatibility_flags = ["no_websocket_standard_binary_type"]

[[durable_objects.bindings]]
name = "VOICE_SESSIONS"
class_name = "VoiceSession"

[[migrations]]
tag = "realtime-v2"
new_sqlite_classes = ["VoiceSession"]
```

Do not rename the class, namespace, or migration tag after a production deployment without adding a proper [Durable Object migration](https://developers.cloudflare.com/durable-objects/reference/durable-objects-migrations/). SQLite-backed Durable Objects are available on both Workers Free and Paid plans, subject to different quotas; see [Durable Objects pricing](https://developers.cloudflare.com/durable-objects/platform/pricing/). Provider sockets keep an object active during a turn, so measure duration and use Paid for a dependable installation if Free limits are tight.

## 4. Configure Worker variables and secrets

Set non-secret variables in `gateway/wrangler.toml`, the Cloudflare dashboard, or `.dev.vars` for local development:

```toml
[vars]
HERMES_BASE_URL = "https://hermes-api.example.com"
ELEVENLABS_VOICE_ID = "your-elevenlabs-voice-id"
REALTIME_STT_MODEL = "scribe_v2_realtime"
TTS_MODEL = "eleven_flash_v2_5"
HERMES_MODEL = "hermes-agent"
HERMES_PROFILE_ID = "voice-safe"
HERMES_BINDING_REVISION = "v1"

# Secure shared-room default. Disabling it requires a separate explicit flag.
CONVERSATION_IDLE_SECONDS = "900"
REALTIME_MAX_AUDIO_SECONDS = "30"
REALTIME_MAX_OUTPUT_SECONDS = "30"
MAX_CONNECTION_AUDIO_SECONDS = "900"
MAX_TURNS_PER_CONNECTION = "256"
MAX_MESSAGES_PER_CONNECTION = "16384"
DIAGNOSTIC_V1_ENABLED = "false"
ELEVENLABS_ENABLE_LOGGING = "true"

# Buffered /v1 diagnostic-route limits, used only if explicitly enabled:
STT_PROVIDER = "elevenlabs"
MAX_AUDIO_BYTES = "4194304"
MAX_AUDIO_SECONDS = "30"
MAX_HERMES_RESPONSE_BYTES = "2097152"
```

All current bindings are:

| Binding | Required | Default | Used by | Purpose |
| --- | --- | --- | --- | --- |
| `VOICE_SESSIONS` | yes for v2 | — | v2 | Durable Object namespace bound to `VoiceSession`. |
| `HERMES_BASE_URL` | yes | — | v1 + v2 | HTTPS Hermes origin, with or without `/v1`. |
| `ELEVENLABS_VOICE_ID` | yes | — | v1 + v2 | Voice used for replies. |
| `ELEVENLABS_BASE_URL` | no | `https://api.elevenlabs.io/v1` | v1 + v2 | Provider base; must be HTTPS. WebSocket URLs are derived from it. |
| `REALTIME_STT_MODEL` | no | `scribe_v2_realtime` | v2 | ElevenLabs realtime transcription model. |
| `TTS_MODEL` | no | `eleven_flash_v2_5` | v1 + v2 | ElevenLabs low-latency TTS model. Multi-context WebSocket does not support `eleven_v3`. |
| `HERMES_MODEL` | no | `hermes-agent` | v1 + v2 | Cosmetic advertised model/route name. The base URL/API key selects the actual Hermes profile and server-side model. |
| `HERMES_PROFILE_ID` | no | `default` (`voice-safe` shipped) | v2 binding | Stable identity for the server-side profile. |
| `HERMES_BINDING_REVISION` | no | `v1` | v2 binding | Bump when SOUL, tools, sandbox, memory, voice instructions, or another behavior-defining property changes. |
| `HERMES_VOICE_INSTRUCTIONS` | no | built-in concise spoken prompt | v1 + v2 | May be a secret; secret form takes precedence over a variable. |
| `CONVERSATION_IDLE_SECONDS` | no | `900` | v2 | Activity is accepted turn start or later successful completion. `0`/`off`/`none` also requires `ALLOW_UNBOUNDED_CONVERSATION=true`; omission keeps 900 seconds. |
| `ALLOW_UNBOUNDED_CONVERSATION` | no | `false` | v2 | Required acknowledgement before disabling inactivity rotation. |
| `STT_LANGUAGE_CODE` | no | auto | v1 + v2 | Optional provider language hint containing letters or `-`. |
| `REALTIME_MAX_AUDIO_SECONDS` | no | `30` | v2 | Hard-bounded to 1–30 seconds to stay below capture-ring/provider auto-commit limits. |
| `REALTIME_MAX_OUTPUT_SECONDS` | no | `30` | v2 | Hard-bounded to 1–30 seconds; decoded TTS PCM is counted before device playback across arbitrary provider chunks. |
| `MAX_CONNECTION_AUDIO_SECONDS` | no | `900` | v2 socket + device/day | Cumulative audio cap for each socket and the durable per-device 24-hour accounting window; accepted up to 86,400 seconds. |
| `MAX_TURNS_PER_CONNECTION` | no | `256` | v2 socket + device/day | Turn-attempt cap for each socket and durable 24-hour window; hard maximum 10,000. |
| `MAX_MESSAGES_PER_CONNECTION` | no | `16384` | v2 socket + device/day | Application-message cap for each socket and durable 24-hour window; hard maximum 1,000,000. |
| `DIAGNOSTIC_V1_ENABLED` | no | `false` | v1 | Expose the stateless diagnostic; disabled returns generic `404`. |
| `ELEVENLABS_ENABLE_LOGGING` | no | `true` | v1 + v2 | Explicit provider logging/retention request value. |
| `STT_PROVIDER` | no | `elevenlabs` | v1 only | `elevenlabs` or `openai-compatible` buffered transcription. Realtime v2 always uses ElevenLabs Scribe Realtime. |
| `STT_MODEL` | no | `scribe_v2` / `whisper-1` | v1 only | Buffered provider model. |
| `STT_BASE_URL` | fallback only | `https://api.openai.com/v1` | v1 only | HTTPS OpenAI-compatible transcription base. |
| `MAX_AUDIO_BYTES` | no | `4194304` | v1 only | Maximum complete WAV body; hard cap 16 MiB. |
| `MAX_AUDIO_SECONDS` | no | `120` (`30` shipped) | v1 only | Complete WAV duration, 1–600 seconds; it cannot change the v2 hard cap. |
| `MAX_HERMES_RESPONSE_BYTES` | no | `2097152` | v1 + v2 recovery | Maximum non-streaming Hermes JSON or stored-response reconciliation body; accepted 64 KiB–8 MiB. Live v2 SSE has its separate fixed 8 MiB stream cap. |
| `ALLOW_SHARED_DEVICE_TOKEN` | no | `false` | auth | Enables the shared-token compatibility path. |
| `ALLOW_IMPLICIT_HERMES_CONTEXT` | no | `false` | memory | Enables implicit `voice:<device-id>` scopes for unmapped devices. |

Create secrets:

```sh
# Required provider/agent credentials:
npx wrangler secret put ELEVENLABS_API_KEY
npx wrangler secret put HERMES_API_KEY

# Required per-device identity/memory maps:
npx wrangler secret put DEVICE_TOKENS_JSON
npx wrangler secret put HERMES_SESSION_KEYS_JSON

# Optional Access pair:
npx wrangler secret put CF_ACCESS_CLIENT_ID
npx wrangler secret put CF_ACCESS_CLIENT_SECRET
```

Cloudflare exposes encrypted secrets and plaintext string variables to Worker
code through the same runtime string-binding shape. The application can verify
presence, format, and pairing but cannot attest which deployment mechanism was
used. The checked-in `wrangler.toml` therefore contains no credential binding,
CI rejects those names under `[vars]`, and production operators must use
`wrangler secret put` for every table entry below.

Use a dedicated ElevenLabs key with only the required Speech-to-Text/Text-to-Speech scopes and an account-appropriate credit quota. ElevenLabs documents key scope and quota restrictions in its [authentication guide](https://elevenlabs.io/docs/api-reference/authentication). The Worker uses the key only in provider handshake/request headers.

The secure default is one unique token and one explicit memory scope for every device. Generate a token with `openssl rand -hex 32` (64 hexadecimal characters/32 random bytes). Both maps use exactly the hardware-derived device ID shown by the firmware:

Example `DEVICE_TOKENS_JSON` value (every token must be 32–512 visible ASCII bytes, `!` through `~`, and values must be unique):

```json
{
  "kitchen-voice-pe": "replace-with-64-random-hex-characters-for-kitchen-device",
  "office-voice-pe": "replace-with-an-independent-64-random-hex-token-for-office"
}
```

Additional secrets:

| Secret | Required | Purpose |
| --- | --- | --- |
| `ELEVENLABS_API_KEY` | yes | Realtime/buffered STT and realtime/HTTP TTS; visible ASCII only because it is sent as an HTTP header. |
| `HERMES_API_KEY` | yes | Hermes API bearer; high privilege; visible ASCII only. |
| `DEVICE_TOKENS_JSON` | yes by default | Exact device-ID → unique 32–512-visible-ASCII-byte token map. |
| `HERMES_SESSION_KEYS_JSON` | yes by default | Exact device-ID → stable Hermes long-term-memory scope map; must cover the token map. |
| `DEVICE_AUTH_TOKEN` | compatibility only | Shared 32–512-visible-ASCII-byte bearer, accepted only with `ALLOW_SHARED_DEVICE_TOKEN=true`. |
| `CF_ACCESS_CLIENT_ID` | optional pair | Access service-token ID protecting Hermes. It may be a secret or variable. |
| `CF_ACCESS_CLIENT_SECRET` | optional pair | Matching Access service-token secret. |
| `HERMES_VOICE_INSTRUCTIONS` | optional | Spoken-answer instructions; deploy as a secret if customized content is sensitive. |
| `STT_API_KEY` | v1 alternate only | OpenAI-compatible buffered STT bearer. |

Both Access values must be configured together. Put `CF_ACCESS_CLIENT_SECRET` in an encrypted Worker secret with `wrangler secret put`, never in `[vars]`. Cloudflare exposes variables and secrets to application code through the same string-binding shape, so the gateway can validate the pair but cannot determine how it was deployed; repository review, secret scanning, and the absence of credential names from `wrangler.toml` enforce this boundary. Do not put provider or Hermes credentials in firmware.

`HERMES_SESSION_KEYS_JSON` is Worker-owned rather than device-supplied, so a compromised device cannot choose another user's memory scope. Keys are 1–256 visible ASCII bytes with no spaces because they are sent in `X-Hermes-Session-Key`; use opaque or colon-delimited labels rather than Unicode room names. Example:

```json
{
  "kitchen-voice-pe": "agent:main:voice:room:kitchen",
  "office-voice-pe": "agent:main:voice:room:office"
}
```

Use one scope per person/room/device according to the Hermes memory behavior you want. Reuse a value only when those devices are intentionally allowed to share long-term memory. The secure loader rejects a per-device token map whose IDs are not all covered here. `ALLOW_IMPLICIT_HERMES_CONTEXT=true` deliberately restores the deterministic `voice:<device-id>` compatibility fallback; it is not the production default. Changing a device's resolved value rotates that device's short-term conversation before its next turn so the old response chain cannot enter another memory scope.

The default `CONVERSATION_IDLE_SECONDS=900` is a safe shared-room boundary. Every accepted turn start counts as activity even when it later fails or is cancelled; successful completion refreshes the timestamp again, so long tool turns do not consume the following idle window. Choose a longer 60–31,536,000-second room policy only deliberately. To make a chain unbounded, set the idle value to `0`, `off`, or `none` **and** set `ALLOW_UNBOUNDED_CONVERSATION=true`.

The Durable Object persists only a SHA-256 fingerprint of normalized `HERMES_BASE_URL`, `HERMES_MODEL`, `HERMES_PROFILE_ID`, `HERMES_BINDING_REVISION`, and the resolved per-device session key—not the raw values. A change to any value rotates the UUID and clears the completed head before the next turn. Increment the revision whenever the selected profile's SOUL, tools, sandbox, memory policy, approval behavior, or `HERMES_VOICE_INSTRUCTIONS` changes without an endpoint/profile-ID change. Ordinary API-key rotation at the same origin does not change the binding.

Shared-token mode exists only for controlled compatibility testing. It requires `ALLOW_SHARED_DEVICE_TOKEN=true` plus the `DEVICE_AUTH_TOKEN` secret. Because that credential admits any syntactically valid device ID, explicit session-map coverage cannot be proven; it also requires `ALLOW_IMPLICIT_HERMES_CONTEXT=true`. Do not combine these flags accidentally in production.

## 5. Build, deploy, and inspect health

Run all local checks before deployment:

```sh
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo audit
npm audit --audit-level=high
npm run build
npx vitest run --max-workers=1 --no-isolate
npx wrangler deploy --dry-run --outdir /tmp/wrangler-dry-run
```

For the Home Assistant App bundle, return to the repository root and use the
pinned Linux/amd64 builder; a native developer build is not a byte-reproducible
release artifact across host platforms:

```sh
scripts/build-gateway-worker-canonical.sh
scripts/sync-addon-worker-artifacts.sh --check
scripts/check-gateway-artifact-privacy.sh
```

Deploying the first version creates the `VoiceSession` SQLite class through the declared migration:

```sh
npx wrangler deploy
curl --fail https://voice.example.com/health
```

The shipped Wrangler file sets `workers_dev=false` and `preview_urls=false`; attach a production custom domain/route before deploy. The realtime and health routes must reach the Worker. The diagnostic route is reachable only if explicitly enabled:

```text
wss://voice.example.com/v2/realtime   primary
https://voice.example.com/health      redacted readiness
https://voice.example.com/v1/voice    optional, disabled by default
```

Cloudflare supports outbound WebSockets through fetch-based upgrade. The Rust gateway uses that form so it can attach ElevenLabs' `xi-api-key`; no provider key is exposed to the device. All credentialed provider and Hermes requests use workerd-supported manual redirect mode, then explicitly require the expected status. WebSocket upgrades require exact `101`; Hermes streaming creation requires exact `200` plus a `text/event-stream` media type; recovery accepts only its documented exact statuses. A `3xx` is rejected without following `Location`, so credentials are not replayed to a redirect target.

The spoken-text cleanup removes formatting, code fences, and URL-shaped text; it is not DLP. If Hermes returns an ordinary text password, token, medical fact, account number, or other PII, that text can still reach ElevenLabs and the room speaker. Deploy a restricted voice-safe Hermes profile/tool policy and test representative sensitive-result and prompt-injection cases before production use.

For a manual handshake smoke test, use a WebSocket client that can set headers and a subprotocol, for example:

```sh
websocat \
  -H="Authorization: Bearer $KITCHEN_DEVICE_TOKEN" \
  -H="X-Device-Id: voice-pe-a1b2c3d4e5f6" \
  --protocol hermes-voice.realtime.v2 \
  wss://voice.example.com/v2/realtime
```

Send the `hello` object from [`protocol.md`](protocol.md); expect `ready` with a non-empty `conversation_id`. This checks routing/authentication and durable conversation state, but not audio framing.

## 6. Configure firmware

The supported factory/adoption flow and the clone-local/manual flow share the same zero-secret base configuration.

### Factory and ESPHome Device Builder adoption

Build and flash `firmware/hermes-voice-pe.factory.yaml`. It contains no Wi-Fi, API, OTA, gateway or provider secret. Provision Wi-Fi with center-button-authorized Improv BLE or Improv Serial, add the discovered ESPHome node to Home Assistant, then select **Adopt** in ESPHome Device Builder.

The imported configuration must compile and install before Hermes is configured. It exposes **Hermes Device ID** as a hardware-stable `voice-pe-<wifi-mac>` value. Add that ID/token pair to `DEVICE_TOKENS_JSON` and an intentional memory scope for the same ID to `HERMES_SESSION_KEYS_JSON`, deploy the Worker configuration, then add these substitutions to the adopter-owned YAML and install again over native OTA:

```yaml
substitutions:
  device_id: ""  # hardware-derived default; normally leave empty
  hermes_gateway_url: !secret hermes_gateway_url
  hermes_device_token: !secret hermes_device_token
  ota_password: !secret ota_password
```

The factory Native API is temporarily keyless; the first adopted image installs Dashboard Import's generated API key, but Dashboard Import creates no OTA password. Set a fresh random `ota_password` during this same enrollment install. Until it completes, the handoff accepts passwordless native OTA and should remain on a trusted or isolated provisioning LAN.

The exact Bluetooth-proxy distinction, Dashboard Import prerequisites, first-boot sequence and recovery behavior are in [`esphome-adoption.md`](esphome-adoption.md). A release factory image is not distributable until its Dashboard Import URL and custom-component source resolve anonymously from a public immutable tag.

### Clone-local/manual configuration

Create the local secrets file:

```sh
cp firmware/secrets.example.yaml firmware/secrets.yaml
```

For realtime v2:

```yaml
wifi_ssid: "..."
wifi_password: "..."
ota_password: "..."
api_encryption_key: "32-byte-base64 ESPHome API key"

hermes_gateway_url: "wss://voice.example.com/v2/realtime"
hermes_device_token: "at-least-32-bytes-of-unique-random-token-material"
```

Use `firmware/hermes-voice-pe.local.yaml`. Its default empty `device_id` derives the same hardware-stable identifier exposed by the diagnostic entity. Put that identifier in both Worker maps; never reuse a token or explicit ID between physical devices. An explicit `device_id` override is supported for migration, but ESPHome rename, friendly-name change and API/OTA credential rotation should not change it.

The realtime component's relevant controls are:

```yaml
hermes_voice:
  gateway_url: "${hermes_gateway_url}"
  auth_token: "${hermes_device_token}"
  device_id: "${device_id}"
  silence_threshold: 250
  silence_duration: 500ms
  speech_timeout: 8s
  min_recording_duration: 300ms
  max_recording_duration: 30s
  request_timeout: 5min
  output_sample_rate: 16000
```

`silence_threshold` and `silence_duration` are hardware-tuning values, not universal constants. The 500 ms starting point reduces turn-end delay relative to buffered v1's original 900 ms, but it must pass the early-cutoff/noisy-room tests.

The supplied YAML wires the `okay_nabu` `micro_wake_word` instance into `hermes_voice`. The same wake phrase starts a turn while idle and requests barge-in while Hermes is thinking or replying; ordinary speech without that wake phrase does not interrupt the shipped configuration. Keep center-button cancellation enabled until the physical AEC/wake tests pass.

Conversation controls are intentionally distinct from turn controls:

- a short center-button press starts a turn while idle or cancels the active turn;
- holding the center button for 2–5 seconds while idle invokes `hermes_voice.new_conversation`;
- holding for at least 10 seconds retains the factory-reset gesture;
- the optional encrypted Home Assistant connection exposes **New Hermes Conversation**, which invokes the same ESPHome action.

The device sends `conversation.reset` with one 16-hex request ID and retains that ID until the gateway returns matching `conversation.reset.done`. If the connection drops after the Durable Object commits the reset, resending the same ID returns the same new conversation rather than rotating twice. Firmware refuses the action locally during an active turn; the gateway independently returns `conversation_busy` for any direct or raced reset. Cancel or let the turn finish, then request it while idle.

These actions rotate only the short-term Responses chain. They do not change the Hermes profile, Worker credentials, device ID, or `X-Hermes-Session-Key`. Reconnect, ESP32 reboot, wake, ordinary short press, cancel, and barge-in preserve the existing conversation automatically.

The current realtime firmware schema requires `wss://`. The gateway's buffered `/v1/voice` route is hidden unless the operator sets `DIAGNOSTIC_V1_ENABLED=true`; realtime firmware never calls it or switches transports after an ambiguous turn. It is a stateless operator diagnostic, not an earlier conversation mode or retry policy.

Wi-Fi can be provisioned with authorized Improv BLE (direct or through an active HA Bluetooth proxy), Improv Serial, or compiled secrets. BLE is disabled five seconds after Wi-Fi connects and must be stopped before wake inference or the persistent Hermes socket starts; it is re-enabled after Wi-Fi loss for recovery. The gateway URL/device token remain adopter-owned compile-time ESPHome settings installed through ordinary OTA—there is deliberately no unauthenticated page or Home Assistant entity for changing credentials. Both may be empty for initial adoption; exactly one is invalid; both valid values enable realtime voice.

### Optional Home Assistant media plane

The supplied firmware exposes an encrypted [ESPHome Native API](https://esphome.io/components/api/) media-player entity and the **New Hermes Conversation** template button. Factory onboarding/Device Builder generates the unique API key automatically. For a clone-local build, generate it with `openssl rand -base64 32` and store it as `api_encryption_key`. The button asks the device to use its authenticated Worker WebSocket; Home Assistant does not receive ElevenLabs/Hermes credentials or conversation history.

The mixer has three independent inputs. Hermes PCM is resampled into its own input. Home Assistant URLs feed separate HTTP/FLAC `speaker_source` pipelines: 48 kHz mono for announcements and 48 kHz stereo for music. The Hermes lifecycle immediately ducks both HA inputs by 20 dB for listening/thinking/reply playback, then restores announcements quickly and music over approximately one second on idle. If an HA announcement is still active, its normal music duck remains in force rather than being incorrectly released. Disabling or losing Home Assistant therefore removes media/announcement playback only, not voice operation. See ESPHome's [`speaker_source` media-player contract](https://esphome.io/components/media_player/speaker_source/) for the media URL/transcode behavior.

## 7. Build and flash

Connect the Voice PE over USB:

```sh
# Universal zero-secret factory/adoption image
uvx --from esphome==2026.6.5 esphome config firmware/hermes-voice-pe.factory.yaml
uvx --from esphome==2026.6.5 esphome compile firmware/hermes-voice-pe.factory.yaml
uvx --from esphome==2026.6.5 esphome run firmware/hermes-voice-pe.factory.yaml

# Or a clone-local preconfigured operator image
uvx --from esphome==2026.6.5 esphome run firmware/hermes-voice-pe.local.yaml
```

The zero-secret first-flash image is generated under:

```text
firmware/.esphome/build/hermes-voice-pe-factory/.pioenvs/hermes-voice-pe/firmware.factory.bin
```

The factory wrapper deliberately uses a different build cache from the configured local wrapper. Do not collapse them: alternating an empty factory API key and a configured encryption key in one incremental ESPHome build directory can retain an incompatible object set.

Subsequent updates use password-protected native ESPHome OTA. Factory onboarding/Device Builder creates a unique Native API key but not an OTA password, so the adopter must set `ota_password` on the immediate enrollment install; the clone-local wrapper supplies both from secrets. The encrypted Native API lets the unit attach to Home Assistant as a media player, but there is deliberately no ESPHome `voice_assistant` component: voice remains attached to Hermes.

The validated build uses ESPHome 2026.6.5 (the package minimum remains 2026.6.0), Voice PE `voice_kit` commit `0579e7b9`, XMOS firmware 1.3.1 by MD5, vendored `okay_nabu` and VAD model bytes pinned by `firmware/models/SHA256SUMS`, and an Apache-licensed `esp_websocket_client` fork pinned to an immutable repository commit. That fork explicitly aborts every HTTP 3xx handshake before any configured bearer can be replayed to a redirect target.

## 8. First hardware test

Watch USB logs:

```sh
uvx --from esphome==2026.6.5 esphome logs firmware/hermes-voice-pe.local.yaml
```

Expected phases are wake-ready, listening, speech detected, thinking, reply playback, recoverable error, Wi-Fi offline, and hardware mute. Exact LED animation is cosmetic; state ordering and cancellation are not.

Test center-button start/cancel and the idle-only 2–5 second new-conversation hold before tuning wake-word sensitivity. Verify one complete realtime sequence in logs:

```text
WSS ready → turn.ready → input frames/acks → response.start
→ tts.start → output frames/acks → tts.end → turn.done
```

Do not infer success from `turn.done` alone; listen for clipping, early cutoff, echo re-trigger, underruns, and delayed tail audio. Run the physical acceptance suite in [`testing.md`](testing.md).

## Operations

- Rotate a device token by updating `DEVICE_TOKENS_JSON` and the adopter-owned ESPHome secret, then install that unit over OTA.
- Rotate the Native API encryption key in Device Builder (or clone-local `firmware/secrets.yaml`) and update/re-pair the Home Assistant ESPHome integration; it is unrelated to the Worker token and hardware-derived device ID.
- Rotate ElevenLabs, Hermes, and Access credentials only in Worker secrets.
- Change Hermes long-term-memory ownership only through `HERMES_SESSION_KEYS_JSON`; the Durable Object detects the new resolved key and rotates the short-term UUID/head before the next turn. It does not delete the older records from Hermes' response store.
- Treat changes to `HERMES_BASE_URL`, `HERMES_MODEL`, `HERMES_PROFILE_ID`, or `HERMES_BINDING_REVISION` the same way: the next accepted turn is fenced into a new conversation. Credential rotation at an otherwise unchanged origin does not itself rotate the chain.
- Inspect Durable Object storage by opaque device ID/turn ID, but do not log or persist transcripts/audio for routine observability.
- Reset conversation continuity with the Home Assistant **New Hermes Conversation** button, the 2–5 second center hold, or another authenticated ESPHome automation invoking `hermes_voice.new_conversation`. Do not manually edit Durable Object storage or delete only the Hermes response: the reset durably rotates the UUID, clears the head/session metadata, and persists its request ID alongside the independent turn journal. There is no unauthenticated reset route.
- Treat `conversation_expired` as a visible boundary: it is emitted only for Hermes' exact structured missing-previous-response error, the current utterance fails without replay, and the next newly spoken turn begins fresh. A generic `404` is `hermes_failed` and preserves the old head.
- Treat `conversation_ambiguous` as a mandatory human boundary. The gateway blocks later turns because a tool may have run; use the physical hold or encrypted Home Assistant button to start a new conversation. Do not retry the utterance automatically.
- Monitor `response.failed`, missing terminal SSE events, provider close codes, queue overflow, reconnect rate, and latency histograms.
- Apply Cloudflare rate limiting to handshake and buffered routes. Device auth prevents authorized provider use; rate limiting bounds brute force and cost.
- Keep outbound Scribe/TTS sockets open only for active work so idle Durable Objects can hibernate.
