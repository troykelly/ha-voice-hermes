# Verification and test plan

Realtime voice crosses firmware, Wi-Fi, a Cloudflare or local `workerd` runtime,
a Durable Object, three remote streams, and physical audio. A successful compile
or one spoken reply is not enough to claim that it is fully streaming or low
latency.

## Current validation snapshot (2026-07-12)

- ESPHome 2026.6.5 validates and clean-compiles both the zero-secret factory and configured/encrypted local variants. The exact final tree uses 2,102,659 bytes (25.9% of the 8,126,464-byte app slot) for factory and 2,101,163 bytes (25.9%) for configured firmware; each uses 67,404 bytes (20.6%) of RAM. Generated SDK configuration has mbedTLS certificate-time verification and SNTP enabled.
- The resolved factory graph has no fallback AP, captive portal or web-server OTA, and the factory/public YAML has no `!secret` dependency. The configured schema rejects realtime capture limits above 30 seconds.
- ESPHome's real Dashboard Import materializer produces adopter-owned YAML with the package reference, Wi-Fi secret references and a unique API encryption key. Anonymous end-to-end validation of the generated file remains blocked until the configured immutable release tag is published.
- Gateway results: 86 Rust tests and 14 Vitest tests against the optimized, compiled Worker in the real Workers runtime pass. `rustfmt`, strict Clippy, `cargo audit`, `npm audit`, optimized WASM build, and Wrangler 4.110.0 deployment dry-run pass. The runtime suite covers redacted health; rejected unauthenticated/cross-device upgrades; hidden v1 diagnostics; hibernating WebSocket eviction; reset idempotency across eviction; socket replacement; durable 24-hour quota continuity and exact-limit admission/reset/ping enforcement; fail-closed credentialed redirects plus Hermes status/media-type rejection before `response.start`; a hard decoded-output PCM ceiling across irregular TTS chunks; and a complete two-turn STT → Hermes SSE → TTS exchange with conversation continuity and exact streamed PCM verification.
- The Home Assistant App is an additional experimental packaging/runtime target.
  Its automated configuration, TLS, artifact, certificate-watch and local-disk
  tests are separate from physical validation. No Home Assistant OS install,
  App cold-backup/restore, real certificate renewal, or physical Voice PE through
  the App was available for this snapshot.
- No Voice PE was attached for this run. Every physical, live-provider, proxy, OTA-authentication and long-duration fault item below remains required before a release may claim complete ESPHome compatibility or measured realtime latency.

## Automated build gates

### Firmware

Run the exact supported ESPHome version:

```sh
uvx --from esphome==2026.6.5 esphome config firmware/hermes-voice-pe.factory.yaml
uvx --from esphome==2026.6.5 esphome compile firmware/hermes-voice-pe.factory.yaml
uvx --from esphome==2026.6.5 esphome config firmware/hermes-voice-pe.local.yaml
uvx --from esphome==2026.6.5 esphome compile firmware/hermes-voice-pe.local.yaml
```

This verifies schema, generated C++, ESP-IDF dependencies, linking, partitions, media codecs, and static image fit. It cannot verify XMOS levels, AEC, runtime PSRAM/heap pressure, Wi-Fi behavior, media ducking, or speaker timing.

### Factory adoption and ESPHome management

Factory/adoption compatibility is release-blocking, not an optional future test. In addition to the compile above:

- anonymously fetch the exact tagged `dashboard_import_url` and custom-component ref; reject `@main`, missing paths, local component sources and public-package `!secret` references;
- call ESPHome's `dashboard_import.import_config(..., encryption=True)` in an empty directory, then validate and compile the generated YAML without any gateway/Hermes setting;
- verify empty `gateway_url` + empty token succeeds in inactive mode, a partial pair fails schema validation, a credential-free WSS URL + valid token succeeds, and normal `!secret` ESPHome output does not reveal the token;
- compile with the package minimum and pinned validation patch, checking both OTA-slot fit and headroom;
- scan the factory binary for known Wi-Fi, API, OTA, Worker, Hermes, ElevenLabs and Cloudflare canary secrets;
- flash the same binary onto at least two erased units and verify unique MAC-suffixed mDNS names plus distinct hardware-derived Hermes device IDs;
- provision one unit through a Home Assistant Bluetooth adapter or active ESPHome Bluetooth proxy, and one through Improv Serial; both require the center-button authorization where applicable;
- verify Home Assistant discovery and Device Builder **Adopt**, first OTA without gateway values, generated API encryption, logs, media and diagnostics;
- add the Worker URL/per-device token plus a random OTA password and OTA again; verify a wrong/missing password is then rejected, voice enables without USB, and the Worker identity survives ESPHome rename/friendly-name change;
- fault DNS, TLS, authentication, Worker and Hermes for 24 hours while repeatedly exercising API, logs, HA media and OTA; require no reboot, starvation, reconnect storm or resource trend;
- force safe mode, interrupt OTA power, lose Wi-Fi and recover over BLE/serial, then erase/reflash the published factory image over USB.

See [`esphome-adoption.md`](esphome-adoption.md) for the exact lifecycle and reset/decommissioning semantics.

### Gateway

Run from `gateway`:

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

That native build is appropriate for development. The App freshness and
release checks use the fixed Linux/amd64 toolchain instead; from the repository
root run:

```sh
scripts/build-gateway-worker-canonical.sh
scripts/sync-addon-worker-artifacts.sh --check
scripts/check-gateway-artifact-privacy.sh
```

Do not replace the vendored App bytes with a macOS, Linux/arm64, or otherwise
native build. Those toolchains can emit semantically equivalent but
byte-different WASM even with identical Rust and Node versions.

Native Rust tests do not exercise Cloudflare WebSocket bindings or Durable Object hibernation. The Vitest Workers-pool suite runs the compiled Worker and a routed auxiliary provider Worker inside workerd. It deliberately evicts the Durable Object while its socket is open, verifies hibernation attachment/recovery, replaces an authenticated socket without resetting the persisted 24-hour usage window, and drives two complete realtime turns through mocked outbound WebSockets and fragmented SSE. The optimized WASM build and Wrangler dry-run remain separate gates.

The full-turn fixture sends irregular provider PCM event sizes, including odd-byte boundaries, and requires the gateway to emit contiguous 2,048-byte device frames plus at most one final short frame with exact sequence, `first_sample`, and byte-for-byte audio reconstruction. It also proves that mutable/final transcripts are not returned to the headless device, the second Hermes request uses the first completed response ID, and first TTS audio is produced before the terminal Hermes SSE event. A separate fixture returns an STT `302` and verifies no request—or `xi-api-key`—reaches the redirect target. Credentialed Worker requests use workerd-supported manual redirect mode and then explicitly require the expected `101`, success, or exact recovery status; every `3xx` is rejected without following `Location`.

At minimum, automated tests must cover:

- device ID, minimum token length, unique per-device credentials, hashed constant-time authentication, established-socket revocation, and explicit shared-token opt-in;
- routing a validated `/v2/realtime` upgrade to the device-named `VOICE_SESSIONS` object;
- exact WebSocket subprotocol negotiation;
- the 20-byte frame codec, network-order integers, PCM little-endian payload, and both directions;
- sequence and `first_sample` monotonicity;
- end-hint flag handling, fatal discontinuity, and rejection of reserved flag bits;
- odd, empty, oversized, wrong-kind, stale-turn, short-nonterminal, post-short, and truncated audio frames;
- 32-frame ACK windows, coalesced cumulative ACKs, and queue overflow cancellation;
- control direction, required fields, turn ordering, duplicate IDs, cancel, ping/pong, `conversation.reset`/`.done`, and control-size limits;
- fragmented/multiline Responses SSE parsing with event and total-byte limits;
- `response.created`, `response.output_text.delta`, tool items, `response.completed`, `response.failed`, and absence of a `[DONE]` requirement;
- conversation promotion only for terminal `status: completed`;
- persisted conversation UUID, digest-only Hermes binding metadata (never the raw session key or binding fields), returned session metadata, explicit reset request-ID idempotency, reset/turn exclusion, accepted-turn idle activity, binding-change fencing, opt-in idle bounds, and legacy-head migration;
- mirrored per-socket and durable per-device message, turn, and captured-audio quotas, including 24-hour-window continuity across socket replacement;
- deterministic Hermes idempotency keys and crash-journal reconciliation that promotes completed responses but fails closed on starting, incomplete, missing, and malformed states;
- phrase segmentation across arbitrary Unicode/token boundaries, cross-phrase fenced-code/split-backtick suppression, and same-phrase Markdown-link/bare-URL cleanup;
- ElevenLabs Scribe messages, manual commit with either the coalescer's pending PCM or an empty audio field, multi-context initialization, flush, final, and close-context cancellation;
- base64 provider limits and 2,048-byte PCM downlink reframing;
- disabled-by-default v1 routing, stateless/non-stored diagnostic Hermes requests, isolated hashed memory scopes, WAV validation, alternate STT multipart fields, complete Hermes extraction, and streaming HTTP TTS.

### Home Assistant App

The local App must reuse the optimized Worker artifacts; it must not acquire a
second gateway implementation. Automated release gates must:

- validate `repository.yaml`, App `config.yaml`, translations, documentation,
  custom AppArmor, `amd64`/`aarch64` architecture declarations, cold backup,
  read-only `/ssl`, published port 8443, and the absence of ingress, host
  networking, Home Assistant/Supervisor APIs, audio, devices and privileges;
- prove the embedded JS/WASM hashes match a fresh optimized gateway build and
  that the exact `workerd` package/version/integrity is locked rather than
  floating;
- build both architecture images from pinned bases/dependencies and scan the
  image, build log, layers, package lock, process arguments/environment, normal
  logs and persistent workerd state for canary Hermes, ElevenLabs, Access,
  device-token and session-scope values;
- validate every option and hard bound, paired Access fields, non-empty and
  unique per-device IDs/tokens, explicit session-map coverage, HTTPS-only Hermes,
  private-network denial by default, the explicit `allow_private_upstreams`
  path, and unconditional denial of loopback/workerd-local destinations;
- reject absolute/traversing certificate paths, symlink escape, missing/empty
  chain/key files, malformed PEM, chains outside the App's public CA bundle, and
  server certificate/key mismatch;
- prove the standard `/ssl/fullchain.pem` and `/ssl/privkey.pem` happy path,
  direct TLS health, exact WSS subprotocol/authentication, publicly trusted
  outbound Hermes TLS, fail-closed redirects, and redacted configuration errors;
- require the generated workerd config and containing tmpfs generation directory
  to be mode-restricted to the dedicated runtime account, then deleted after
  readiness, with secrets absent from exported environment, process arguments,
  application logs and persistent local-disk state;
- prove that invalid changed options stop the active runtime so old credentials
  cannot linger, while an invalid/incomplete certificate-file renewal retains
  the active validated certificate only when the normalized options are exactly
  unchanged; a simultaneous option and TLS failure must stop;
- restart workerd only after an atomic certificate/key change, load the new
  chain after the required two identical validated polls plus the startup
  budget, reconnect the device socket, and retain an idle conversation head
  across that restart;
- exercise an in-flight certificate restart after Hermes acceptance and require
  the existing ambiguous-turn safety behavior rather than automatic replay;
- prove local-disk Durable Object state, completed-response head, reset
  idempotency and 24-hour quotas survive process/App restart; and
- take a cold backup, restore it to an isolated instance, verify state integrity,
  then prove the documented source/clone credential rotation and no-concurrent-
  clone rule. Do not attempt to import Cloudflare's opaque Durable Object data.

CI now generates an SPDX JSON SBOM and runs a pinned Trivy image vulnerability
and secret scan for each native architecture build, failing on fixed
high/critical findings. Release publication must still archive/review the SBOM,
re-scan the exact published digests against the then-current advisory database,
produce a dependency/license inventory, and inspect final layers/manifests.

Because Home Assistant's secure `password` schema triggers Supervisor's
k-anonymous pwned check, documentation/PII tests must also verify disclosure of
the locally computed SHA-1 prefix behavior: only the first five hexadecimal
characters go to `api.pwnedpasswords.com`, not the clear value or full hash.
Debug-log tests must assume resolved options are sensitive and reject credential
text in any shareable output.

## Local Cloudflare smoke tests

Create `gateway/.dev.vars` from the example with test credentials, then:

```sh
cd gateway
npm run dev
```

Verify:

1. `GET /health` returns `200` only when required vars, secrets, and the Durable Object binding are present.
2. `/v2/realtime` without an upgrade, bearer, device ID, or required subprotocol fails before reaching a Durable Object.
3. An authenticated socket receives `ready`, including the expected formats, 32-frame windows, and a non-empty persisted `conversation_id`.
4. A second connection for the same device replaces the first; a different device maps to a different object.
5. Invalid JSON and malformed binary frames produce generic protocol errors without echoing input.
6. An idle `conversation.reset` receives matching `conversation.reset.done`; reconnect and repeat the same request ID and verify the UUID is unchanged. A new request ID must produce a new UUID.

`wrangler dev` is still not proof that production Cloudflare, Access, Tunnel, and provider WebSockets behave identically. Repeat the smoke suite against the deployed hostname.

## Provider contract tests

Use restricted test keys/quotas and a non-privileged Hermes profile. The automated provider Worker uses only synthetic canary credentials, transcripts, response IDs, and PCM. Live-provider validation must use non-sensitive utterances unless the test environment is explicitly approved for PII. Speech formatting cleanup is not DLP and must never be credited with preventing Hermes from speaking an ordinary text secret or other sensitive value.

### Scribe Realtime

- Send known PCM in 2,048-byte/64 ms device frames and verify it reaches Scribe before `turn.commit`.
- Verify adjacent frames are coalesced into approximately 128 ms upstream messages. A lone remainder must be carried atomically in the `commit: true` message; when there is no remainder the commit audio is empty.
- Confirm Scribe can emit mutable `partial_transcript` events during a long utterance, but the shipped gateway discards them: no `transcript.partial` is sent to the device and no Hermes request begins from one. Separately keep the reserved optional control codec/firmware-ignore path covered for forward compatibility.
- Confirm no Hermes request begins from a partial, even if it contains a complete-looking command.
- Send `turn.commit(last_seq=N)` and verify the gateway commits only after accepting input through `N`: pending coalescer PCM must be included in that `commit: true` message, while an empty audio field is used when no PCM remains.
- Confirm exactly one non-empty committed transcript starts Hermes.
- Exercise Scribe auth, quota, rate-limit, queue-overflow, chunk-size, inactivity, and mid-stream close errors.

### Hermes Responses SSE

- Verify the request includes `stream: true`, `store: true`, the committed transcript, spoken-answer instructions, deterministic `Idempotency-Key`, and the device's explicitly configured `X-Hermes-Session-Key`.
- First turn must omit `previous_response_id`; a subsequent turn must use the last completed ID.
- Confirm `conversation` is absent.
- Verify configuration fails if `HERMES_SESSION_KEYS_JSON` does not cover every `DEVICE_TOKENS_JSON` device. Separately test the deliberate `ALLOW_IMPLICIT_HERMES_CONTEXT=true` compatibility path and its `voice:<device-id>` value.
- Capture `X-Hermes-Session-Id` from the successful response headers and verify it is persisted only with a completed safe head; continuation still uses `previous_response_id`.
- Use a fixture where turn B refers to text and a harmless tool result from turn A. Prove B receives both through the stored response chain without the gateway resending history.
- Split SSE fields at every possible network boundary, include comment keepalives, and verify incremental parsing.
- Speak only `response.output_text.delta.delta`. Never speak `function_call`, `function_call_output`, status, arguments, or result content.
- Do not wait for `[DONE]`; require `response.completed` or `response.failed`.
- Run `scripts/verify-hermes-contract.py` against audited commit `5ecc07986f46463ca3096679b03a46402eb19cee` for chaining, retrieval, exact missing-response errors, and cleanup. Separately re-audit the streaming source path and record that it does not apply the non-streaming idempotency cache; the contract script deliberately does not duplicate a potentially tool-using request.

Conversation safety scenarios are release blockers:

| Scenario | Required outcome / next-turn head |
| --- | --- |
| A completes, then B completes | B |
| A completes, then B emits text and fails | A |
| A completes, then B is cancelled/disconnected | A |
| A completes, then object restarts with B journaled as `incomplete` | A retained; `conversation_ambiguous`; block new turns until explicit reset |
| A completes, then object restarts and GET reports B `completed` | B |
| First turn fails before any completion | no `previous_response_id` |
| Reconnect, device reboot, ordinary wake, or short button start after A | A; same conversation UUID |
| Barge-in cancels B after A completed | A; same conversation UUID |
| Explicit reset after A with request R | no previous ID; new conversation UUID; same session key |
| Duplicate delivery of request R | same reset UUID; do not rotate again |
| Distinct reset request S after R | another new UUID; no previous ID |
| Reset requested while turn/reset active | `conversation_busy`; no state change |
| Idle setting omitted | use 900-second boundary |
| Idle setting `0`/`off`/`none` without explicit unbounded flag | configuration failure |
| Idle setting `0`/`off`/`none` with `ALLOW_UNBOUNDED_CONVERSATION=true` | never rotate for inactivity |
| Idle threshold not yet reached | retain head and UUID |
| Idle threshold exactly reached before next turn | rotate before the turn; omit previous ID |
| Failed or cancelled accepted turn before the threshold | refresh activity at `turn.start`; retain head and UUID |
| Long successful turn completes after its start | refresh activity again at completion |
| Hermes base URL, model/route, profile ID, binding revision, or resolved session key changes | rotate before the next turn; never send the old previous ID |
| Hermes returns exact structured missing-previous error naming stored A | fail current turn without replay; rotate UUID/head; next newly spoken turn has no previous ID |
| Hermes returns generic/unrelated 404 | `hermes_failed`; retain A and UUID |
| Request was accepted but no in-flight ID was durably observed | `conversation_ambiguous`; retain A but block new turns until explicit reset |

Use a harmless fixture tool that records invocation IDs. Prove that an ambiguous turn is not automatically retried. A tool may have run even when the response head remains A.

Also prove conversation-boundary recovery:

1. Persist an in-flight B candidate for conversation UUID 1, then explicitly reset to UUID 2 before reconciliation. Recovery must not promote B into UUID 2 even if GET later reports B completed.
2. Persist a reset result but drop `conversation.reset.done`; reconnect and resend the same request ID. The acknowledgement must return UUID 2, not UUID 3.
3. Complete A and record Hermes response session ID H. A normal reply must remain in UUID 1 and advance the response head; explicit reset must clear H with A while preserving the long-term-memory key.
4. Configure idle values `59`, `31536001`, and malformed text and require health/configuration failure; accept `60`, `900`, and `31536000`. Require the explicit unbounded flag with every off spelling.
5. Persist a completed head with binding `(origin A, model A, profile A, revision A, session key A)`. Change each binding field independently and verify a new UUID/head before the next turn. Change only the API bearer at origin A and verify it does not expose the secret in storage or logs and does not by itself rotate.
6. Migrate a legacy state with no binding: the first current configuration must bind in place without discarding its valid completed head; only a later mismatch rotates.

### Multi-context TTS

- Open with `eleven_flash_v2_5`, `pcm_16000`, `auto_mode=true`, and alignment disabled.
- Feed Hermes deltas in adversarial token splits; verify the first complete phrase reaches TTS before Hermes completion.
- Decode and send first PCM before TTS final and before Hermes terminal completion for a multi-sentence fixture.
- Accept both `is_final` and `isFinal`, while requiring the expected `contextId`.
- Reframe decoded provider audio to bounded PCM frames and hold no more than 32 unacknowledged downlink frames.
- At normal completion, flush, deliver all PCM, send `tts.end`, receive final consumption-credit ACK, then send `turn.done`.
- At cancellation, close the old context and discard all later messages for that context.
- Exercise auth, quota/rate limit, malformed base64, oversized event, socket close, inactivity, and missing-final behavior.

## Buffered v1 diagnostic-route test

The diagnostic route is tested separately and must never be counted as realtime success. First verify it returns generic `404` with `DIAGNOSTIC_V1_ENABLED=false`; enable it only in an isolated test configuration:

```sh
ffmpeg -i spoken-command.wav -ar 16000 -ac 1 -c:a pcm_s16le utterance.wav

curl --fail-with-body \
  -H "Authorization: Bearer $KITCHEN_DEVICE_TOKEN" \
  -H "X-Device-Id: voice-pe-a1b2c3d4e5f6" \
  -H "Content-Type: audio/wav" \
  --data-binary @utterance.wav \
  https://voice.example.com/v1/voice \
  --output reply.pcm

ffplay -f s16le -ar 16000 -ac 1 reply.pcm
```

This isolates provider, Tunnel, Access, and Hermes configuration from realtime framing. It buffers the utterance and complete Hermes answer by design. Verify its Hermes request uses `store: false`, no `conversation`, no `previous_response_id`, and a hashed `diagnostic:<sha256>` memory scope. Repeating it must not form a Responses chain or change the Durable Object's `ready.conversation_id`/next realtime head. Disable the route again after the test.

## Latency instrumentation

Gateway telemetry uses an opaque turn ID and a per-turn monotonic elapsed offset from Workers `performance.now()`. Do not log audio, transcript text, Hermes text, tool arguments/results, credentials, or provider response bodies.

Gateway stage markers:

```text
turn_started
stt_connected
stt_session_ready
input_first_frame
input_end_hint                 # only when the optional header flag is present
input_committed
stt_final
hermes_start
hermes_headers
hermes_created
hermes_first_delta
tts_first_phrase
hermes_completed
tts_first_audio
first_output_frame_sent
tts_final
output_final_ack
turn_cancelled | turn_aborted | turn_failed
```

The gateway currently emits the stage timings above. Configure Cloudflare analytics or the load-test harness to additionally record durations and counters rather than absolute provider payloads:

- input/output bytes and frames;
- max in-flight input/output frames;
- queue high-water marks;
- ACK gaps and backpressure pauses;
- reconnect count and WebSocket close code category;
- Hermes tool count and tool-duration aggregate, without names/arguments if unnecessary;
- provider stage durations and terminal outcome;
- cancellation-to-final-old-frame interval.

The physical-test harness must collect these device-side monotonic markers. They are measurement requirements, not all currently exported as structured firmware telemetry:

```text
wake_accepted
capture_started
first_speech_sample
last_speech_sample
commit_sent
first_output_frame_received
first_pcm_enqueued
first_pcm_played_or_speaker_started
last_pcm_consumed_from_receive_ring
speaker_drained
cancel_requested
speaker_stopped
```

Cloudflare and ESP32 clocks are separate. Do not subtract cross-host timestamps as though synchronized. Compute provider/gateway intervals within the Worker clock and physical end-to-end intervals entirely on the device. For true acoustic latency, use an external rig that records a reference speech signal and speaker output on the same clock.

Report p50, p95, p99, maximum, failure count, and sample size. Separate these populations:

- short no-tool reply;
- long multi-sentence no-tool reply;
- tool-using reply;
- barge-in;
- reconnect/recovery;
- buffered v1 diagnostic route.

Initial release targets on stable Wi-Fi, using a short no-tool English fixture, are:

| Metric | Target |
| --- | ---: |
| Last speech sample → committed transcript | p95 ≤ 900 ms |
| Committed transcript → first Hermes text delta | report, no provider-independent hard limit |
| First complete Hermes phrase → first TTS PCM at gateway | p95 ≤ 750 ms |
| First gateway PCM → device speaker start | p95 ≤ 200 ms |
| End of speech → first audible reply | p50 ≤ 1.5 s; p95 ≤ 3.0 s |
| Cancel request → old reply inaudible | p95 ≤ 250 ms |
| Network restored → authenticated `ready` | p95 ≤ 5 s |

If a chosen Hermes model or provider route cannot meet the no-tool end-to-end target, publish measured results and adjust the target explicitly; do not relabel buffered playback as realtime.

## Required physical Voice PE validation

No release may claim “fully streaming,” “barge-in,” or production readiness until these pass on an actual unit.

### Home Assistant App installation

On real supported Home Assistant OS `amd64` and `aarch64` targets:

1. Add the public repository from a clean App store, install/build the App, and
   verify protection remains enabled and the declared AppArmor profile loads.
2. Configure a public-CA chain/key at `/ssl/fullchain.pem` and
   `/ssl/privkey.pem`. From an actual Voice PE network, verify DNS, SAN, complete
   chain, certificate time, WSS upgrade/authentication, and `/healthz` on the
   mapped port.
3. Renew the certificate atomically while devices are idle. Require the new
   certificate after two identical validated poll snapshots plus startup,
   bounded reconnect, unchanged idle
   conversation UUID/head, and no credential/transcript logging. Repeat during
   a harmless in-flight turn and confirm ambiguous recovery never replays it.
4. Test a public Hermes origin with private upstream access disabled. Then test
   an isolated private HTTPS Hermes hostname with explicit private access and a
   publicly trusted DNS-01 certificate. Confirm loopback remains denied and
   disabling the option removes private-range egress. With the option enabled,
   verify and record that the expansion applies to all gateway fetches and
   WebSockets, not only Hermes.
5. Run the complete streaming proof below through the App with live restricted
   Hermes/ElevenLabs accounts. Record the same p50/p95/p99 device-side latency
   and do not infer parity from Cloudflare/workerd unit tests.
6. While Hermes voice is active, play Home Assistant music and announcements
   over the encrypted Native API. Verify mixing/ducking and confirm the App uses
   no Home Assistant ingress/API/audio path.
7. Take an encrypted cold backup including the App and `ssl`, save its emergency
   kit off-device, destroy the test installation, and restore it. Verify local
   conversation/reset/quota state and provider readiness before reconnecting the
   device.
8. Restore a second isolated clone. Before any device access, change its DNS/TLS
   identity and rotate Hermes, ElevenLabs, Access, device tokens and test session
   scopes; prove the source and clone cannot both accept the same production
   identity. Exercise a Cloudflare-to-local cutover and require a visibly new
   short-term conversation.
9. Keep 8443 on an isolated LAN/VPN and prove firewall rules reject an untrusted
   segment. If a reverse proxy is used, verify WSS upgrades and streaming through
   it, then exercise handshake rate, concurrent-connection, and idle-connection
   limits without interrupting valid long-lived audio sessions.
10. Stop the App, rotate canary provider/device secrets while stopped, restart,
    and prove only the new values work. Separately inject an invalid live option
    and require the App to stop rather than keep old credentials active; inject
    an incomplete certificate-only renewal with unchanged options and require
    the active validated certificate to remain until valid files stabilize.

Until this matrix is recorded, the App remains experimental and documentation
must say that Home Assistant OS and physical Voice PE validation are outstanding.

### Microphone and endpointing

1. Capture channel 0 and inspect PCM16 for sample rate, byte order, DC offset, clipping, missing samples, and level.
2. Compare channel 0 and channel 1 in quiet, far-field, fan/music noise, and speaker-echo conditions.
3. Tune `silence_threshold` and the 500 ms starting `silence_duration` using at least 100 varied commands. Early cutoff must be below 1%; no-speech and long-tail behavior must be recorded.
4. Verify input frames leave the device continuously after `turn.ready`; delay readiness through the configured recovery/connect/session budgets and confirm the complete 30-second capture remains bounded in the 1 MiB external-PSRAM ring, then drains without losing its beginning.
5. Run 500 wake/capture/reply cycles and check heap/PSRAM high-water marks, fragmentation, callback overruns, and microphone ownership.

### Streaming proof

For a long utterance and multi-sentence answer, capture correlated device/gateway timestamps and prove all four statements:

1. Scribe receives audio before the device records the final speech sample.
2. Hermes starts immediately after the committed transcript, not after a WAV upload.
3. TTS receives the first phrase before Hermes emits `response.completed`.
4. The speaker begins before ElevenLabs emits the final TTS message.

If any statement is false, that run is not end-to-end streaming.

### Playback

1. Confirm raw 16 kHz mono PCM reaches both required hardware channels through resampler/mixer/AIC3204.
2. Complete 100 long-answer turns with zero persistent underruns, corrupt frames, tail truncations, or post-`turn.done` audio.
3. Verify the 128 KiB output ring never overflows, insertion alone does not release credit, and gateway/TTS reading pauses at the 32-frame consumption window when the speaker is deliberately slowed.
4. Test volume range, headphone insertion, speaker amp behavior, and final drain.
5. Play a long Home Assistant music stream through its independent input. Start/cancel multiple Hermes turns and verify an immediate 20 dB duck for the full turn, Hermes remains intelligible, and music restores over approximately one second without restarting or corrupting either pipeline.
6. Exercise an HA announcement while music is playing, including one that overlaps the end of a Hermes turn. Verify the independent mono announcement path works and music remains ducked until both the Hermes and announcement duck conditions have cleared.
7. Stop Home Assistant and block its Native API/media origin. Confirm Hermes wake, STT, conversation continuity, and playback remain operational.
8. Reconnect Home Assistant and press **New Hermes Conversation** while idle. Confirm one reset acknowledgement, a changed conversation UUID, unchanged long-term-memory key, and no audio/provider request. Repeat using a 2–5 second center-button hold.

### Cancellation and barge-in

1. Cancel with the center button during capture, STT commit, Hermes text, tool execution, TTS startup, and PCM playback.
2. Confirm queued old PCM is cleared and no late frame from the cancelled turn becomes audible.
3. Say the configured `okay_nabu` wake phrase during playback in quiet and noisy rooms, then speak a new command. Confirm it cancels old audio, opens a fresh turn, and XMOS AEC prevents the device's own reply from triggering wake inference or becoming the new command. Record arbitrary speech without the wake phrase as non-barge-in for the shipped configuration.
4. Meet the 250 ms p95 audible-stop target across at least 100 cancellation trials.
5. If acoustic barge-in fails, disable/mark barge-in unsupported and retain button cancellation; transport support alone is insufficient.
6. Verify a short press never triggers conversation reset, a 2–5 second hold triggers exactly one idle reset, and a ≥10 second hold retains factory-reset behavior. Attempt the medium hold during a turn and confirm it cannot race/replace the active conversation head.

### Privacy and security

1. With physical mute engaged, confirm no intelligible samples leave the device and wake inference is stopped.
2. Attempt missing, wrong, short, malformed, and cross-device bearers.
3. Confirm an invalid TLS certificate fails with no insecure fallback.
4. Confirm Access and Hermes bearer checks fail independently.
5. Confirm a Native API client without the encryption key cannot control the optional media entity, and that possessing this key grants no Worker/Hermes access.
6. Verify logs/analytics contain no transcript, spoken response, raw audio, provider body, API key, or Access secret.
7. Trigger a guarded Hermes tool. Confirm the voice path never auto-approves and conversation state advances only on completed safe turns.

## Fault injection

Inject each fault before commit, after commit, after `response.created`, during a tool, after first TTS audio, and while draining playback:

- Wi-Fi loss and high latency/jitter;
- Worker deployment/restart and device reconnection;
- Durable Object re-instantiation with an in-flight journal;
- Scribe/TTS 401, 429, provider-capacity errors, malformed JSON/base64, oversized events, and socket close;
- Hermes 401, exact missing-previous-response error, unrelated/generic 404, malformed SSE, missing terminal event, `response.failed`, and stall;
- duplicate/gapped/out-of-order sequence, wrong `first_sample`, discontinuity, reserved flags, and stale turn IDs;
- full input/output windows and deliberately withheld ACKs;
- rapid buttons, mute changes, wake during playback, and maximum-duration speech;
- dropped/reset acknowledgements, duplicate reset request IDs, accepted failed/cancelled-turn activity, default/disabled idle boundaries, binding changes, exact previous-response expiry, and ambiguous restart states.

After every fault, assert bounded memory, no automatic ambiguous replay, no promotion of an incomplete Hermes response, old-turn audio suppression, and eventual return to `ready` or a controlled reconnect.

## Current limitations to disclose

- A committed transcript is required before Hermes starts; speculative tool execution from partial STT is intentionally forbidden.
- Hermes tool time can dominate end-to-end latency.
- `/v1/responses` does not provide the Runs API's interactive approval exchange.
- Audited Hermes commit `5ecc079` does not apply its idempotency cache to streaming Responses; ambiguous post-acceptance turns require explicit conversation reset.
- Aborting SSE is best effort and cannot undo completed tool side effects.
- Output ACK means consumed from the Hermes receive ring into its fixed local staging buffer, not accepted by the downstream speaker or physically played by the DAC.
- Acoustic barge-in remains conditional on physical XMOS/AEC results.
- The buffered v1 diagnostic route is not realtime and is never an automatic retry.
- Buffered v1 is disabled by default, stateless, non-stored, and memory-isolated; it has no conversational continuity.
- Automatic conversation expiry defaults to 900 seconds. Disabling it requires an explicit unbounded-conversation flag.
- Gateway URL and device bearer are compile-time firmware settings in the current release.
