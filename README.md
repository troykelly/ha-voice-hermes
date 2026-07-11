# Home Assistant Voice PE for Hermes

This repository replaces the Home Assistant-dependent voice transport on the **Home Assistant Voice: Preview Edition** with a direct, authenticated connection to a Hermes agent.

The primary transport is realtime v2:

```text
Voice PE ── persistent WSS /v2/realtime ── Cloudflare Worker + per-device Durable Object
   │                                              ├─ ElevenLabs Scribe v2 Realtime WSS
   │                                              ├─ Hermes /v1/responses SSE
   │                                              └─ ElevenLabs Flash v2.5 multi-context TTS WSS
   └──────────── streamed PCM16 playback ◀─────────────────────────────────────────────
```

Microphone PCM is uploaded while the user is speaking, Hermes text is consumed as SSE deltas, and ElevenLabs audio is returned while Hermes is still generating the answer. The older `POST /v1/voice` WAV request remains available for buffered diagnostics, but it has a separate Hermes chain and is deprecated for conversational use.

Home Assistant is not required in the voice path. The firmware retains the Voice PE hardware stack: XMOS far-field processing and acoustic echo reference, on-device wake word, physical mute, center button, volume dial, LED ring, AIC3204 DAC, and internal speaker. An encrypted ESPHome Native API/media-player path is included separately for optional Home Assistant music and announcements; it never carries Hermes microphone audio or conversations. Hermes remains self-hosted and should be exposed to the Worker only through an authenticated Cloudflare Tunnel and Access policy.

## Repository map

- [`firmware/hermes-voice-pe.factory.yaml`](firmware/hermes-voice-pe.factory.yaml) — universal, zero-secret ESPHome first-flash image with authorized Improv BLE/Serial and Dashboard Import.
- [`firmware/hermes-voice-pe.yaml`](firmware/hermes-voice-pe.yaml) — zero-secret public package imported when a factory device is adopted.
- [`firmware/hermes-voice-pe.local.yaml`](firmware/hermes-voice-pe.local.yaml) — clone-local/manual operator wrapper with ESPHome and per-device Worker credentials.
- [`firmware/components/hermes_voice`](firmware/components/hermes_voice) — custom realtime capture, WebSocket, flow-control, cancellation, and PCM playback component.
- [`gateway`](gateway) — Rust `workers-rs` gateway compiled to WebAssembly, including the per-device Durable Object.
- [`docs/research.md`](docs/research.md) — source-grounded feasibility findings and API choices.
- [`docs/architecture.md`](docs/architecture.md) — realtime pipeline, state ownership, conversation rules, and safety boundaries.
- [`docs/protocol.md`](docs/protocol.md) — exact device/gateway realtime and buffered wire contracts.
- [`docs/deployment.md`](docs/deployment.md) — Hermes, Tunnel, Worker, Durable Object, and firmware configuration.
- [`docs/testing.md`](docs/testing.md) — automated verification, latency instrumentation, and physical release tests.
- [`docs/esphome-adoption.md`](docs/esphome-adoption.md) — factory provisioning, Bluetooth-proxy semantics, Device Builder adoption, gateway enrollment, OTA and recovery contract.
- [`docs/proactive-delivery.md`](docs/proactive-delivery.md) — stretch-goal design for Hermes-initiated announcements and contextual conversations on one or more devices.
- [`docs/multi-agent-wake-routing.md`](docs/multi-agent-wake-routing.md) — stretch-goal design for multiple wake phrases that route to isolated Hermes agents and conversation lanes.

## Quick start

1. Install the audited Hermes Agent commit [`4aa499f`](https://github.com/NousResearch/hermes-agent/tree/4aa499ff9f3fcc0c38ce61da46805a4dcc8f612e) (declared version 0.18.2), or another exact commit/image digest you have re-audited; enable its authenticated API server and verify streaming `POST /v1/responses` locally.
2. Expose only the loopback Hermes API through Cloudflare Tunnel and an Access service-token policy.
3. Configure and deploy the Worker and its Durable Object from [`gateway`](gateway).
4. For a clone-local manual install, copy `firmware/secrets.example.yaml` to `firmware/secrets.yaml`, set the realtime gateway URL and a unique per-device token, then flash:

```sh
uvx --from esphome==2026.6.0 esphome run firmware/hermes-voice-pe.local.yaml
```

For a universal blank device, flash [`firmware/hermes-voice-pe.factory.yaml`](firmware/hermes-voice-pe.factory.yaml), provision it with physically authorized Improv BLE (direct Bluetooth or an active Home Assistant Bluetooth proxy), and adopt it in ESPHome Device Builder. The first adopted image intentionally works with Hermes unconfigured. Add the Worker URL, per-device token, and a new random ESPHome OTA password to the adopter-owned YAML, then immediately install that hardening/enrollment image over native OTA. The complete flow and the required public-package release prerequisite are in [`docs/esphome-adoption.md`](docs/esphome-adoption.md).

The adopted device contains only Wi-Fi, gateway/device, OTA, and local Native API credentials. Hermes, ElevenLabs, and Cloudflare Access credentials remain Worker secrets. See [`docs/deployment.md`](docs/deployment.md) for the full procedure and every binding.

## Configuration ownership

| Location | User configures |
| --- | --- |
| ESPHome Device Builder or clone-local `secrets.yaml` | Wi-Fi, `wss://…/v2/realtime`, this unit's Worker token, and its ESPHome API/OTA credentials. |
| Voice PE identity | By default `voice-pe-<wifi-mac>`, exposed as **Hermes Device ID** and used as the Worker's token-map key; independent of ESPHome rename/friendly name. |
| Cloudflare Worker variables | Hermes HTTPS base URL, ElevenLabs voice ID, optional model/language/limit overrides, optional conversation-idle rotation, and the `VOICE_SESSIONS` Durable Object binding. |
| Cloudflare Worker secrets | ElevenLabs API key, Hermes API key, optional Access service token, a per-device token map, and optional device-to-Hermes long-term-memory scope map. |
| Hermes host | API server/key/model plus the tools, sandbox, and approval policy permitted for voice use. |

The Voice PE never connects to Hermes or ElevenLabs directly. It authenticates only to the Worker; the per-device Durable Object opens Scribe, Hermes, and TTS streams and owns the conversation UUID, Hermes-binding fence, safe completed-response head, Hermes session metadata, activity boundary, reset idempotency state, and current-turn journal. Home Assistant's optional local connection controls the independent media-player entity and exposes an authenticated **New Hermes Conversation** button; provider credentials never enter Home Assistant.

ESPHome management is a current compatibility boundary. Factory firmware is discoverable through standard Improv BLE, including through an active ESPHome Bluetooth proxy; Native API/mDNS then hands off to Home Assistant and Dashboard Import. BLE is disabled before wake/audio work and re-enabled on Wi-Fi loss. Voice PE is not itself used as a Bluetooth proxy.

## Conversation behavior

Realtime v2 is a stateful Hermes conversation, not a sequence of generic prompts. After the first successful turn, each newly committed user utterance is a **reply** in the same conversation: the gateway supplies the previous completed Hermes response as `previous_response_id`, and Hermes reconstructs the complete transcript, tool calls, and tool results. Cancellation or failure never promotes a partial answer.

A **new conversation** is normally an intentional user boundary. While idle, either press the Home Assistant **New Hermes Conversation** button or hold the Voice PE center button for 2–5 seconds. The gateway durably rotates the conversation UUID and clears the short-term response chain. A reconnect, reboot, ordinary wake, short button press, cancellation, or barge-in does not reset context. Optional `CONVERSATION_IDLE_SECONDS` can add an inactivity boundary, but it is off by default. As a safety fence, changing the configured Hermes base URL, model/route label, or resolved long-term-memory session key also starts a fresh chain instead of sending an old response ID into a different scope.

`X-Hermes-Session-Key` is deliberately separate: it scopes Hermes long-term memory and remains stable across ordinary new-conversation boundaries. Configure per-device scopes with the Worker secret `HERMES_SESSION_KEYS_JSON`; an unmapped device uses `voice:<device-id>`. Changing that resolved value is itself a safety boundary. The Hermes agent/profile is selected by `HERMES_BASE_URL` and `HERMES_API_KEY`; `HERMES_MODEL` is the API's advertised model/route label.

## Realtime guarantees and limits

Realtime v2 removes whole-utterance upload and complete-response TTS waits. It still waits for a **committed** transcript before starting a Hermes turn; mutable STT partials are never allowed to trigger tools. Hermes tool execution can also dominate latency.

Cancellation starts immediately at the voice transport: the device clears queued PCM, the gateway closes the active TTS context, ignores late frames for the cancelled turn, and aborts the Hermes SSE request. Its measured audible-stop target is defined in the test plan; it cannot undo a Hermes tool that already ran, and an aborted Hermes response is never promoted as conversation history. Voice approvals are not auto-approved; deployments that need interactive approval must add a trusted approval UI or use a voice-safe Hermes tool policy.

The buffered `POST /v1/voice` path remains useful for provider diagnosis. It uploads one complete WAV, starts TTS after the full Hermes response, and uses a separate `voice-buffered-<device-id>` named Hermes chain. It does not read or advance the realtime conversation, must not be used when validating realtime latency, and is deprecated as an interactive conversation transport.

## Future: Hermes-initiated delivery

A documented stretch goal lets Hermes speak first on one Voice PE or a configured group. It deliberately separates a context-free **announcement** from a **conversation invitation** whose completed proactive Hermes response becomes the parent of the user's reply. The design includes a Hermes delivery adapter, authenticated/idempotent Worker ingress, device/group targeting, quiet-hours and consent policy, first-responder claiming for group conversations, and a negotiated future device protocol. It is not implemented in realtime v2. See [`docs/proactive-delivery.md`](docs/proactive-delivery.md).

## Future: multiple wake words and agents

Another documented stretch goal allows phrases such as **“Hey Avery”** and **“Hey Jordan”** to select different Hermes agents. Each stable route resolves through a Worker-owned device allowlist to a distinct Hermes profile/API process, memory scope, completed-response chain and optional ElevenLabs voice. Wake words are selectors rather than user authentication; an unknown or unavailable route never falls back to another agent. The design, protocol-v3 direction, model constraints and isolation tests are in [`docs/multi-agent-wake-routing.md`](docs/multi-agent-wake-routing.md).

## Validation status

Compilation and synthetic tests are necessary but not sufficient. A physical Voice PE is required to validate microphone levels, endpoint timing, acoustic echo behavior during barge-in, bounded queues under impaired Wi-Fi, playback underruns, and end-of-speech-to-first-audio latency. The concrete release criteria are in [`docs/testing.md`](docs/testing.md).
