# HA Voice PE → Hermes Cloudflare gateway

This directory contains a Rust `workers-rs` Cloudflare Worker compiled to WebAssembly. Realtime v2 is the primary API and uses one SQLite-backed `VoiceSession` Durable Object per authenticated device. Buffered HTTPS v1 is hidden by default and, when explicitly enabled, is stateless and isolated from realtime conversation state.

## Realtime pipeline

`GET /v2/realtime` upgrades the authenticated device connection using WebSocket subprotocol `hermes-voice.realtime.v2`:

1. the edge handler validates the upgrade, subprotocol, device ID, and bearer before selecting `VOICE_SESSIONS.id_from_name(device_id)`;
2. the Durable Object serializes the device's turns and persists a local conversation UUID, a one-way fingerprint of the Hermes origin/model/session-key/profile binding, the safe completed Hermes head, returned Hermes session ID, last activity, last reset request ID, current journal, and a bounded 24-hour usage budget;
3. 64 ms microphone PCM frames are accepted/acknowledged while capture continues and coalesced in adjacent pairs into approximately 128 ms ElevenLabs Scribe v2 Realtime messages;
4. a device `turn.commit` causes an explicit Scribe manual commit;
5. the committed transcript starts Hermes `POST /v1/responses` with `stream: true`, `store: true`, and the last completed `previous_response_id`;
6. append-only `response.output_text.delta` text is sanitized and segmented into phrases while structured tool records are ignored for speech;
7. phrases feed an ElevenLabs Flash v2.5 multi-context TTS WebSocket;
8. decoded `pcm_16000` audio is reframed and returned before Hermes or TTS finishes, subject to a hard 30-second per-turn output ceiling enforced across provider-event boundaries;
9. cumulative device ACKs enforce bounded input/output windows and cancellation drops late turn data.

Hermes `response.created` produces an in-flight ID only. The Durable Object promotes it to `last_completed_response_id` exclusively after `response.completed` with embedded status `completed`. The next committed utterance supplies that ID as `previous_response_id`, so Hermes reconstructs the complete transcript and structured tool history. Failure, disconnect, and cancellation before completion preserve the previous safe head and are never automatically retried. Once Hermes has completed, later TTS/playback failure does not roll its response or tool state back.

The Worker supplies a stable `X-Hermes-Session-Key` for long-term-memory scope independently of the short-term response chain. Secure configuration requires `HERMES_SESSION_KEYS_JSON` to cover every device in `DEVICE_TOKENS_JSON`; an implicit `voice:<device-id>` scope is available only with `ALLOW_IMPLICIT_HERMES_CONTEXT=true`. The selected Hermes agent/profile comes from the `HERMES_BASE_URL`/`HERMES_API_KEY` endpoint and credentials. `HERMES_MODEL` is only the advertised API model/route label. `HERMES_PROFILE_ID` and `HERMES_BINDING_REVISION` fence profile-policy changes.

An explicit `conversation.reset` is accepted only while no turn or reset is active. Its 16-hex request ID makes retry idempotent: repeating an acknowledged or ambiguously delivered reset returns the same `conversation.reset.done` and UUID instead of rotating twice. Reconnect, reboot, wake, cancellation, and barge-in do not by themselves rotate context. The secure inactivity boundary defaults to 900 seconds and measures activity from accepted turn starts as well as later successful completion. Disabling it requires `ALLOW_UNBOUNDED_CONVERSATION=true`. A changed Hermes base URL, model/route label, profile ID, binding revision, or resolved session key rotates the UUID/head before the next turn.

Only Hermes' exact structured “previous response not found” error for the supplied ID is an expiry boundary: the gateway rotates the UUID and fails the current turn without replay. Streaming creation requires exact HTTP `200` with a valid `text/event-stream` media type before `response.start` can reach the device. Redirects, other success statuses, non-SSE bodies, and unrelated or generic `404` responses fail closed without destroying the safe head.

Each Hermes request carries a deterministic, conversation/turn-scoped `Idempotency-Key`. At audited Hermes commit [`5ecc079`](https://github.com/NousResearch/hermes-agent/tree/5ecc07986f46463ca3096679b03a46402eb19cee), the streaming Responses branch does not use the non-streaming idempotency cache. If a restart/disconnect leaves a response starting, missing, or incomplete, the Durable Object fails closed with `conversation_ambiguous` and blocks later audio turns until an explicit reset. A retrieved completed response is promoted; no command is automatically replayed.

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

### `POST /v1/voice` (disabled by default)

When enabled, the diagnostic accepts a complete RIFF/WAVE body containing PCM16LE mono/16 kHz audio:

```http
Authorization: Bearer <device token>
X-Device-Id: kitchen-voice-pe
Content-Type: audio/wav
```

Unless `DIAGNOSTIC_V1_ENABLED=true`, this route deliberately returns the same `404` as an unknown route. When enabled it uses batch STT, a non-streaming `store: false` Hermes request, a hashed diagnostic memory scope, and ElevenLabs' streaming HTTP TTS endpoint. Success is raw PCM16LE. It has no response chain, never reads or advances the Durable Object conversation, and is never an automatic retry target.

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

Non-secret bindings and policy flags:

| Binding | Required | Default | Scope |
| --- | --- | --- | --- |
| `VOICE_SESSIONS` | v2 | — | `VoiceSession` Durable Object namespace. |
| `HERMES_BASE_URL` | yes | — | HTTPS Hermes API base. |
| `ELEVENLABS_VOICE_ID` | yes | — | Reply voice. |
| `ELEVENLABS_BASE_URL` | no | `https://api.elevenlabs.io/v1` | ElevenLabs HTTPS base; WSS URLs are derived. |
| `REALTIME_STT_MODEL` | no | `scribe_v2_realtime` | v2 Scribe model. |
| `TTS_MODEL` | no | `eleven_flash_v2_5` | Realtime and diagnostic TTS model. |
| `HERMES_MODEL` | no | `hermes-agent` | Cosmetic Hermes advertised route/model name; the origin/key selects the actual profile and server-side model. |
| `HERMES_PROFILE_ID` | no | `default` (`voice-safe` in shipped Wrangler config) | Stable identifier for the selected server-side profile. |
| `HERMES_BINDING_REVISION` | no | `v1` | Operator-bumped fence for changes to SOUL, tools, sandbox, memory, or policy. |
| `HERMES_VOICE_INSTRUCTIONS` | no | built-in concise spoken-answer prompt | Plain-text instructions, preferably stored as a secret if customized. |
| `CONVERSATION_IDLE_SECONDS` | no | `900` | Inactivity boundary; otherwise 60–31,536,000. `0`/`off`/`none` also requires `ALLOW_UNBOUNDED_CONVERSATION=true`. |
| `ALLOW_UNBOUNDED_CONVERSATION` | no | `false` | Explicit acknowledgement required to disable the idle boundary. |
| `STT_LANGUAGE_CODE` | no | auto | Optional language hint. |
| `REALTIME_MAX_AUDIO_SECONDS` | no | `30` | Realtime maximum and hard cap, 1–30 seconds. |
| `REALTIME_MAX_OUTPUT_SECONDS` | no | `30` | Realtime playback maximum and hard cap, 1–30 seconds; decoded provider PCM is counted before reframing or playback. |
| `MAX_CONNECTION_AUDIO_SECONDS` | no | `900` | Cumulative captured audio cap applied both per socket and per device's durable 24-hour accounting window; maximum 86,400. |
| `MAX_TURNS_PER_CONNECTION` | no | `256` | Turn-attempt cap applied both per socket and per device's durable 24-hour accounting window; maximum 10,000. |
| `MAX_MESSAGES_PER_CONNECTION` | no | `16384` | Application-message cap applied both per socket and per device's durable 24-hour accounting window; maximum 1,000,000. |
| `DIAGNOSTIC_V1_ENABLED` | no | `false` | Expose stateless `POST /v1/voice`; otherwise it is indistinguishable from a missing route. |
| `ELEVENLABS_ENABLE_LOGGING` | no | `true` | Explicitly passed to supported ElevenLabs STT/TTS endpoints. |
| `STT_PROVIDER` | no | `elevenlabs` | Buffered v1 only: `elevenlabs` or `openai-compatible`. |
| `STT_MODEL` | no | `scribe_v2` / `whisper-1` | Buffered v1 STT model. |
| `STT_BASE_URL` | alternate v1 only | `https://api.openai.com/v1` | OpenAI-compatible buffered STT base. |
| `MAX_AUDIO_BYTES` | no | `4194304` | Buffered complete request limit; hard cap 16 MiB. |
| `MAX_AUDIO_SECONDS` | no | `120` (`30` in shipped Wrangler config) | Buffered v1 WAV limit only; accepted 1–600 seconds. It never expands realtime's 30-second cap. |
| `MAX_HERMES_RESPONSE_BYTES` | no | `2097152` | Buffered Hermes JSON and v2 stored-response recovery limit; accepted 64 KiB–8 MiB. |
| `ALLOW_SHARED_DEVICE_TOKEN` | no | `false` | Enables the shared-token compatibility mode. |
| `ALLOW_IMPLICIT_HERMES_CONTEXT` | no | `false` | Permits an unmapped device to use `voice:<device-id>`. |

Secrets:

| Secret | Required | Purpose |
| --- | --- | --- |
| `ELEVENLABS_API_KEY` | yes | Scribe and TTS HTTP/WebSocket authentication. |
| `HERMES_API_KEY` | yes | Hermes bearer. |
| `DEVICE_TOKENS_JSON` | yes by default | Device-ID → unique independent token map. Tokens are 32–512 visible ASCII bytes (`!`–`~`). |
| `HERMES_SESSION_KEYS_JSON` | yes by default | Device-ID → stable long-term-memory scope map; must cover every configured device. |
| `DEVICE_AUTH_TOKEN` | compatibility only | Shared 32–512 visible-ASCII-byte bearer; accepted only with `ALLOW_SHARED_DEVICE_TOKEN=true`. |
| `CF_ACCESS_CLIENT_ID` | optional pair | Access service-token ID; may be a secret or variable. |
| `CF_ACCESS_CLIENT_SECRET` | optional pair | Matching Access secret. |
| `HERMES_VOICE_INSTRUCTIONS` | optional | Spoken-answer instructions; deploy as a secret if customized content is sensitive. |
| `STT_API_KEY` | alternate v1 only | OpenAI-compatible STT bearer. |

Both Access values must be present together. Deploy `CF_ACCESS_CLIENT_SECRET` with `wrangler secret put`, never under `[vars]`: Workers exposes both forms as the same runtime string binding, so this distinction is enforced by deployment review/CI rather than application code. Provider/Hermes URLs must be HTTPS. Credentialed requests use workerd-supported manual redirect handling and explicitly accept only the expected WebSocket/success/recovery statuses, so every `3xx` is rejected without following `Location` or forwarding a bearer to another origin.

An exact durable message, turn, or input-audio limit is exhausted: a new transport upgrade is rejected with `429` before WebSocket acceptance until its 24-hour window rolls over. `conversation.reset` and `ping` flush their message charge before any reset write or `pong`, so reconnecting cannot create an unmetered control/storage path.

Example per-device secret:

```json
{"kitchen-voice-pe":"replace-with-64-random-hex-characters-for-kitchen-device","office-voice-pe":"replace-with-an-independent-64-random-hex-token-for-office"}
```

Required matching memory-scope secret:

```json
{"kitchen-voice-pe":"agent:main:voice:room:kitchen","office-voice-pe":"agent:main:voice:room:office"}
```

Only share a session-key value between devices when they are intentionally meant to share Hermes long-term memory. Explicit, idle, and confirmed-expiry conversation rotations clear transcript/tool-chain continuity without rotating this key; a mapping/profile/binding change creates a fenced conversation using the new configuration.

## Build and test

Toolchain versions are pinned by `../rust-toolchain.toml`, `../.node-version`, `Cargo.lock`, and `package-lock.json`. Install the WASM target and locked Worker builder, then use `npm ci`:

```sh
rustup target add wasm32-unknown-unknown
cargo install worker-build --version 0.8.4 --locked
npm ci
```

Run:

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

`npm run build` is the native development build. Native Rust/LLVM,
wasm-bindgen, Binaryen and esbuild output varies by host OS and architecture.
Build the byte-reviewed App artifact from the repository root, then verify or
refresh its vendored copy, with:

```sh
scripts/build-gateway-worker-canonical.sh
scripts/sync-addon-worker-artifacts.sh --check
# Maintainers only, after reviewing an intentional source change:
scripts/sync-addon-worker-artifacts.sh --update
```

The canonical artifact builder has no Node/npm dependency. It fixes
Linux/amd64, the Rust base-image digest, Rust 1.96.0, worker-build 0.8.4 with
its locked dependency graph, and the exact wasm-bindgen 0.2.126, Binaryen 130,
and esbuild 0.28.0 native archives. Each native archive is verified against a
reviewed upstream digest before execution. The builder also uses a
deny-by-default Docker context so local credentials and unrelated files never
reach it. Node.js remains pinned for Wrangler and runtime tests outside that
artifact-only build.

The recorded 2026-07-12 result is 86 passing Rust tests and 14 passing tests against the optimized, compiled Worker in workerd. The runtime suite includes a complete two-turn mocked Scribe WebSocket → fragmented Hermes SSE → multi-context ElevenLabs TTS WebSocket exchange; odd provider PCM event-boundary reframing and byte-exact device audio; `previous_response_id` continuity; transcript non-disclosure to the device; TTS-before-Hermes-terminal streaming; fail-closed redirects and Hermes status/media-type validation; Durable Object eviction; reset idempotency; socket replacement; exact durable-quota enforcement before reset/ping side effects; and the aggregate decoded-output ceiling. This synthetic coverage does not replace live-provider or physical Voice PE testing.

For local runtime testing:

```sh
cp .dev.vars.example .dev.vars
npm run dev
```

Deploy only after creating secrets:

```sh
wrangler secret put DEVICE_TOKENS_JSON
wrangler secret put HERMES_SESSION_KEYS_JSON
wrangler secret put ELEVENLABS_API_KEY
wrangler secret put HERMES_API_KEY
wrangler deploy
```

## Safety and operational boundaries

- Provider STT partials and the committed transcript are not forwarded to the headless device; only the committed transcript invokes Hermes.
- Structured Hermes function calls/results are not spoken.
- Non-speech Hermes events are streamed-discarded without retaining or parsing their tool payloads; retained terminal/text events and the total SSE stream have separate bounds.
- Responses SSE terminates on `response.completed` or `response.failed`, not `[DONE]`.
- Device cancellation aborts transport and TTS context, but cannot undo a Hermes tool side effect.
- Conversation continuation uses only a completed `previous_response_id`; the named `conversation` alias is excluded from realtime because Hermes' streaming mapping can point at an in-progress or incomplete response.
- Ambiguous post-Hermes interruption blocks subsequent turns until explicit reset; the deterministic idempotency key is not treated as replay authority while the audited Hermes streaming branch ignores it.
- Explicit reset is idle-only and request-ID idempotent. Transport lifecycle events never silently reset conversation context.
- Responses streaming has no Runs approval-response channel; the gateway never auto-approves.
- Every queue/event/body is bounded; overflow cancels instead of buffering indefinitely.
- Durable conversation state stores a SHA-256 binding digest, not raw Hermes origin/model/profile/revision/session-scope values. That digest is minimization rather than encryption and remains sensitive operational metadata when its inputs are predictable.
- Established hibernating sockets revalidate their credential fingerprint on callbacks. Message, turn, and cumulative-audio quotas apply both to each socket and to a per-device durable 24-hour accounting window, so reconnecting does not reset the abuse budget.
- STT, Hermes, and TTS phases have absolute deadlines; the TTS context receives bounded 15-second keepalives during long Hermes tool work.
- Logs and errors exclude audio, transcripts, spoken text, tool payloads, credentials, and upstream response bodies.
- Speech cleanup is formatting hygiene, not a DLP system: an ordinary Hermes text reply can still repeat a secret or other PII. Production must use a restricted voice-safe Hermes profile/tool policy and test representative sensitive-result cases.
- An output ACK returns credit only after PCM leaves the device receive ring for its fixed speaker-staging buffer; it is later than network acceptance but still does not prove the downstream speaker or DAC consumed it.
- Physical Voice PE and live-provider tests remain required for AEC/barge-in and latency claims; see [`../docs/testing.md`](../docs/testing.md).
