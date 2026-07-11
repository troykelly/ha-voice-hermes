# Feasibility and research findings

Research was refreshed on 11 July 2026 against:

- ESPHome Voice PE firmware [`0579e7b9`](https://github.com/esphome/home-assistant-voice-pe/tree/0579e7b9d8504264719c593474c85447253c9dc1).
- ESPHome 2026.6.0 [`e2157a3d`](https://github.com/esphome/esphome/tree/e2157a3d26a8959c7c7ff212ab40afdd7b9f9d13).
- Hermes Agent [`4aa499ff`](https://github.com/NousResearch/hermes-agent/tree/4aa499ff9f3fcc0c38ce61da46805a4dcc8f612e), whose package version is [0.18.2](https://github.com/NousResearch/hermes-agent/blob/4aa499ff9f3fcc0c38ce61da46805a4dcc8f612e/pyproject.toml#L10).
- Current ElevenLabs Realtime STT and multi-context TTS API references.
- Current Cloudflare Workers, WebSocket, and Durable Object documentation.

## Verdict

The project is viable. It requires a replacement ESPHome component and a voice gateway; changing a backend URL in the stock firmware is insufficient.

The realtime design is now preferable to the buffered prototype because every required external interface is available:

- Voice PE audio and controls are reusable independently of Home Assistant.
- Cloudflare Workers can terminate the device WebSocket and create outbound authenticated WebSockets.
- Durable Objects provide one ordered coordinator per device and can hibernate while only the device socket is idle.
- ElevenLabs Scribe v2 Realtime accepts PCM as it is captured and returns partial and committed transcripts.
- Hermes 0.18.2 provides streaming Responses SSE, structured tool items, stored response chains, a returned transcript-session ID, and stable long-term-memory session-key scoping.
- ElevenLabs multi-context TTS accepts incremental text, returns incremental PCM, and gives each response a cancellable context ID.

The old buffered HTTPS/WAV transport remains useful for diagnostics, but it is no longer the primary architecture and its separate named chain is deprecated for conversational use.

## Voice PE facts that drive the design

The official configuration runs an ESP32-S3 at 240 MHz with 16 MB flash and octal PSRAM. The audio front end is an XMOS XU316 managed by the Voice PE-specific `voice_kit` component. The authoritative starting points are the upstream [`home-assistant-voice.yaml`](https://github.com/esphome/home-assistant-voice-pe/blob/0579e7b9d8504264719c593474c85447253c9dc1/home-assistant-voice.yaml), the [Voice PE product page](https://www.home-assistant.io/voice-pe/), and the [hardware datasheet](https://voice-pe.home-assistant.io/resources/home_assistant_voice_preview_edition_datasheet_v1_1.pdf).

The microphone bus presents stereo signed 32-bit samples at 16 kHz:

- channel 0: acoustic echo cancellation, interference cancellation, noise suppression, and AGC;
- channel 1: acoustic echo cancellation, interference cancellation, and noise suppression without final AGC.

ESPHome's [`MicrophoneSource`](https://github.com/esphome/esphome/blob/e2157a3d26a8959c7c7ff212ab40afdd7b9f9d13/esphome/components/microphone/microphone_source.cpp) selects a channel and converts it to signed 16-bit PCM. The integration uses channel 0 for STT and channel 1 with gain 4 for wake-word inference.

Output is 48 kHz stereo, 32-bit I²S through the AIC3204. Gateway audio is 16 kHz mono PCM; ESPHome's resampler and mixer feed the existing hardware path. This also preserves the XMOS playback reference needed for acoustic echo cancellation. Reliable barge-in still needs physical testing because transport cancellation cannot compensate for a poorly tuned acoustic path.

ESPHome 2026.6 also provides the [`speaker_source` media player](https://esphome.io/components/media_player/speaker_source/), which explicitly supports independent media and announcement pipelines feeding distinct mixer inputs. The implementation uses three inputs: Hermes PCM, an HTTP/FLAC Home Assistant announcement pipeline, and an HTTP/FLAC Home Assistant music pipeline. Native API is therefore useful as an optional media/control plane without reintroducing Home Assistant into the voice conversation.

The physical mute switch cuts microphone power. Firmware also stops capture and marks the microphone muted so state and privacy remain explicit.

### ESPHome factory adoption findings

The official Voice PE onboarding path is standard [Improv over BLE](https://esphome.io/components/esp32_improv/), not a special Voice or Bluetooth-proxy protocol. Home Assistant may reach its GATT service through a local adapter, a phone, or an existing ESPHome proxy with active connections; a passive proxy can advertise discovery but cannot provision it. Voice PE itself should not run the always-on [`bluetooth_proxy`](https://esphome.io/components/bluetooth_proxy/) workload.

The official [factory configuration](https://github.com/esphome/home-assistant-voice-pe/blob/0579e7b9d8504264719c593474c85447253c9dc1/home-assistant-voice.factory.yaml#L1-L57) uses the center button as Improv authorizer, waits five seconds after Wi-Fi association, disables BLE before voice work, and re-enables it on disconnect. This is a resource policy rather than a hardware impossibility: ESPHome explicitly warns that BLE is expensive beside audio. The Hermes build retains that lifecycle plus Improv Serial/USB recovery. It deliberately omits a fallback AP/captive portal because that factory surface would enable unauthenticated browser OTA.

Provisioning, Home Assistant addition and ESPHome adoption are distinct operations. Improv writes Wi-Fi; Native API/mDNS starts the ESPHome config flow; `dashboard_import` plus `esphome.project` lets Device Builder create adopter-owned YAML with a generated API key. ESPHome's [Dashboard Import implementation](https://github.com/esphome/esphome/blob/e2157a3d26a8959c7c7ff212ab40afdd7b9f9d13/esphome/components/dashboard_import/__init__.py) requires a public package URL. It does not provision arbitrary gateway or provider secrets.

Therefore the public package must compile with Hermes unconfigured, and the adopter adds the Worker URL, per-device token, and random OTA password in a later normal OTA. The custom component now treats empty/empty as an inactive state without allocating its transport/audio buffers; a hardware-derived device ID remains stable across ESPHome renames. Full details are in [`esphome-adoption.md`](esphome-adoption.md).

## Why stock ESPHome cannot connect to Hermes

ESPHome's [`voice_assistant`](https://github.com/esphome/esphome/blob/e2157a3d26a8959c7c7ff212ab40afdd7b9f9d13/esphome/components/voice_assistant/voice_assistant.h) depends on the ESPHome Native API and holds an `api::APIConnection`. A Home Assistant client subscribes to the device, requests a run, receives audio, then returns lifecycle events and a TTS URL. There is no generic outbound WebSocket mode or configurable agent URL.

The replacement reverses voice ownership: the Voice PE initiates authenticated TLS on port 443. A Cloudflare Worker cannot turn the stock private-LAN device into a public Native API server; Workers' [TCP sockets API](https://developers.cloudflare.com/workers/runtime-apis/tcp-sockets/) is outbound and does not make RFC1918 devices reachable through household NAT. The implemented Native API remains a normal optional LAN connection used only for Home Assistant media/control, not as a bridge to Hermes.

## Hermes interface

Hermes is the tool-capable agent runtime, not the audio service. Its [API server documentation](https://hermes-agent.nousresearch.com/docs/user-guide/features/api-server/) defines `POST /v1/responses`, streaming SSE, `previous_response_id`, Runs, approvals, cancellation, model discovery, `X-Hermes-Session-Id`, and `X-Hermes-Session-Key`. The API defaults to `127.0.0.1:8642` and requires `API_SERVER_KEY` bearer authentication.

Hermes 0.18.2 advertises `responses_streaming: true`, but still advertises `audio_api: false` and `realtime_voice: false` in its [capabilities implementation](https://github.com/NousResearch/hermes-agent/blob/4aa499ff9f3fcc0c38ce61da46805a4dcc8f612e/gateway/platforms/api_server.py#L1450-L1535). The gateway must therefore supply STT and TTS.

The selected request is:

```json
{
  "model": "hermes-agent",
  "input": "committed transcript",
  "instructions": "Give a concise natural spoken answer...",
  "store": true,
  "stream": true,
  "previous_response_id": "resp_previous_completed_turn"
}
```

`previous_response_id` is omitted for the first turn. This is a real stateful Hermes conversation: the official [Responses documentation](https://hermes-agent.nousresearch.com/docs/user-guide/features/api-server/#post-v1responses) says the stored chain reconstructs the complete conversation, including function calls and their results, and reuses one Hermes session across chained turns. The response's `X-Hermes-Session-Id` is persisted as metadata with the safe head.

The gateway also sends an operator-owned stable long-term-memory scope. `HERMES_SESSION_KEYS_JSON` maps device IDs to values such as `agent:main:voice:room:kitchen`; an unmapped device uses `X-Hermes-Session-Key: voice:<device-id>`. Hermes' [long-term-memory scoping documentation](https://hermes-agent.nousresearch.com/docs/user-guide/features/api-server/#long-term-memory-scoping-x-hermes-session-key) explicitly defines this header as independent of the transcript-scoped `X-Hermes-Session-Id`, which rotates on `/new`. An explicit, idle, or stale-head Voice PE boundary therefore drops `previous_response_id` and rotates its local UUID while retaining the memory key; changing the key itself creates a separately fenced boundary.

Hermes profile selection is also server-side. `HERMES_BASE_URL` and its bearer identify the intended profile/API process; Hermes documents under [API limitations](https://hermes-agent.nousresearch.com/docs/user-guide/features/api-server/#limitations) that the Responses `model` field is cosmetic and the actual model is configured on the server. The integration cannot select a different trusted agent merely by changing the request label.

Hermes stores an `in_progress` response as soon as `response.created` is emitted and persists an `incomplete` response when an SSE client disconnects. That behavior is explicit in the pinned 0.18.2 [Responses streaming implementation](https://github.com/NousResearch/hermes-agent/blob/4aa499ff9f3fcc0c38ce61da46805a4dcc8f612e/gateway/platforms/api_server.py#L2611-L3195) and remains visible in the later official implementation at [`7acaff5`](https://github.com/NousResearch/hermes-agent/blob/7acaff5ef2bcbaa22bd23b72efe60906123a4f55/gateway/platforms/api_server.py#L2601-L2772). A named-conversation mapping advances when that initial streaming snapshot is persisted, before completion. Consequently, realtime v2 does not use Hermes' otherwise convenient named `conversation` alias. It owns an explicit pointer: a newly observed response ID is journaled only as an in-flight reconciliation candidate, and the gateway promotes it as `previous_response_id` **only** after receiving `response.completed` whose embedded response has `status: "completed"`. Turns that fail, are interrupted, or disconnect before that terminal event retain the last known completed ID.

This prevents a later voice turn from inheriting a partial assistant answer. It does not undo tool side effects from the abandoned turn, so ambiguous Hermes turns are never automatically replayed.

### Conversation-boundary findings

Hermes' own lifecycle distinguishes a transcript conversation from stable long-term-memory scope. The safest voice policy follows that distinction:

- a **reply** is the next committed transcript chained from the last completed response in the same local conversation UUID;
- an explicit **new conversation** is analogous to Hermes `/new`: rotate transcript context but retain the stable session key;
- device reconnect, restart, wake, cancellation, and barge-in are recovery/turn events and do not imply the user changed conversations;
- an optional inactivity boundary can be useful for a shared appliance, but there is no universal Hermes timeout, so it is disabled by default and operator-configurable; accepted failed/cancelled turns still count as user activity;
- a missing previous response is a hard boundary because Hermes' response store is an LRU of 100 records; the failed utterance must not be replayed after rotating context;
- changing the Hermes origin, advertised model/route, or resolved memory session key is another hard boundary; the old response ID must never be sent into a different agent or memory scope.

The implementation turns these findings into durable state: a conversation UUID, Hermes base/model/session-key binding, completed response head, returned Hermes session ID, last accepted-turn/completion activity time, last reset request ID, and a current-turn journal. `conversation.reset`/`conversation.reset.done` makes explicit reset request-ID idempotent across ambiguous WebSocket delivery. The local HA button and a 2–5 second center-button hold invoke the same action only while idle.

Hermes tools execute on the API-server host. Keep the API on loopback and publish it through an outbound [Cloudflare Tunnel](https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/), protected by an [Access service token](https://developers.cloudflare.com/cloudflare-one/access-controls/service-credentials/service-tokens/). Spoken input authenticates a device, not a human.

### Proactive-delivery findings

Current upstream Hermes documents an authenticated [Jobs API](https://hermes-agent.nousresearch.com/docs/user-guide/features/api-server/#jobs-api-background-scheduled-work), [Cron delivery](https://hermes-agent.nousresearch.com/docs/user-guide/features/cron/), [webhook-driven automation](https://hermes-agent.nousresearch.com/docs/user-guide/messaging/webhooks/), and extensible platform delivery. Those are viable trigger sources for a future Voice PE delivery adapter. They are not, by themselves, a callback into this Worker's idle Responses chain; the pinned API adapter has [no persistent outbound channel](https://github.com/NousResearch/hermes-agent/blob/4aa499ff9f3fcc0c38ce61da46805a4dcc8f612e/gateway/platforms/api_server.py#L835-L843).

Cron agent jobs start in fresh isolated sessions. Continuable delivery can mirror a result into an origin chat session on supported platforms, but fan-out/broadcast targets are not continuable. Therefore a cron final message may be used directly as a context-free announcement, but it must not be labelled a reply in a Voice PE conversation. For a true proactive conversation, the target conversation owner must run and complete a Hermes Responses turn from its selected safe head, then use that proactive response ID as the parent of the user's reply.

The recommended Hermes integration is a `voice_pe` [platform-adapter plugin](https://hermes-agent.nousresearch.com/docs/developer-guide/adding-platform-adapters) backed by authenticated, idempotent HTTPS to the Worker. Cron/Jobs, webhook delivery, and the operator-facing `hermes send` transport can share that adapter. Hermes intentionally [does not register its generic send engine as a default model-callable tool](https://github.com/NousResearch/hermes-agent/blob/4aa499ff9f3fcc0c38ce61da46805a4dcc8f612e/tools/send_message_tool.py#L1952-L1965): autonomous scheduled outreach uses `cronjob`, while arbitrary immediate agent outreach would need a dedicated, separately permissioned plugin tool. The future Worker/device architecture, group first-responder semantics, security constraints, and release tests are documented in [`proactive-delivery.md`](proactive-delivery.md). This feature is not implemented by realtime v2. The relevant source is identical between pinned `4aa499f` and current `7acaff5`, but both identify as package version 0.18.2; implementation should pin and re-audit an exact commit or image digest rather than a semantic-version floor.

### Multiple wake words and Hermes profiles

Pinned ESPHome 2026.6.0 supports multiple `micro_wake_word` models, but only the first is enabled on a fresh device and enabled models infer sequentially. All manifests must share one feature step size; each adds flash and its own runtime arenas. The public detection automation returns a manifest phrase rather than the YAML model ID, and multiple models may queue detections for one utterance. Therefore agent routing needs explicit boot enablement, a compiled model-ID-to-route table, resource/false-accept qualification and fail-closed co-detection arbitration.

Hermes request `model` does not normally select another agent. Optional `model_routes` changes inference provider/model within the same profile and silently defaults unknown aliases; it does not change SOUL, tools, profile-wide memory, response database or API authority. A genuine Avery/Jordan split uses distinct [Hermes profiles](https://hermes-agent.nousresearch.com/docs/user-guide/profiles/) and profile API processes/ports. Each device/canonical-conversation-lane pair then receives an independent completed response head and default memory key. The full device, Worker, profile, protocol, security and acceptance design is in [`multi-agent-wake-routing.md`](multi-agent-wake-routing.md).

## ElevenLabs realtime interfaces

### Speech to text

The [Realtime STT API](https://elevenlabs.io/docs/api-reference/speech-to-text/v-1-speech-to-text-realtime) is:

```text
wss://api.elevenlabs.io/v1/speech-to-text/realtime
  ?model_id=scribe_v2_realtime
  &audio_format=pcm_16000
  &commit_strategy=manual
```

The gateway authenticates with `xi-api-key`. Each message carries base64 mono PCM16LE plus its 16,000 Hz sample rate. ElevenLabs recommends 0.1–1 second chunks and 16 kHz mono. Device frames remain 64 ms for responsive flow control; the Durable Object coalesces two adjacent frames into an approximately 128 ms Scribe message. On commit, any remaining single frame is carried atomically in the final `input_audio_chunk` with `commit: true`; if no audio remains pending, that commit message has an empty `audio_base_64`. This follows the provider's published range without delaying device ACK/backpressure by 128 ms.

Exact audio and commit messages are:

```json
{
  "message_type": "input_audio_chunk",
  "audio_base_64": "<base64 PCM16LE>",
  "commit": false,
  "sample_rate": 16000
}
```

```json
{
  "message_type": "input_audio_chunk",
  "audio_base_64": "",
  "commit": true,
  "sample_rate": 16000
}
```

The relevant incoming types are `session_started`, `partial_transcript`, `committed_transcript`, and the documented error types. When timestamps are enabled, `committed_transcript_with_timestamps` follows the ordinary committed event and must not trigger a duplicate Hermes turn. Realtime v2 leaves timestamps disabled.

`partial_transcript` is mutable and is never agent input. The device protocol reserves optional partial telemetry, but the shipped gateway deliberately discards provider partials so they cannot pressure the device's bounded control queue. `committed_transcript` is the sole transcript forwarded to the device and the sole trigger for Hermes. See ElevenLabs' [commit strategy and audio format guidance](https://elevenlabs.io/docs/eleven-api/guides/how-to/speech-to-text/realtime/transcripts-and-commit-strategies) and [event/error reference](https://elevenlabs.io/docs/eleven-api/guides/how-to/speech-to-text/realtime/event-reference).

For a live microphone ElevenLabs also supports remote VAD. This integration uses the Voice PE's explicit end-of-turn signal with manual commit so stopping local capture cannot deprive remote VAD of the trailing silence it needs. The endpoint detector must still be tuned on hardware.

### Text to speech

The [multi-context TTS API](https://elevenlabs.io/docs/api-reference/text-to-speech/v-1-text-to-speech-voice-id-multi-stream-input) is:

```text
wss://api.elevenlabs.io/v1/text-to-speech/{voice_id}/multi-stream-input
  ?model_id=eleven_flash_v2_5
  &output_format=pcm_16000
  &auto_mode=true
  &sync_alignment=false
```

One TTS connection is used for each active voice turn and each Hermes answer gets a unique context ID. Holding a socket across several turns could save a handshake, but would keep the Durable Object active while the user is idle. The gateway instead opens it in parallel with Hermes after STT commit. It turns append-only Hermes text deltas into complete, speakable phrases rather than sending fragments of Markdown or individual tokens. At the end of the response it flushes the context. Base64 PCM is decoded and forwarded immediately as binary downlink frames.

Context initialization, incremental text, final flush, and cancellation are:

```json
{"text":" ","context_id":"0000000000000042","voice_settings":{"stability":0.5,"similarity_boost":0.8,"speed":1.0}}
```

```json
{"text":"Here is the first complete phrase. ","context_id":"0000000000000042"}
```

```json
{"context_id":"0000000000000042","flush":true}
```

```json
{"context_id":"0000000000000042","close_context":true}
```

Audio arrives as JSON containing base64 `audio`, camel-case `contextId`, and a final indicator documented as `is_final`; parsers accept `isFinal` as well because ElevenLabs' examples/generated types have used both. Alignment objects are ignored. `output_format=pcm_16000` is explicit because the endpoint otherwise defaults to an MP3 format.

On barge-in, `{"context_id":"...","close_context":true}` is the upstream cancellation primitive. The gateway also invalidates that turn and discards late audio. The device must clear its own queued PCM because cancelling a provider context cannot retract audio already delivered. ElevenLabs recommends one socket per end-user session, complete-sentence flushing, prompt context cleanup, and no more than five concurrent contexts in its [multi-context guide](https://elevenlabs.io/docs/eleven-api/guides/how-to/websockets/multi-context-web-socket).

ElevenLabs reports roughly 150 ms model latency for Scribe v2 Realtime and roughly 75 ms inference for Flash v2.5, excluding network and application time. Those are vendor figures, not project measurements; see the [models reference](https://elevenlabs.io/docs/overview/models) and [latency guide](https://elevenlabs.io/docs/eleven-api/guides/how-to/best-practices/latency-optimization).

## Cloudflare and WASM feasibility

Rust is compiled to `wasm32-unknown-unknown` and uses Cloudflare runtime networking through `workers-rs`; it is not a WASI server process. The platform supports both sides of this topology: an incoming device WebSocket and outbound ElevenLabs WebSockets. For an outbound socket with `xi-api-key`, the Worker uses an HTTPS `fetch` with `Upgrade: websocket` and the auth header, then accepts the returned socket. This is the header-capable form documented in [Workers WebSockets](https://developers.cloudflare.com/workers/examples/websockets/).

A per-device Durable Object serializes turns, carries the conversation/binding metadata and last completed Hermes response ID, owns bounded queues, and provides a single destination for reconnects. The incoming device WebSocket can use the [Durable Object Hibernation API](https://developers.cloudflare.com/durable-objects/best-practices/websockets/) while idle. Active outbound sockets prevent hibernation and accrue duration, so Scribe and TTS sockets are closed after the active turn; see the [Durable Object lifecycle](https://developers.cloudflare.com/durable-objects/concepts/durable-object-lifecycle/).

Whisper, Hermes, FFmpeg, and sample-rate conversion do not run inside the Worker. The Worker validates frames, coordinates state, adapts WebSocket/SSE payloads, performs bounded base64 conversion, and enforces cancellation. Audio processing remains on the Voice PE or at the provider.

## Options considered

| Option | Assessment | Decision |
| --- | --- | --- |
| Stock Voice PE voice path plus Native API bridge | Requires an always-on LAN process and preserves the Home Assistant voice protocol. | Rejected for voice; Native API is retained only as an optional local media/control plane. |
| Voice PE directly to Hermes | Hermes has no remote audio or realtime voice API. | Not viable without a voice layer. |
| ElevenLabs Agents with Hermes as Custom LLM | Managed VAD/STT/TTS can be very fast, but adds another agent-control plane and complicates Hermes-owned tools, approvals, and conversation identity. | Viable alternative, not selected. |
| Device WSS → Durable Object → Scribe Realtime → Hermes SSE → multi-context TTS | Owns the protocol, keeps secrets off-device, permits bounded flow control and cancellation, and streams every expensive stage. | **Primary realtime v2 implementation.** |
| Buffered HTTPS/WAV → batch STT → complete Hermes → streaming HTTP TTS | Simple, but adds whole-utterance and whole-agent-response waits. Its independent `voice-buffered-<device-id>` named chain is not semantically continuous with realtime. | Retained for diagnostics; deprecated for conversational use. |
| Hermes experimental relay contract | Not a stable remote audio transport. | Deferred. |

## Remaining uncertainty

There is no known API or platform blocker. The remaining release risk is physical behavior: XMOS tap selection, endpoint tuning, AEC during simultaneous playback and capture, TLS/WebSocket memory pressure, queue bounds under weak Wi-Fi, mono-to-stereo output, playback drain, and actual latency from Australia. Those are explicit acceptance tests rather than assumptions.
