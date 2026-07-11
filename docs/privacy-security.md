# Privacy, room policy and data lifecycle

Voice is inherently personal data. Treat microphone audio, transcripts,
generated speech, tool arguments/results and memory-provider records as
sensitive even when they do not contain an obvious name or account number.

## Data path

1. Voice PE sends PCM and protocol metadata only to the authenticated Worker.
2. The per-device Durable Object forwards PCM to the configured STT provider.
3. Only a committed transcript is sent to the selected Hermes profile.
4. Speakable Hermes text is sent to the configured TTS provider and returned as
   PCM. Structured tool payloads are neither spoken nor logged by the Worker.
5. Home Assistant's optional Native API/media path does not receive Hermes
   microphone audio, transcripts or provider credentials.

The Durable Object intentionally persists opaque conversation/response IDs,
the one-way, non-bearer agent-binding digest, timestamps, reset idempotency,
bounded turn-recovery state and aggregate 24-hour usage counters. It does not
intentionally persist audio, transcripts, spoken text, raw binding/session-key
values or tool payloads. Hermes stores the full response chain, including tool
history, and its configured memory provider may store additional derived
memory.

The binding digest is data minimization, not encryption: predictable profile,
room or session-scope labels may still be susceptible to offline guessing by
an attacker who already has Durable Object storage. Treat that storage and its
opaque correlation IDs as sensitive operational metadata.

## Spoken-output boundary

The gateway discards structured tool events and removes Markdown/code/URL
formatting from text selected for speech. That sanitizer is not data-loss
prevention and does not understand the meaning of an ordinary Hermes reply. If
Hermes writes a password, token, medical fact, account number or other PII into
`response.output_text.delta`, the TTS provider and room speaker will receive
it. Production therefore requires a dedicated voice-safe Hermes profile with
restricted tools, sandbox/approval policy and explicit rules against returning
sensitive values. Test representative tool results and prompt-injection cases;
do not claim that the gateway alone prevents semantic disclosure.

Decoded provider PCM is independently capped at 30 seconds per turn before it
is reframed or forwarded. This bounds a faulty or compromised TTS stream; it
does not establish that the synthesized content is appropriate to speak.

## Required device policy

Production configuration requires an explicit per-device token map and an
explicit long-term-memory session-key mapping for every device. Shared device
credentials and implicit `voice:<device-id>` memory scopes are disabled unless
the operator deliberately enables their unsafe compatibility flags.

The secure default short-term conversation idle boundary is 15 minutes. An
unbounded chain must be explicitly enabled. Choose the policy by room, not by
hardware capability:

- a personal office may use a longer idle boundary and a personal memory scope;
- a kitchen, lounge or meeting room should use a shorter boundary, a shared-room
  scope, a restricted Hermes tool profile and no sensitive personal memory;
- a public or guest area should normally disable long-term memory and expose
  only low-impact tools.

Wake words identify an intended agent; they do not authenticate a speaker.
Device bearer authentication identifies hardware, not the person in the room.

## “New conversation” is not “forget me”

`conversation.reset` rotates the short-term Responses chain. It deliberately
keeps the configured `X-Hermes-Session-Key`, so Hermes long-term memory can
survive an ordinary new topic. The UI must label this operation **New Hermes
Conversation**, not delete, erase or forget.

A complete deletion/decommission procedure is operator-owned:

1. remove the device token and memory-scope mappings and terminate its active
   Worker socket;
2. erase/reflash the Voice PE so compiled Wi-Fi, API, OTA and Worker credentials
   are gone;
3. delete the device's Durable Object storage through an authorized maintenance
   operation or deployment tooling;
4. delete the associated Hermes response/session records and any records held
   by its memory provider;
5. delete retained STT/TTS history according to the provider account policy.

Do not promise that pressing the reset button performs those external deletion
steps.

## ElevenLabs retention

`ELEVENLABS_ENABLE_LOGGING` is explicit. `true` uses ordinary provider history;
`false` requests Zero Retention Mode for realtime and buffered STT/TTS. Zero
Retention Mode is available only to eligible Enterprise accounts, so deployment
must verify that requests succeed and that no request appears in provider
history before claiming zero retention.

## Diagnostics and observability

The buffered `/v1/voice` diagnostic endpoint is disabled by default. Production
logs must contain only device aliases, opaque turn IDs, stages, durations,
counts and stable error categories. Never log audio, transcript text, spoken
text, tool payloads, credentials, provider response bodies or memory contents.
Use a non-identifying device alias in dashboards where the raw device ID could
reveal a room or person.

If an operator enables v1, one complete WAV is processed by the selected batch
STT provider and Hermes. The request uses `store: false`, no response-chain
pointer, and a hashed diagnostic memory scope isolated from realtime. That
prevents a short-term Responses chain; it does not override Hermes memory-plugin
behavior or provider retention contracts. Use synthetic speech and a restricted
profile for diagnostics, then disable the route again.
