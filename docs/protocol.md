# Device gateway protocol

Realtime v2 is the primary device/gateway contract. Buffered HTTPS v1 remains at `/v1/voice` for explicit diagnostics; its separate named Hermes chain is deprecated for conversational use.

## Realtime v2

### WebSocket handshake

```http
GET /v2/realtime HTTP/1.1
Host: voice.example.com
Upgrade: websocket
Connection: Upgrade
Authorization: Bearer <device-token>
X-Device-Id: kitchen-voice-pe
Sec-WebSocket-Protocol: hermes-voice.realtime.v2
```

The URL must use `wss://`. The server accepts only the exact `hermes-voice.realtime.v2` subprotocol and returns it in the `101 Switching Protocols` response.

`X-Device-Id` is 1–64 ASCII characters, starts with an ASCII letter or number, and otherwise contains only letters, numbers, `.`, `_`, or `-`. It selects both the per-device credential and the per-device Durable Object. HTTP header names are case-insensitive.

Device authentication is resolved in this order:

1. When `DEVICE_TOKENS_JSON` is configured, the exact device ID must have a matching token.
2. Otherwise, `DEVICE_AUTH_TOKEN` is used.

Tokens are 16–512 bytes and may not contain ASCII controls. Independent per-device tokens are recommended.

Authentication, device-ID validation, and subprotocol validation happen before the Worker forwards the upgrade to a Durable Object. Failed handshakes do not create billable voice sessions.

### Message classes

WebSocket **text** messages are UTF-8 JSON controls. WebSocket **binary** messages contain exactly one 20-byte header followed by raw PCM16LE. A control object and a binary frame are never concatenated into one WebSocket message.

Malformed JSON, binary frames shorter than 20 bytes, and unsupported required protocol fields are protocol errors. The gateway rejects unknown client control types. A device may ignore an otherwise valid, unknown server status control so optional telemetry can be added compatibly; it must not infer audio or turn state from it. The gateway sends an `error` control when possible, then cancels the current turn or closes the socket when `fatal` is true.

### Binary audio frame

There is deliberately no magic prefix; the WebSocket subprotocol and fixed header fields identify the format.

| Offset | Size | Type | Field | Required v2 value/meaning |
| ---: | ---: | --- | --- | --- |
| 0 | 1 | `u8` | `version` | `2` |
| 1 | 1 | `u8` | `kind` | `1` input PCM device→gateway; `2` output PCM gateway→device |
| 2 | 1 | `u8` | `flags` | Bit field: `0x01` end-of-utterance hint, `0x02` discontinuity; all other bits are reserved and rejected |
| 3 | 1 | `u8` | `header_len` | `20` |
| 4 | 8 | `u64` | `turn_id` | Unsigned turn identifier, network byte order |
| 12 | 4 | `u32` | `sequence` | Network byte order; starts at `0` independently for each direction and turn |
| 16 | 4 | `u32` | `first_sample` | Network byte order; zero-based sample offset of the first payload sample in that direction/turn |
| 20 | variable | bytes | `payload` | Headerless signed PCM16 little-endian, mono, 16,000 Hz |

All multi-byte header integers use network byte order (big-endian). PCM samples remain little-endian.

Payload length must be non-zero and even. Device uplink targets 2,048 payload bytes: 1,024 samples or 64 ms at 16 kHz. The final frame may be shorter. `first_sample` starts at zero and advances by `payload_length / 2`; it detects dropped, duplicated, or misordered audio independently of `sequence`.

Normal frames set flags to zero. These two flags are defined only for microphone input; playback output sets flags to zero. `0x01` may mark the final observed microphone frame, but `turn.commit` and its `last_seq` remain the authoritative end-of-input declaration. `0x02` reports a capture discontinuity and is fatal for that turn: the gateway cancels instead of transcribing known-corrupt or gapped PCM. It never permits a receiver to synthesize missing audio. Input flags may be ORed. Values containing any bit outside `0x03` are invalid.

The receiving side validates all of the following before accepting audio:

- direction-appropriate `kind`;
- exact version, flags, and header length;
- active `turn_id`;
- expected next sequence and sample offset;
- configured payload and queue bounds;
- even PCM payload length.

Sequence wrap is not supported inside a turn. The configured maximum turn duration is far below the wrap point.

In documentation and JSON, turn IDs are fixed-width 16-character hexadecimal strings representing the same unsigned 64-bit header value, for example `"0000000000000042"`.

### Client-to-server controls

#### `hello`

The first application message after upgrade:

```json
{
  "v": 2,
  "type": "hello",
  "firmware": "ha-voice-hermes/0.2.0",
  "input": "pcm_s16le_16000_mono",
  "output": "pcm_s16le_16000_mono",
  "barge_in": true
}
```

No audio or turn control is valid until the server replies `ready`.

#### `turn.start`

```json
{"v":2,"type":"turn.start","turn_id":"0000000000000042"}
```

Creates a turn. The gateway replies `turn.ready` before accepting its first input frame. Only one turn is active per device connection.

#### `conversation.reset`

```json
{
  "v": 2,
  "type": "conversation.reset",
  "request_id": "0123456789abcdef"
}
```

Requests an explicit new conversation. `request_id` is exactly 16 lowercase hexadecimal characters, may not be all zero, and is generated once for the user action. The device retains and reuses it if delivery or acknowledgement is ambiguous; it must not generate another ID merely because the WebSocket reconnects.

The reset is valid only after `ready` and while no turn or other reset is active. The gateway durably creates a new conversation UUID, clears the completed Hermes response head/session metadata, records the request ID, and only then acknowledges it. Retrying the most recently accepted request ID is idempotent and returns the same UUID. A genuinely new user action uses a new request ID and creates another new conversation; request IDs must never be intentionally reused after a later reset.

Resetting short-term transcript/tool-chain context does not rotate the Worker's stable `X-Hermes-Session-Key` long-term-memory scope.

#### `turn.commit`

```json
{
  "v": 2,
  "type": "turn.commit",
  "turn_id": "0000000000000042",
  "last_seq": 17
}
```

Declares that input frame `last_seq` is the final microphone frame. The gateway waits until all frames through that sequence have been accepted, then sends ElevenLabs' manual realtime STT commit. If one 64 ms frame remains in the coalescing buffer it is included in that same `commit: true` provider message; otherwise the provider audio field is empty. A missing or inconsistent final sequence is an error; the gateway does not silently transcribe a truncated turn.

#### `turn.cancel`

```json
{
  "v": 2,
  "type": "turn.cancel",
  "turn_id": "0000000000000042",
  "reason": "button"
}
```

Cancels capture, STT, Hermes streaming, TTS, and queued gateway audio for the turn. Reasons are short diagnostic labels such as `button`, `mute`, `barge_in`, `timeout`, or `disconnect`; they do not grant additional authority.

Cancellation is not transactional rollback. A Hermes tool may already have produced an external side effect.

#### `output.ack`

```json
{
  "v": 2,
  "type": "output.ack",
  "turn_id": "0000000000000042",
  "seq": 9
}
```

Returns playback credit through the highest contiguous output frame consumed through `seq`. “Consumed” means every byte of that frame has left the Hermes 128 KiB network receive ring for the fixed 4 KiB local speaker-staging buffer; it does not mean the downstream speaker accepted it or the DAC made it audible. Merely inserting a WebSocket frame into the receive ring does not release credit, and only one staging buffer exists, so stalled downstream playback quickly stops further credit. The device never acknowledges an old or cancelled turn, normally coalesces progress every four consumed frames, and sends final progress after consuming the frame declared by `tts.end`. The gateway gates transmission to 32 not-yet-consumed output frames and reframes provider PCM to a 2,048-byte target even though the receiver validates an individual payload up to 8,192 bytes. This propagates local queue backpressure to the TTS reader instead of allowing a faster-than-realtime provider to fill the device indefinitely.

#### `ping`

```json
{"v":2,"type":"ping"}
```

Application keepalive/health probe. It is independent of WebSocket protocol ping frames and does not extend a turn deadline.

### Server-to-client controls

#### `ready`

```json
{
  "v": 2,
  "type": "ready",
  "input_window": 32,
  "output_window": 32,
  "frame_ms": 64,
  "input_format": "pcm_s16le_16000_mono",
  "output_format": "pcm_s16le_16000_mono",
  "conversation_id": "12345678-1234-1234-1234-123456789abc"
}
```

The connection is authenticated and application messages may begin. The v2 implementation advertises a 32-frame window in both directions, a 64 ms target microphone frame, and the Durable Object's current conversation UUID. Conversation IDs are opaque 1–64 character labels containing ASCII letters, digits, `.`, `_`, or `-`; a client may display/log the value but must not construct meaning from it. A client must reject unsupported formats rather than reinterpret them.

#### `conversation.reset.done`

```json
{
  "v": 2,
  "type": "conversation.reset.done",
  "request_id": "0123456789abcdef",
  "conversation_id": "98765432-1234-1234-1234-123456789abc"
}
```

Acknowledges that the reset identified by `request_id` is durable. The ID must match the pending device request. A repeated most-recent `conversation.reset` with that same request ID returns the same `conversation_id`, including after reconnect. The device clears its pending reset only after this matching acknowledgement.

#### `pong`

```json
{"v":2,"type":"pong"}
```

Reply to an application `ping`.

#### `turn.ready`

```json
{"v":2,"type":"turn.ready","turn_id":"0000000000000042"}
```

The gateway has installed turn state and is ready for binary input sequence zero.

#### `input.ack`

```json
{
  "v": 2,
  "type": "input.ack",
  "turn_id": "0000000000000042",
  "seq": 17
}
```

Acknowledges the highest contiguous input accepted through `seq`. An ACK never skips a gap. The firmware gates microphone transmission to at most 32 unacknowledged 2,048-byte frames (64 KiB). The gateway normally coalesces ACKs every four frames and sends final progress at commit.

#### `transcript.final`

Before the final event, the gateway may send optional, mutable telemetry:

```json
{
  "v": 2,
  "type": "transcript.partial",
  "turn_id": "0000000000000042",
  "text": "What is the weather"
}
```

`transcript.partial` may be replaced by any later partial and may be omitted entirely. It is non-authoritative, must not be treated as a committed command, and is never sent to Hermes. The v2 protocol reserves this extension, but the current gateway intentionally does not forward Scribe partials, protecting the firmware's bounded control queue.

The authoritative event is:

```json
{
  "v": 2,
  "type": "transcript.final",
  "turn_id": "0000000000000042",
  "text": "What is the weather tomorrow?"
}
```

Contains the non-empty committed Scribe transcript used as Hermes input. Optional STT partials are telemetry only and cannot trigger Hermes.

#### `response.start`

```json
{"v":2,"type":"response.start","turn_id":"0000000000000042"}
```

Hermes accepted the committed transcript and began a streaming response. It does not imply that a tool or the response will complete.

#### `tts.start`

```json
{
  "v": 2,
  "type": "tts.start",
  "turn_id": "0000000000000042",
  "sample_rate": 16000
}
```

Binary output frame sequence zero may follow. The client clears any stale playback state for this turn before accepting it.

#### `tts.end`

```json
{
  "v": 2,
  "type": "tts.end",
  "turn_id": "0000000000000042",
  "last_seq": 31
}
```

Declares the final binary output sequence. `last_seq` corresponds to the last emitted audio frame and is required. The device emits a final cumulative `output.ack` only after that frame has left the Hermes receive ring for its fixed speaker-staging buffer. After `turn.done`, it still waits for the staging buffer, downstream speaker path, and DAC to drain before considering playback complete.

#### `turn.done`

```json
{"v":2,"type":"turn.done","turn_id":"0000000000000042"}
```

The gateway has reached the terminal successful state for the turn. The device may return to wake-word-ready after its speaker has drained.

#### `error`

```json
{
  "v": 2,
  "type": "error",
  "turn_id": "0000000000000042",
  "code": "stt_failed",
  "message": "Voice transcription failed",
  "fatal": false
}
```

`turn_id`, `message`, and `fatal` are optional. `code` is stable and safe to log; `message` is generic and never contains provider bodies, transcripts, URLs, or credentials. A nonfatal error terminates the affected turn. A fatal error terminates the WebSocket.

Representative codes include:

- `protocol_error`, `unsupported_version`, `invalid_frame`, `sequence_error`;
- `turn_conflict`, `unknown_turn`, `queue_overflow`, `turn_timeout`;
- `conversation_busy`, `conversation_reset_failed`, `conversation_unavailable`, `conversation_expired`;
- `stt_failed`, `empty_transcript`, `hermes_failed`, `tts_failed`;
- `cancelled`, `configuration_error`, `internal_error`.

Clients must treat unknown codes as an error without retrying the current turn.

### Normal ordering

```text
C  hello
S  ready(conversation_id)
C  conversation.reset(request_id)       # optional, idle only
S  conversation.reset.done(request_id, conversation_id)
C  turn.start
S  turn.ready
C  binary input seq 0..N
S  cumulative input.ack; a future implementation may interleave optional transcript.partial
C  turn.commit(last_seq=N)
S  transcript.final
S  response.start
S  tts.start
S  binary output seq 0..M
C  output.ack (cumulative, may be coalesced)
S  tts.end(last_seq=M)
S  turn.done
```

Controls and binary frames can be interleaved after their ordering prerequisites. The gateway may emit status controls while upstream tools run, but it never emits output PCM before `tts.start`.

### Reconnect, retry, and idempotency

A WebSocket connection is one transport epoch. Reconnect repeats `hello` and starts a new epoch; it does not resume partially acknowledged audio. With unchanged Worker configuration it does not create a new conversation: the returned `ready.conversation_id` remains the persisted UUID. A turn interrupted by network loss is ambiguous because Hermes may already have run tools, so neither device nor gateway automatically replays it.

The per-device Durable Object retains the conversation UUID and only the previous **completed** Hermes response ID as its next-turn head. A failed or disconnected candidate response does not become conversation history. The user can speak a new turn after reconnect, continuing from the prior completed head. Reboot, wake, cancel, and barge-in follow the same rule.

An unacknowledged explicit reset is the exception to ordinary non-retry: its side effect is deliberately idempotent. Firmware may resend the same `conversation.reset.request_id` after `ready` until it receives the matching `conversation.reset.done`; it never retries an audio turn.

Idle expiry and Hermes-binding changes are server-owned boundaries checked when `turn.start` is reserved, not additional client controls. An enabled idle timeout uses the latest accepted-turn start or later completion timestamp. If the normalized Hermes base URL, configured model/route label, or resolved session key differs from the conversation's stored binding, the gateway rotates before processing that turn and omits the old `previous_response_id`. A later `ready` reports the new UUID; audio-turn ordering is unchanged.

## Buffered HTTPS v1 diagnostic compatibility route

### Request

```http
POST /v1/voice HTTP/1.1
Authorization: Bearer <device-token>
Content-Type: audio/wav
Accept: audio/pcm
X-Device-Id: kitchen-voice-pe
X-Hermes-Voice-Protocol: 1
```

The body is a complete RIFF/WAVE file with uncompressed PCM format code 1, little-endian, mono, 16,000 Hz, 16 bits/sample, 32,000 bytes/second, and block alignment 2. Additional valid RIFF chunks and padding are accepted; chunk sizes and a non-empty `data` chunk are required.

### Success

```http
HTTP/1.1 200 OK
Content-Type: audio/pcm; rate=16000; channels=1; format=s16le
Cache-Control: no-store
X-Audio-Sample-Rate: 16000
X-Audio-Channels: 1
X-Audio-Bits-Per-Sample: 16
```

The response body is headerless PCM16LE. HTTP chunk boundaries are not audio-frame boundaries. Clients support either chunked transfer or content length.

### Errors and idempotency

Errors are generic JSON with `Cache-Control: no-store`. Representative HTTP statuses are `400`, `401`, `404`, `405`, `413`, `415`, `502`, and `503`.

Buffered v1 has no durable turn idempotency key and is never an automatic fallback for an ambiguous realtime turn. It uses a separate Hermes named conversation, `voice-buffered-<device-id>`, and does not read or advance the realtime Durable Object UUID/head. Its context can continue across buffered calls but cannot be treated as semantically continuous with realtime v2. Operators choose it before a new diagnostic recording begins; interactive use is deprecated.

## Health

`GET /health` validates required Worker configuration and bindings without making paid provider requests. It returns `200` and `status: "ok"`, or `503` and `status: "degraded"`; it never enumerates missing secrets to an unauthenticated caller.
