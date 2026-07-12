# Privacy, room policy and data lifecycle

Voice is inherently personal data. Treat microphone audio, transcripts,
generated speech, tool arguments/results and memory-provider records as
sensitive even when they do not contain an obvious name or account number.

## Data path

1. Voice PE sends PCM and protocol metadata only to the authenticated gateway,
   either the Cloudflare Worker or the same WASM in the Home Assistant App.
2. The per-device Durable Object forwards PCM to the configured STT provider.
3. Only a committed transcript is sent to the selected Hermes profile.
4. Speakable Hermes text is sent to the configured TTS provider and returned as
   PCM. Structured tool payloads are neither spoken nor logged by the gateway.
5. Home Assistant's optional Native API/media path does not receive Hermes
   microphone audio, transcripts or provider credentials.

The Durable Object intentionally persists opaque conversation/response IDs,
the one-way, non-bearer agent-binding digest, timestamps, reset idempotency,
bounded turn-recovery state and aggregate 24-hour usage counters. It does not
intentionally persist audio, transcripts, spoken text, raw binding/session-key
values or tool payloads. Hermes stores the full response chain, including tool
history, and its configured memory provider may store additional derived
memory.

In the Home Assistant App, the Durable Object metadata is local-disk state under
its backed-up `/data` volume. In Cloudflare it is managed Durable Object storage;
the stores are independent and are not safely copied between backends.

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

## Home Assistant App option and backup boundary

The local App requests no ingress, Home Assistant/Supervisor API, host network,
audio, device, privileged capability, or writable Home Assistant configuration
mount. It reads TLS files from the standard read-only `/ssl` map and serves only
the WSS/health port. Optional Home Assistant music and announcements stay on the
independent encrypted ESPHome Native API media path; Home Assistant Core does not
receive Hermes audio, transcripts, response state, or provider credentials.

Keep that published port on a firewalled LAN or VPN, or behind a
WebSocket-aware reverse proxy with connection and handshake rate limits. The
off-by-default `allow_private_upstreams` option expands **all** gateway egress to
private ranges, not only Hermes; loopback/workerd-local destinations remain
denied. A private Hermes origin must still use publicly trusted TLS because the
App has no custom Hermes CA option.

Supervisor does receive the local deployment's Hermes, ElevenLabs, optional
Access and per-device options. The App uses Home Assistant's recommended
`password` schema type for credential fields, but that masks form display only.
Supervisor resolves `!secret` references and gives the App clear values in
`/data/options.json`; cold App backups include those values with the local
conversation metadata. The launcher safely snapshots and validates the source
options/TLS files, hands a mode-restricted tmpfs generation to the dedicated
unprivileged `gateway` account, and deletes it after readiness. Reload probes
stay root-only until the old runtime stops and a stable replacement is selected;
core dumps are disabled. The launcher never intentionally puts a credential into
a process argument, exported environment, or log. Supervisor itself [logs a requested `!secret` name](https://github.com/home-assistant/supervisor/blob/1e81816c855310f3217b7f5113573b1b661d4f4d/supervisor/homeassistant/secrets.py#L24-L32)
at info level, so avoid person/room-identifying secret names if those logs may be
shared.

The same options file and backup also contain the stable hardware-derived
`voice-pe-<Wi-Fi-MAC>` device ID plus unmasked Hermes model/profile/revision
labels. Those values are operational identifiers and can reveal device linkage
or room/person naming even though the bearer and session fields are masked in
the form. Use neutral profile/revision/session labels where practical and redact
device IDs and labels from shared diagnostics.

Current July 2026 Supervisor also performs a throttled pwned-password check for
every non-empty App option typed `password`: it computes SHA-1 locally and sends
only the first five hexadecimal characters to the Pwned Passwords range API.
Neither the clear value nor full hash is sent, but the remote service observes a
k-anonymous prefix, request time, and source IP. This also covers random API and
device tokens because the secure UI schema masks them as passwords. See the
pinned Supervisor [hash collection](https://github.com/home-assistant/supervisor/blob/1e81816c855310f3217b7f5113573b1b661d4f4d/supervisor/apps/options.py#L150-L153),
[daily App check](https://github.com/home-assistant/supervisor/blob/1e81816c855310f3217b7f5113573b1b661d4f4d/supervisor/resolution/checks/app_pwned.py#L31-L52),
and [k-anonymous request](https://github.com/home-assistant/supervisor/blob/1e81816c855310f3217b7f5113573b1b661d4f4d/supervisor/utils/pwned.py#L10-L29).

Use encrypted backups, keep the matching [backup emergency kit](https://www.home-assistant.io/more-info/backup-emergency-kit/)
off-device, and inspect/redact exports before sharing. Avoid Supervisor DEBUG
logging during credential work and conservatively treat debug logs/diagnostics
as capable of containing resolved options. Never run a restored clone beside
its source with the same hostname/tokens: rotate Hermes, ElevenLabs, Access,
device tokens and lab session scopes first. A Cloudflare-to-local or
local-to-Cloudflare cutover begins a new short-term conversation; the explicitly
reused Hermes session key may preserve intended long-term memory but does not
migrate the completed response chain.

For emergency secret rotation, stop the App before changing options, save the
replacement values while it is stopped, then start it and verify readiness. An
invalid changed option stops the runtime rather than leaving old credentials
active. Only an invalid certificate-file renewal may retain the active validated
certificate, and only when all normalized options are unchanged.

A compromised TLS private key follows the same stop-first rule: stop the App,
replace both key and chain, revoke the old certificate, then restart and verify
the externally served fingerprint. The routine-renewal exception marks App
health degraded and stops at active-certificate expiry or after its bounded
grace period; it is not an emergency key-revocation mechanism.
