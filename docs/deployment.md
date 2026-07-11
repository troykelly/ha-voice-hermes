# Deployment

This deployment keeps Hermes private, runs the realtime coordinator as a Rust/WASM Cloudflare Worker plus a SQLite-backed Durable Object, and gives each Voice PE one outbound authenticated WebSocket.

## 1. Prepare an audited Hermes build

Install or upgrade Hermes normally, then verify:

```sh
hermes --version
```

The audited baseline is commit [`4aa499f`](https://github.com/NousResearch/hermes-agent/tree/4aa499ff9f3fcc0c38ce61da46805a4dcc8f612e), whose package version is declared as 0.18.2. It supplies Responses SSE, structured function-call events, persistent response chains, and the stable session-key header used by realtime v2. Current later source may still report the same package version, so `>=0.18.2` is not a reproducible compatibility boundary: pin this commit/container digest or re-audit and pin another exact build.

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

The headers should include `X-Hermes-Session-Id`. The stream must contain `response.created`, one or more `response.output_text.delta` events, and terminal `response.completed` with `response.status` equal to `completed`. Responses SSE does not end with `[DONE]`.

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

The Responses endpoint is the conversation interface. With `store: true`, a later request's `previous_response_id` reconstructs the complete prior transcript, tool calls, and tool results. The Worker also records the response's `X-Hermes-Session-Id` header, but continuation is driven by the completed response ID. `X-Hermes-Session-Key` is a separate operator-owned long-term-memory scope and intentionally survives explicit, idle, and stale-head short-term rotations; changing the configured scope fences the old chain and adopts the new key.

### Voice-safe Hermes policy

A device bearer authenticates hardware, not the speaker. Configure a narrow voice toolset or sandbox and fail closed for dangerous actions. The selected `/v1/responses` stream has no interactive approval-response channel. Hermes' structured approval flow belongs to the Runs API, so this gateway never auto-approves a pending command. See [Hermes security and approvals](https://hermes-agent.nousresearch.com/docs/user-guide/security/).

## 2. Publish Hermes through Tunnel and Access

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
rustup run stable cargo install worker-build --version 0.8.4 --locked
cd gateway
npm install
```

Rust 1.88 or newer is required by the locked dependencies.

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

# Optional; omitted/0/off/none keeps conversations open until explicit reset.
CONVERSATION_IDLE_SECONDS = "0"

# Buffered /v1 diagnostic-route defaults:
STT_PROVIDER = "elevenlabs"
MAX_AUDIO_BYTES = "4194304"
MAX_AUDIO_SECONDS = "120"
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
| `CONVERSATION_IDLE_SECONDS` | no | off | v2 | Optional inactivity boundary. Activity is an accepted turn start or later successful completion. `0`, `off`, `none`, or omission disables it; otherwise 60–31,536,000 seconds. |
| `STT_LANGUAGE_CODE` | no | auto | v1 + v2 | Optional provider language hint containing letters or `-`. |
| `MAX_AUDIO_SECONDS` | no | `120` | v1 + v2 | Server hard limit for one utterance; allowed 1–600 seconds. Firmware normally caps at 30 seconds. |
| `STT_PROVIDER` | no | `elevenlabs` | v1 only | `elevenlabs` or `openai-compatible` buffered transcription. Realtime v2 always uses ElevenLabs Scribe Realtime. |
| `STT_MODEL` | no | `scribe_v2` / `whisper-1` | v1 only | Buffered provider model. |
| `STT_BASE_URL` | fallback only | `https://api.openai.com/v1` | v1 only | HTTPS OpenAI-compatible transcription base. |
| `MAX_AUDIO_BYTES` | no | `4194304` | v1 only | Maximum complete WAV body; hard cap 16 MiB. |
| `MAX_HERMES_RESPONSE_BYTES` | no | `2097152` | v1 + v2 recovery | Maximum non-streaming Hermes JSON or stored-response reconciliation body; accepted 64 KiB–8 MiB. Live v2 SSE has its separate fixed 8 MiB stream cap. |

Create secrets:

```sh
# Required provider/agent credentials:
npx wrangler secret put ELEVENLABS_API_KEY
npx wrangler secret put HERMES_API_KEY

# Optional memory-scope map and Access pair:
npx wrangler secret put HERMES_SESSION_KEYS_JSON
npx wrangler secret put CF_ACCESS_CLIENT_ID
npx wrangler secret put CF_ACCESS_CLIENT_SECRET
```

Use a dedicated ElevenLabs key with only the required Speech-to-Text/Text-to-Speech scopes and an account-appropriate credit quota. ElevenLabs documents key scope and quota restrictions in its [authentication guide](https://elevenlabs.io/docs/api-reference/authentication). The Worker uses the key only in provider handshake/request headers.

Then choose one device-auth mode:

```sh
# Shared development token:
npx wrangler secret put DEVICE_AUTH_TOKEN

# Preferred production mapping; takes precedence when present:
npx wrangler secret put DEVICE_TOKENS_JSON
```

Example `DEVICE_TOKENS_JSON` value:

```json
{
  "kitchen-voice-pe": "long-random-token-for-kitchen",
  "office-voice-pe": "different-long-random-token-for-office"
}
```

Additional secrets:

| Secret | Required | Purpose |
| --- | --- | --- |
| `ELEVENLABS_API_KEY` | yes | Realtime/buffered STT and realtime/HTTP TTS. |
| `HERMES_API_KEY` | yes | Hermes API bearer; high privilege. |
| `HERMES_SESSION_KEYS_JSON` | no | Exact device-ID → stable Hermes long-term-memory scope map. Unmapped devices use `voice:<device-id>`. |
| `DEVICE_AUTH_TOKEN` | one auth mode | Shared 16–512 byte device bearer. |
| `DEVICE_TOKENS_JSON` | preferred auth mode | Exact device-ID → token map; supersedes shared bearer. |
| `CF_ACCESS_CLIENT_ID` | optional pair | Access service-token ID protecting Hermes. It may be a plain variable, but a secret is preferable. |
| `CF_ACCESS_CLIENT_SECRET` | optional pair | Matching Access service-token secret. |
| `STT_API_KEY` | v1 alternate only | OpenAI-compatible buffered STT bearer. |

Both Access values must be configured together. Do not put provider or Hermes credentials in firmware.

`HERMES_SESSION_KEYS_JSON` is Worker-owned rather than device-supplied, so a compromised device cannot choose another user's memory scope. Keys are 1–256 characters without control characters or leading/trailing whitespace. Example:

```json
{
  "kitchen-voice-pe": "agent:main:voice:room:kitchen",
  "office-voice-pe": "agent:main:voice:room:office"
}
```

Use one scope per person/room/device according to the Hermes memory behavior you want. Reuse a value only when those devices are intentionally allowed to share long-term memory. This mapping does not change the per-device Durable Object; if it is absent, the deterministic `voice:<device-id>` fallback is used. Changing a device's resolved value intentionally rotates that device's short-term conversation before its next turn so the old response chain cannot enter another memory scope.

Leave `CONVERSATION_IDLE_SECONDS` disabled initially. Explicit new-conversation control is predictable and matches Hermes' `/new` distinction between transcript continuity and stable memory. If a shared-room installation benefits from automatic expiry, choose an operator policy such as `1800` only after considering normal pauses. Every accepted turn start counts as activity even when it later fails or is cancelled; successful completion refreshes the timestamp again, so long tool turns do not consume the following idle window. No value below 60 seconds is accepted.

The Durable Object persists a non-secret conversation binding of normalized `HERMES_BASE_URL`, `HERMES_MODEL`, and the resolved per-device session key. A change to any of those values rotates the UUID and clears the completed head before the next turn. This is intentional fail-closed behavior: update the configuration, expect the first following utterance to start a new transcript chain, and do not attempt to copy a response ID between Hermes profiles or memory scopes. Ordinary API-key rotation at the same origin does not change this binding.

## 5. Build, deploy, and inspect health

Run all local checks before deployment:

```sh
rustup run stable cargo fmt --check
rustup run stable cargo test
rustup run stable cargo check
rustup run stable cargo clippy -- -D warnings
worker-build --release
npx wrangler deploy --dry-run
```

Deploying the first version creates the `VoiceSession` SQLite class through the declared migration:

```sh
npx wrangler deploy
curl --fail https://YOUR-WORKER.example.workers.dev/health
```

If using a custom hostname, both routes must reach this Worker:

```text
wss://voice.example.com/v2/realtime   primary
https://voice.example.com/v1/voice   buffered diagnostic compatibility route
```

Cloudflare supports outbound WebSockets through fetch-based upgrade. The Rust gateway uses that form so it can attach ElevenLabs' `xi-api-key`; no provider key is exposed to the device.

For a manual handshake smoke test, use a WebSocket client that can set headers and a subprotocol, for example:

```sh
websocat \
  -H="Authorization: Bearer $DEVICE_AUTH_TOKEN" \
  -H="X-Device-Id: voice-pe-a1b2c3d4e5f6" \
  --protocol hermes-voice.realtime.v2 \
  wss://voice.example.com/v2/realtime
```

Send the `hello` object from [`protocol.md`](protocol.md); expect `ready` with a non-empty `conversation_id`. This checks routing/authentication and durable conversation state, but not audio framing.

## 6. Configure firmware

The supported factory/adoption flow and the clone-local/manual flow share the same zero-secret base configuration.

### Factory and ESPHome Device Builder adoption

Build and flash `firmware/hermes-voice-pe.factory.yaml`. It contains no Wi-Fi, API, OTA, gateway or provider secret. Provision Wi-Fi with center-button-authorized Improv BLE or Improv Serial, add the discovered ESPHome node to Home Assistant, then select **Adopt** in ESPHome Device Builder.

The imported configuration must compile and install before Hermes is configured. It exposes **Hermes Device ID** as a hardware-stable `voice-pe-<wifi-mac>` value. Add that ID/token pair to `DEVICE_TOKENS_JSON`, then add these substitutions to the adopter-owned YAML and install again over native OTA:

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
hermes_device_token: "the-token-for-this-device"
```

Use `firmware/hermes-voice-pe.local.yaml`. Its default empty `device_id` derives the same hardware-stable identifier exposed by the diagnostic entity. Put that identifier in `DEVICE_TOKENS_JSON`; never reuse a token or explicit ID between physical devices. An explicit `device_id` override is supported for migration, but ESPHome rename, friendly-name change and API/OTA credential rotation should not change it.

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

The current realtime firmware schema requires `wss://`. The gateway's buffered `/v1/voice` route remains available to diagnostic/native clients and to the earlier buffered firmware build, but realtime firmware never switches to it after an ambiguous turn. Moving a physical unit back to v1 is an explicit reconfiguration/reflash, not a retry policy.

Wi-Fi can be provisioned with authorized Improv BLE (direct or through an active HA Bluetooth proxy), Improv Serial, or compiled secrets. BLE is disabled five seconds after Wi-Fi connects and must be stopped before wake inference or the persistent Hermes socket starts; it is re-enabled after Wi-Fi loss for recovery. The gateway URL/device token remain adopter-owned compile-time ESPHome settings installed through ordinary OTA—there is deliberately no unauthenticated page or Home Assistant entity for changing credentials. Both may be empty for initial adoption; exactly one is invalid; both valid values enable realtime voice.

### Optional Home Assistant media plane

The supplied firmware exposes an encrypted [ESPHome Native API](https://esphome.io/components/api/) media-player entity and the **New Hermes Conversation** template button. Factory onboarding/Device Builder generates the unique API key automatically. For a clone-local build, generate it with `openssl rand -base64 32` and store it as `api_encryption_key`. The button asks the device to use its authenticated Worker WebSocket; Home Assistant does not receive ElevenLabs/Hermes credentials or conversation history.

The mixer has three independent inputs. Hermes PCM is resampled into its own input. Home Assistant URLs feed separate HTTP/FLAC `speaker_source` pipelines: 48 kHz mono for announcements and 48 kHz stereo for music. The Hermes lifecycle immediately ducks both HA inputs by 20 dB for listening/thinking/reply playback, then restores announcements quickly and music over approximately one second on idle. If an HA announcement is still active, its normal music duck remains in force rather than being incorrectly released. Disabling or losing Home Assistant therefore removes media/announcement playback only, not voice operation. See ESPHome's [`speaker_source` media-player contract](https://esphome.io/components/media_player/speaker_source/) for the media URL/transcode behavior.

## 7. Build and flash

Connect the Voice PE over USB:

```sh
# Universal zero-secret factory/adoption image
uvx --from esphome==2026.6.0 esphome config firmware/hermes-voice-pe.factory.yaml
uvx --from esphome==2026.6.0 esphome compile firmware/hermes-voice-pe.factory.yaml
uvx --from esphome==2026.6.0 esphome run firmware/hermes-voice-pe.factory.yaml

# Or a clone-local preconfigured operator image
uvx --from esphome==2026.6.0 esphome run firmware/hermes-voice-pe.local.yaml
```

The zero-secret first-flash image is generated under:

```text
firmware/.esphome/build/hermes-voice-pe-factory/.pioenvs/hermes-voice-pe/firmware.factory.bin
```

The factory wrapper deliberately uses a different build cache from the configured local wrapper. Do not collapse them: alternating an empty factory API key and a configured encryption key in one incremental ESPHome build directory can retain an incompatible object set.

Subsequent updates use password-protected native ESPHome OTA. Factory onboarding/Device Builder creates a unique Native API key but not an OTA password, so the adopter must set `ota_password` on the immediate enrollment install; the clone-local wrapper supplies both from secrets. The encrypted Native API lets the unit attach to Home Assistant as a media player, but there is deliberately no ESPHome `voice_assistant` component: voice remains attached to Hermes.

The firmware pins ESPHome 2026.6.0, Voice PE `voice_kit` commit `0579e7b9`, XMOS firmware 1.3.1 by MD5, and a versioned `okay_nabu` wake-word model.

## 8. First hardware test

Watch USB logs:

```sh
uvx --from esphome==2026.6.0 esphome logs firmware/hermes-voice-pe.local.yaml
```

Expected phases are wake-ready, listening, speech detected, thinking, reply playback, recoverable error, Wi-Fi offline, and hardware mute. Exact LED animation is cosmetic; state ordering and cancellation are not.

Test center-button start/cancel and the idle-only 2–5 second new-conversation hold before tuning wake-word sensitivity. Verify one complete realtime sequence in logs:

```text
WSS ready → turn.ready → input frames/acks → transcript.final
→ response.start → tts.start → output frames/acks → tts.end → turn.done
```

Do not infer success from `turn.done` alone; listen for clipping, early cutoff, echo re-trigger, underruns, and delayed tail audio. Run the physical acceptance suite in [`testing.md`](testing.md).

## Operations

- Rotate a device token by updating `DEVICE_TOKENS_JSON` and the adopter-owned ESPHome secret, then install that unit over OTA.
- Rotate the Native API encryption key in Device Builder (or clone-local `firmware/secrets.yaml`) and update/re-pair the Home Assistant ESPHome integration; it is unrelated to the Worker token and hardware-derived device ID.
- Rotate ElevenLabs, Hermes, and Access credentials only in Worker secrets.
- Change Hermes long-term-memory ownership only through `HERMES_SESSION_KEYS_JSON`; the Durable Object detects the new resolved key and rotates the short-term UUID/head before the next turn. It does not delete the older records from Hermes' response store.
- Treat changes to `HERMES_BASE_URL` or `HERMES_MODEL` the same way: the next accepted turn is fenced into a new conversation. Credential rotation at an otherwise unchanged origin does not itself rotate the chain.
- Inspect Durable Object storage by opaque device ID/turn ID, but do not log or persist transcripts/audio for routine observability.
- Reset conversation continuity with the Home Assistant **New Hermes Conversation** button, the 2–5 second center hold, or another authenticated ESPHome automation invoking `hermes_voice.new_conversation`. Do not manually edit Durable Object storage or delete only the Hermes response: the reset durably rotates the UUID, clears the head/session metadata, and persists its request ID alongside the independent turn journal. There is no unauthenticated reset route.
- Treat a `conversation_expired`/Hermes `404` as a visible boundary: the current utterance fails without replay, the Durable Object has already rotated, and the next newly spoken turn begins fresh.
- Monitor `response.failed`, missing terminal SSE events, provider close codes, queue overflow, reconnect rate, and latency histograms.
- Apply Cloudflare rate limiting to handshake and buffered routes. Device auth prevents authorized provider use; rate limiting bounds brute force and cost.
- Keep outbound Scribe/TTS sockets open only for active work so idle Durable Objects can hibernate.
