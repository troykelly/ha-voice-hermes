# Home Assistant Voice PE for Hermes

This repository replaces the Home Assistant-dependent voice transport on the **Home Assistant Voice: Preview Edition** with a direct, authenticated connection to a Hermes agent.

The primary transport is realtime v2 and has two deployment targets:

```text
Voice PE ── persistent WSS /v2/realtime ──┬─ Cloudflare Worker + managed Durable Object ──┐
                                          └─ Home Assistant App + local `workerd` DO ──┤
                                                                                   ├─ ElevenLabs Scribe v2 Realtime WSS
                                                                                   ├─ Hermes /v1/responses SSE
                                                                                   └─ ElevenLabs Flash v2.5 multi-context TTS WSS
Voice PE ◀────────────── streamed PCM16 playback ─────────────────────────────────────────────────────────────────┘
```

Microphone PCM is uploaded while the user is speaking, Hermes text is consumed as SSE deltas, and ElevenLabs audio is returned while Hermes is still generating the answer. The older `POST /v1/voice` WAV request is disabled by default. If an operator explicitly enables it, it is a stateless, non-stored, memory-isolated provider diagnostic—not a conversation transport or an automatic fallback.

Home Assistant is not required in the voice path. The firmware retains the Voice PE hardware stack: XMOS far-field processing and acoustic echo reference, on-device wake word, physical mute, center button, volume dial, LED ring, AIC3204 DAC, and internal speaker. An encrypted ESPHome Native API/media-player path is included separately for optional Home Assistant music and announcements; it never carries Hermes microphone audio or conversations. Hermes remains self-hosted. The Cloudflare deployment should expose it only through an authenticated Tunnel and Access policy. The local Home Assistant App instead requires an HTTPS Hermes origin reachable from its protected container; private-network access is a deliberate, off-by-default option.

## Repository map

- [`firmware/hermes-voice-pe.factory.yaml`](firmware/hermes-voice-pe.factory.yaml) — universal, zero-secret ESPHome first-flash image with authorized Improv BLE/Serial and Dashboard Import.
- [`firmware/hermes-voice-pe.yaml`](firmware/hermes-voice-pe.yaml) — zero-secret public package imported when a factory device is adopted.
- [`firmware/hermes-voice-pe.local.yaml`](firmware/hermes-voice-pe.local.yaml) — clone-local/manual operator wrapper with ESPHome and per-device Worker credentials.
- [`firmware/components/hermes_voice`](firmware/components/hermes_voice) — custom realtime capture, WebSocket, flow-control, cancellation, and PCM playback component.
- [`gateway`](gateway) — Rust `workers-rs` gateway compiled to WebAssembly, including the per-device Durable Object.
- [`ha_voice_hermes_gateway`](ha_voice_hermes_gateway) — experimental Home Assistant App that runs the same optimized WASM under pinned `workerd`, with direct TLS and local-disk Durable Object persistence.
- [`repository.yaml`](repository.yaml) — makes this same repository installable as a Home Assistant App repository; a separate add-on repository is not required.
- [`docs/research.md`](docs/research.md) — source-grounded feasibility findings and API choices.
- [`docs/architecture.md`](docs/architecture.md) — realtime pipeline, state ownership, conversation rules, and safety boundaries.
- [`docs/protocol.md`](docs/protocol.md) — exact device/gateway realtime and buffered wire contracts.
- [`docs/deployment.md`](docs/deployment.md) — Hermes, Tunnel, Worker, Durable Object, and firmware configuration.
- [`docs/testing.md`](docs/testing.md) — automated verification, latency instrumentation, and physical release tests.
- [`docs/feature-matrix.md`](docs/feature-matrix.md) — explicit retained, replaced, and currently omitted upstream Voice PE features.
- [`docs/privacy-security.md`](docs/privacy-security.md) — audio/transcript data flow, shared-room defaults, retention, reset-versus-delete semantics, and decommissioning.
- [`docs/release-checklist.md`](docs/release-checklist.md) — automated, physical, tagging, artifact, and immutable-source publication gates.
- [`docs/esphome-adoption.md`](docs/esphome-adoption.md) — factory provisioning, Bluetooth-proxy semantics, Device Builder adoption, gateway enrollment, OTA and recovery contract.
- [`docs/proactive-delivery.md`](docs/proactive-delivery.md) — stretch-goal design for Hermes-initiated announcements and contextual conversations on one or more devices.
- [`docs/multi-agent-wake-routing.md`](docs/multi-agent-wake-routing.md) — stretch-goal design for multiple wake phrases that route to isolated Hermes agents and conversation lanes.
- [`docs/protocol-v3-roadmap.md`](docs/protocol-v3-roadmap.md) — one canonical future negotiation and precedence model shared by proactive delivery and multi-agent wake routing.

## Quick start

1. Install the audited Hermes Agent commit [`5ecc079`](https://github.com/NousResearch/hermes-agent/tree/5ecc07986f46463ca3096679b03a46402eb19cee), or another exact commit/image digest you have re-audited; enable its authenticated API server and run `scripts/verify-hermes-contract.py` against it.
2. Choose a gateway deployment: expose only loopback Hermes through Cloudflare Tunnel/Access and deploy [`gateway`](gateway), or install the same-repository [Home Assistant App](ha_voice_hermes_gateway/DOCS.md) with publicly trusted `/ssl/fullchain.pem` and `/ssl/privkey.pem` plus an HTTPS Hermes origin. Private origins require explicit `allow_private_upstreams: true`.
3. Configure a unique device token and explicit Hermes memory scope for every Voice PE in the chosen gateway.
4. For a clone-local manual install, copy `firmware/secrets.example.yaml` to `firmware/secrets.yaml`, set the realtime gateway URL and a unique per-device token, then flash:

```sh
uvx --from esphome==2026.6.5 esphome run firmware/hermes-voice-pe.local.yaml
```

For a universal blank device, flash [`firmware/hermes-voice-pe.factory.yaml`](firmware/hermes-voice-pe.factory.yaml), provision it with physically authorized Improv BLE (direct Bluetooth or an active Home Assistant Bluetooth proxy), and adopt it in ESPHome Device Builder. The first adopted image intentionally works with Hermes unconfigured. Read its **Hermes Device ID**, add matching token and memory-scope entries to the Worker, then add the Worker URL, per-device token, and a new random ESPHome OTA password to the adopter-owned YAML. Immediately install that hardening/enrollment image over native OTA. The complete flow and the required public-package release prerequisite are in [`docs/esphome-adoption.md`](docs/esphome-adoption.md).

The adopted device contains only Wi-Fi, gateway/device, OTA, and local Native API credentials. Hermes, ElevenLabs, and optional Cloudflare Access credentials remain in Worker secrets or masked Home Assistant App options. See [`docs/deployment.md`](docs/deployment.md) for both deployment procedures and every binding.

For the local App, keep the WSS port on a firewalled LAN or VPN, or place it
behind a WebSocket-aware reverse proxy with connection and handshake rate
limits. The private-upstream opt-in expands all gateway egress to private
network ranges; loopback remains denied, and private Hermes still needs
publicly trusted TLS.

## Configuration ownership

| Location | User configures |
| --- | --- |
| ESPHome Device Builder or clone-local `secrets.yaml` | Wi-Fi, `wss://…/v2/realtime`, this unit's Worker token, and its ESPHome API/OTA credentials. |
| Voice PE identity | By default `voice-pe-<wifi-mac>`, exposed as **Hermes Device ID** and used as the Worker's token-map key; independent of ESPHome rename/friendly name. |
| Cloudflare Worker variables | Hermes HTTPS base URL, agent-profile identity/revision, ElevenLabs voice ID, model/language/limit overrides, the 15-minute default conversation boundary, per-socket/durable usage quotas, and the `VOICE_SESSIONS` Durable Object binding. |
| Cloudflare Worker secrets | ElevenLabs API key, Hermes API key, optional Access service token, the required per-device token map, and the required device-to-Hermes long-term-memory scope map. |
| Local Home Assistant App | The same provider/agent settings plus unique per-device token/session entries; standard read-only `/ssl` certificate paths; local Durable Object state in backed-up `/data`. Resolved App options and backups contain clear credential values and must be protected. |
| Hermes host | API server/key/model plus the tools, sandbox, and approval policy permitted for voice use. |

The Voice PE never connects to Hermes or ElevenLabs directly. It authenticates only to the Worker; the per-device Durable Object opens Scribe, Hermes, and TTS streams and owns the conversation UUID, Hermes-binding fence, safe completed-response head, Hermes session metadata, activity boundary, reset idempotency state, and current-turn journal. Home Assistant's optional local device connection controls the independent media-player entity and exposes an authenticated **New Hermes Conversation** button. In the Cloudflare deployment provider credentials never enter Home Assistant; in the local deployment Supervisor stores them as masked App options, while Home Assistant Core and the Native API media path do not receive them.

In the local deployment, “Worker” in the protocol means the identical WASM gateway running inside the Home Assistant App. The App itself does not use ingress or Home Assistant/Supervisor APIs. Home Assistant music and announcements continue through the independent encrypted ESPHome Native API path, not through the App.

ESPHome management is a current compatibility boundary. Factory firmware is discoverable through standard Improv BLE, including through an active ESPHome Bluetooth proxy; Native API/mDNS then hands off to Home Assistant and Dashboard Import. BLE is disabled before wake/audio work and re-enabled on Wi-Fi loss. Voice PE is not itself used as a Bluetooth proxy.

## Conversation behavior

Realtime v2 is a stateful Hermes conversation, not a sequence of generic prompts. After the first successful turn, each newly committed user utterance is a **reply** in the same conversation: the gateway supplies the previous completed Hermes response as `previous_response_id`, and Hermes reconstructs the complete transcript, tool calls, and tool results. Cancellation or failure never promotes a partial answer.

A **new conversation** is normally an intentional user boundary. While idle, either press the Home Assistant **New Hermes Conversation** button or hold the Voice PE center button for 2–5 seconds. The gateway durably rotates the conversation UUID and clears the short-term response chain. A reconnect, reboot, ordinary wake, short button press, cancellation, or barge-in does not by itself reset context. The secure default also rotates after 900 seconds of inactivity. Disabling that boundary requires both an off value and `ALLOW_UNBOUNDED_CONVERSATION=true`. Changing the configured Hermes base URL, model/route label, profile ID, binding revision, or resolved long-term-memory session key starts a fresh chain instead of crossing agent scopes.

Cloudflare Durable Object storage and the App's local-disk storage are separate. Switching a device between them starts a new short-term conversation; there is no supported opaque-state migration. Reusing the intentional Hermes session key may preserve long-term memory, but it does not transfer the completed response chain.

`X-Hermes-Session-Key` is deliberately separate: it scopes Hermes long-term memory and remains stable across ordinary new-conversation boundaries. Configure an explicit scope for every device in the Worker secret `HERMES_SESSION_KEYS_JSON`. Implicit `voice:<device-id>` scopes require the deliberate compatibility flag `ALLOW_IMPLICIT_HERMES_CONTEXT=true`. The Hermes agent/profile is selected by `HERMES_BASE_URL` and `HERMES_API_KEY`; `HERMES_MODEL` is only the API's advertised model/route label. Pin `HERMES_PROFILE_ID` and increment `HERMES_BINDING_REVISION` whenever SOUL, tools, sandbox, memory policy, voice instructions, or another behavior-defining property changes.

An interrupted turn after Hermes may have accepted the request is **ambiguous**: a tool could have run even if the final response was lost. The current audited Hermes streaming branch does not apply its non-streaming `Idempotency-Key` cache. The gateway therefore sends a deterministic key for forward compatibility, never replays the utterance, and blocks later turns with `conversation_ambiguous` until the user explicitly starts a new conversation.

## Realtime guarantees and limits

Realtime v2 removes whole-utterance upload and complete-response TTS waits. It still waits for a **committed** transcript before starting a Hermes turn; mutable STT partials are never allowed to trigger tools. Hermes tool execution can also dominate latency.

The implemented realtime path uses ElevenLabs Scribe v2 Realtime plus ElevenLabs Flash v2.5 multi-context TTS. An OpenAI-compatible Whisper endpoint is supported only by the disabled buffered diagnostic route; it is not silently substituted into realtime because batch transcription would give up microphone streaming and its latency guarantees.

Cancellation starts immediately at the voice transport: the device clears queued PCM, the gateway closes the active TTS context, ignores late frames for the cancelled turn, and aborts the Hermes SSE request. Its measured audible-stop target is defined in the test plan; it cannot undo a Hermes tool that already ran, and an aborted Hermes response is never promoted as conversation history. Voice approvals are not auto-approved; deployments that need interactive approval must add a trusted approval UI or use a voice-safe Hermes tool policy.

The buffered `POST /v1/voice` path is hidden as `404` unless `DIAGNOSTIC_V1_ENABLED=true`. It uploads one complete WAV, makes a non-streaming Hermes request with `store: false`, uses a hashed diagnostic memory scope, and starts TTS only after the full answer. It has no response chain, does not read or advance realtime state, and must not be used when validating realtime latency.

## Future: Hermes-initiated delivery

A documented stretch goal lets Hermes speak first on one Voice PE or a configured group. It deliberately separates a context-free **announcement** from a **conversation invitation** whose completed proactive Hermes response becomes the parent of the user's reply. The design includes a Hermes delivery adapter, authenticated/idempotent Worker ingress, device/group targeting, quiet-hours and consent policy, first-responder claiming for group conversations, and a negotiated future device protocol. It is not implemented in realtime v2. See [`docs/proactive-delivery.md`](docs/proactive-delivery.md).

## Future: multiple wake words and agents

Another documented stretch goal allows phrases such as **“Hey Avery”** and **“Hey Jordan”** to select different Hermes agents. Each stable route resolves through a Worker-owned device allowlist to a distinct Hermes profile/API process, memory scope, completed-response chain and optional ElevenLabs voice. Wake words are selectors rather than user authentication; an unknown or unavailable route never falls back to another agent. The design, protocol-v3 direction, model constraints and isolation tests are in [`docs/multi-agent-wake-routing.md`](docs/multi-agent-wake-routing.md).

## Validation status

The firmware and gateway compile; 85 Rust tests and 14 tests against the optimized, compiled Worker in workerd pass. Runtime coverage includes Durable Object hibernation/replacement, immediate durable-quota admission and side-effect gates, strict fail-closed provider redirects/status/media types, a hard decoded-output ceiling across irregular TTS chunks, and a byte-exact two-turn mocked Scribe → Hermes SSE → ElevenLabs TTS streaming conversation. The Home Assistant App adds packaging, TLS/configuration, certificate-reload, and local-persistence tests, but no physical Voice PE, Home Assistant OS target, or live Hermes/ElevenLabs credentials were available for the recorded run. Physical adoption/OTA, App install/backup/restore, AEC/barge-in, impaired-Wi-Fi, playback, live-provider continuity, PII/DLP policy validation, and measured latency remain release gates; the repository deliberately has no release tag until they pass. See [`docs/testing.md`](docs/testing.md).
