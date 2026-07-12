# Home Assistant Voice Hermes Gateway

Run the same realtime Rust/WASM voice gateway locally as a protected Home
Assistant App (formerly add-on). A Home Assistant Voice: Preview Edition sends
authenticated microphone audio over WSS; the gateway streams it to ElevenLabs
Scribe v2 Realtime, maintains an actual Hermes Responses conversation, streams
Hermes text into ElevenLabs Flash v2.5 TTS, and returns PCM while the answer is
still being generated.

This App:

- keeps one durable short-term response chain and one explicit Hermes
  long-term-memory scope per device;
- uses the standard read-only `/ssl` mount, defaulting to
  `/ssl/fullchain.pem` and `/ssl/privkey.pem`;
- requires publicly trusted TLS for the Voice PE and Hermes, with no custom
  Hermes CA option;
- stores local Durable Object state under the App's backed-up `/data` volume;
- requests no Home Assistant, Supervisor, ingress, host-network, audio, device,
  or privileged access; and
- leaves Home Assistant music and announcements on the independent encrypted
  ESPHome Native API/media-player path.

The App is experimental. Treat its self-hosted `workerd`/single-machine
local-disk Durable Object stack as beta-quality infrastructure; it is not
Cloudflare's managed edge service. No physical Voice PE or Home Assistant OS
installation has yet passed this repository's release matrix.

Keep its WSS port on a firewalled LAN or VPN. A private Hermes origin requires
`allow_private_upstreams: true`, which expands all gateway egress to private
ranges; loopback remains denied.

See [the App documentation](DOCS.md) for certificate, Hermes, ElevenLabs,
device-enrollment, backup, and migration instructions.
