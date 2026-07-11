# HA Voice PE → Hermes Cloudflare gateway

This directory contains a Rust `workers-rs` Cloudflare Worker compiled to WebAssembly. Realtime v2 is the primary API and uses one SQLite-backed `VoiceSession` Durable Object per authenticated device. Buffered HTTPS v1 remains available for diagnostics, but its separate named Hermes chain is deprecated for conversational use.

## Realtime pipeline

`GET /v2/realtime` upgrades the authenticated device connection using WebSocket subprotocol `hermes-voice.realtime.v2`:

1. the edge handler validates the upgrade, subprotocol, device ID, and bearer before selecting `VOICE_SESSIONS.id_from_name(device_id)`;
2. the Durable Object serializes the device's turns and persists a local conversation UUID, Hermes origin/model/session-key binding, safe completed Hermes head, returned Hermes session ID, last activity, last reset request ID, and current journal;
3. 64 ms microphone PCM frames are accepted/acknowledged while capture continues and coalesced in adjacent pairs into approximately 128 ms ElevenLabs Scribe v2 Realtime messages;
4. a device `turn.commit` causes an explicit Scribe manual commit;
5. the committed transcript starts Hermes `POST /v1/responses` with `stream: true`, `store: true`, and the last completed `previous_response_id`;
6. append-only `response.output_text.delta` text is sanitized and segmented into phrases while structured tool records are ignored for speech;
7. phrases feed an ElevenLabs Flash v2.5 multi-context TTS WebSocket;
8. decoded `pcm_16000` audio is reframed and returned before Hermes or TTS finishes;
9. cumulative device ACKs enforce bounded input/output windows and cancellation drops late turn data.

Hermes `response.created` produces an in-flight ID only. The Durable Object promotes it to `last_completed_response_id` exclusively after `response.completed` with embedded status `completed`. The next committed utterance supplies that ID as `previous_response_id`, so Hermes reconstructs the complete transcript and structured tool history. Failure, disconnect, and cancellation before completion preserve the previous safe head and are never automatically retried. Once Hermes has completed, later TTS/playback failure does not roll its response or tool state back.

The Worker supplies a stable `X-Hermes-Session-Key` for long-term-memory scope independently of the short-term response chain. `HERMES_SESSION_KEYS_JSON` can map device IDs to operator-selected scopes; the fallback is `voice:<device-id>`. The selected Hermes agent/profile comes from the `HERMES_BASE_URL`/`HERMES_API_KEY` endpoint and credentials. `HERMES_MODEL` is only the advertised API model/route label.

An explicit `conversation.reset` is accepted only while no turn or reset is active. Its 16-hex request ID makes retry idempotent: repeating an acknowledged or ambiguously delivered reset returns the same `conversation.reset.done` and UUID instead of rotating twice. Reconnect, reboot, wake, cancellation, and barge-in preserve the UUID and safe head. Optional inactivity rotation is disabled by default and measures activity from accepted turn starts as well as later successful completion. A changed Hermes base URL, model/route label, or resolved session key rotates the UUID/head before the next turn, fencing a stored response from a different agent or memory scope.

If Hermes returns `404` for an evicted previous response, the gateway rotates to a new conversation UUID and fails the current turn without replaying it. The next newly spoken turn starts that fresh chain while retaining the stable long-term-memory key.

See [`../docs/protocol.md`](../docs/protocol.md) for the exact 20-byte binary frame and JSON controls, and [`../docs/architecture.md`](../docs/architecture.md) for state/cancellation semantics.

## APIs

### `GET /v2/realtime`

Required handshake fields:

```http
Authorization: Bearer <device token>
X-Device-Id: kitchen-voice-pe
Sec-WebSocket-Protocol: hermes-voice.realtime.v2
Upgrade: websocket
```

Success is `101 Switching Protocols` with the selected subprotocol. The first application message is `hello`; the gateway replies `ready` with PCM formats, 64 ms target frames, 32-frame windows, and the current conversation UUID. While idle, the device can send `conversation.reset`; the gateway durably rotates context and acknowledges the same request ID with `conversation.reset.done`.

Audio WebSocket messages are binary; control messages are JSON text. PCM is signed 16-bit little-endian mono at 16 kHz. Multi-byte values in the 20-byte framing header use network byte order.

### `POST /v1/voice`

The fallback accepts a complete RIFF/WAVE body containing PCM16LE mono/16 kHz audio:

```http
Authorization: Bearer <device token>
X-Device-Id: kitchen-voice-pe
Content-Type: audio/wav
```

It uses batch STT, the separate `voice-buffered-<device-id>` named Hermes chain, a complete non-streaming response, and ElevenLabs' streaming HTTP TTS endpoint. Success is a raw PCM16LE response. This route is not realtime, never reads or advances the Durable Object conversation, is never an automatic retry target, and is deprecated for interactive conversation.

### `GET /health`

Returns `200` only when required configuration is valid and reports `"realtime": true`. Invalid configuration returns `503` with a generic degraded response and no secret names/values.

## Durable Object binding

`wrangler.toml` declares:

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

The object is named by an already authenticated device ID. It uses the Durable Object Hibernation WebSocket API for the device connection. Outbound provider sockets are closed after active work because they prevent hibernation.

## Configuration

Non-secret bindings:

| Binding | Required | Default | Scope |
| --- | --- | --- | --- |
| `VOICE_SESSIONS` | v2 | — | `VoiceSession` Durable Object namespace. |
| `HERMES_BASE_URL` | yes | — | HTTPS Hermes API base. |
| `ELEVENLABS_VOICE_ID` | yes | — | Reply voice. |
| `ELEVENLABS_BASE_URL` | no | `https://api.elevenlabs.io/v1` | ElevenLabs HTTPS base; WSS URLs are derived. |
| `REALTIME_STT_MODEL` | no | `scribe_v2_realtime` | v2 Scribe model. |
| `TTS_MODEL` | no | `eleven_flash_v2_5` | Realtime and fallback TTS model. |
| `HERMES_MODEL` | no | `hermes-agent` | Cosmetic Hermes advertised route/model name; the origin/key selects the actual profile and server-side model. |
| `CONVERSATION_IDLE_SECONDS` | no | off | Optional new-conversation boundary after inactivity: `0`, `off`, or `none` disables it; otherwise 60–31,536,000 seconds. |
| `STT_LANGUAGE_CODE` | no | auto | Optional language hint. |
| `MAX_AUDIO_SECONDS` | no | `120` | Maximum v1/v2 turn duration, 1–600 seconds. |
| `STT_PROVIDER` | no | `elevenlabs` | Buffered v1 only: `elevenlabs` or `openai-compatible`. |
| `STT_MODEL` | no | `scribe_v2` / `whisper-1` | Buffered v1 STT model. |
| `STT_BASE_URL` | alternate v1 only | `https://api.openai.com/v1` | OpenAI-compatible buffered STT base. |
| `MAX_AUDIO_BYTES` | no | `4194304` | Buffered complete request limit; hard cap 16 MiB. |
| `MAX_HERMES_RESPONSE_BYTES` | no | `2097152` | Buffered Hermes JSON and v2 stored-response recovery limit; accepted 64 KiB–8 MiB. |

Secrets:

| Secret | Required | Purpose |
| --- | --- | --- |
| `ELEVENLABS_API_KEY` | yes | Scribe and TTS HTTP/WebSocket authentication. |
| `HERMES_API_KEY` | yes | Hermes bearer. |
| `HERMES_SESSION_KEYS_JSON` | no | Device-ID → stable Hermes long-term-memory scope map; unmapped devices use `voice:<device-id>`. |
| `DEVICE_AUTH_TOKEN` | one auth mode | Shared 16–512 byte device bearer. |
| `DEVICE_TOKENS_JSON` | preferred auth mode | Device-ID → independent token map; takes precedence. |
| `CF_ACCESS_CLIENT_ID` | optional pair | Access service-token ID protecting Hermes. |
| `CF_ACCESS_CLIENT_SECRET` | optional pair | Matching Access secret. |
| `STT_API_KEY` | alternate v1 only | OpenAI-compatible STT bearer. |

Both Access values must be present together. Provider/Hermes URLs must be HTTPS and credentialed redirects are rejected.

Example per-device secret:

```json
{"kitchen-voice-pe":"long-random-token-1","office-voice-pe":"long-random-token-2"}
```

Optional memory-scope secret:

```json
{"kitchen-voice-pe":"agent:main:voice:room:kitchen","office-voice-pe":"agent:main:voice:room:office"}
```

Only share a session-key value between devices when they are intentionally meant to share Hermes long-term memory. Explicit, idle, and stale-head conversation rotations clear transcript/tool-chain continuity without rotating this key; an operator mapping change creates a binding-fenced conversation using the new value.

## Build and test

Prerequisites are Rust 1.88+, `wasm32-unknown-unknown`, Node.js, `worker-build` 0.8.4, and Wrangler:

```sh
rustup target add wasm32-unknown-unknown
cargo install worker-build --version 0.8.4 --locked
npm install
```

Run:

```sh
cargo fmt --check
cargo test
cargo check
cargo clippy -- -D warnings
worker-build --release
npx wrangler deploy --dry-run
```

For local runtime testing:

```sh
cp .dev.vars.example .dev.vars
npm run dev
```

Deploy only after creating secrets:

```sh
wrangler secret put DEVICE_TOKENS_JSON
wrangler secret put ELEVENLABS_API_KEY
wrangler secret put HERMES_API_KEY
# Optional when overriding voice:<device-id> memory scopes:
wrangler secret put HERMES_SESSION_KEYS_JSON
wrangler deploy
```

## Safety and operational boundaries

- Provider STT partials are discarded rather than forwarded to the bounded device control queue; only the committed transcript reaches the device and invokes Hermes.
- Structured Hermes function calls/results are not spoken.
- Non-speech Hermes events are streamed-discarded without retaining or parsing their tool payloads; retained terminal/text events and the total SSE stream have separate bounds.
- Responses SSE terminates on `response.completed` or `response.failed`, not `[DONE]`.
- Device cancellation aborts transport and TTS context, but cannot undo a Hermes tool side effect.
- Conversation continuation uses only a completed `previous_response_id`; the named `conversation` alias is excluded from realtime because Hermes' streaming mapping can point at an in-progress or incomplete response.
- Explicit reset is idle-only and request-ID idempotent. Transport lifecycle events never silently reset conversation context.
- Responses streaming has no Runs approval-response channel; the gateway never auto-approves.
- Every queue/event/body is bounded; overflow cancels instead of buffering indefinitely.
- STT, Hermes, and TTS phases have absolute deadlines; the TTS context receives bounded 15-second keepalives during long Hermes tool work.
- Logs and errors exclude audio, transcripts, spoken text, tool payloads, credentials, and upstream response bodies.
- An output ACK returns credit only after PCM leaves the device receive ring for its fixed speaker-staging buffer; it is later than network acceptance but still does not prove the downstream speaker or DAC consumed it.
- Physical Voice PE and live-provider tests remain required for AEC/barge-in and latency claims; see [`../docs/testing.md`](../docs/testing.md).
