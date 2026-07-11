import { env, exports } from "cloudflare:workers";
import { evictDurableObject, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";

const DEVICE_ID = "voice-pe-test";
const DEVICE_TOKEN = "test-device-token-0123456789abcdef0123456789abcdef";
const PROTOCOL = "hermes-voice.realtime.v2";
const AUDIO_HEADER_BYTES = 20;
const AUDIO_FRAME_BYTES = 2_048;
const TEST_SESSION_KEY = "agent:test:voice:device:voice-pe-test";

function timeout(ms, message) {
  return new Promise((_, reject) => {
    setTimeout(() => reject(new Error(message)), ms);
  });
}

function nextControl(socket, expectedType) {
  const message = new Promise((resolve, reject) => {
    const onMessage = (event) => {
      try {
        const control = JSON.parse(event.data);
        if (control.type !== expectedType) return;
        socket.removeEventListener("message", onMessage);
        resolve(control);
      } catch (error) {
        socket.removeEventListener("message", onMessage);
        reject(error);
      }
    };
    socket.addEventListener("message", onMessage);
  });
  return Promise.race([
    message,
    timeout(2_000, `timed out waiting for ${expectedType}`),
  ]);
}

function fetchDeviceUpgrade() {
  return exports.default.fetch("https://voice.test/v2/realtime", {
    headers: {
      Authorization: `Bearer ${DEVICE_TOKEN}`,
      Connection: "Upgrade",
      "Sec-WebSocket-Protocol": PROTOCOL,
      Upgrade: "websocket",
      "X-Device-Id": DEVICE_ID,
    },
  });
}

async function sendControl(socket, control, responseType) {
  const response = nextControl(socket, responseType);
  socket.send(JSON.stringify(control));
  return response;
}

async function openDeviceSession() {
  const response = await fetchDeviceUpgrade();
  expect(response.status).toBe(101);
  expect(response.headers.get("Sec-WebSocket-Protocol")).toBe(PROTOCOL);
  const socket = response.webSocket;
  expect(socket).toBeDefined();
  socket.accept();
  const ready = await sendControl(
    socket,
    {
      v: 2,
      type: "hello",
      firmware: "ha-voice-hermes/0.3.0",
      input: "pcm_s16le_16000_mono",
      output: "pcm_s16le_16000_mono",
      barge_in: true,
    },
    "ready",
  );
  expect(ready.v).toBe(2);
  return { ready, socket };
}

async function startCommittedTurn(socket, inbox, turnId, seed = 131) {
  const messageStart = inbox.all.length;
  socket.send(JSON.stringify({ v: 2, type: "turn.start", turn_id: turnId }));
  expect(await inbox.control("turn.ready")).toMatchObject({ turn_id: turnId });
  const input = makeBytes(AUDIO_FRAME_BYTES, seed);
  socket.send(encodeInputFrame(turnId, 0, 0, 0x01, input).buffer);
  socket.send(
    JSON.stringify({
      v: 2,
      type: "turn.commit",
      turn_id: turnId,
      last_seq: 0,
    }),
  );
  expect(await inbox.control("input.ack")).toMatchObject({
    seq: 0,
    turn_id: turnId,
  });
  return messageStart;
}

async function setProviderMode(path, mode) {
  const response = await env.PROVIDER_MOCK.fetch(
    new Request(`https://provider-control.test/${path}`, {
      body: JSON.stringify({ mode }),
      headers: { "Content-Type": "application/json" },
      method: "POST",
    }),
  );
  expect(response.ok).toBe(true);
}

async function openDevice() {
  return (await openDeviceSession()).socket;
}

function makeBytes(length, seed) {
  return Uint8Array.from({ length }, (_, index) => (index * 29 + seed) % 251);
}

function concatBytes(chunks) {
  const output = new Uint8Array(
    chunks.reduce((length, chunk) => length + chunk.length, 0),
  );
  let offset = 0;
  for (const chunk of chunks) {
    output.set(chunk, offset);
    offset += chunk.length;
  }
  return output;
}

function base64ToBytes(value) {
  const binary = atob(value);
  return Uint8Array.from(binary, (character) => character.charCodeAt(0));
}

function encodeInputFrame(turnId, sequence, firstSample, flags, pcm) {
  const frame = new Uint8Array(AUDIO_HEADER_BYTES + pcm.length);
  const view = new DataView(frame.buffer);
  const numericTurnId = BigInt(`0x${turnId}`);
  frame[0] = 2;
  frame[1] = 1;
  frame[2] = flags;
  frame[3] = AUDIO_HEADER_BYTES;
  view.setUint32(4, Number(numericTurnId >> 32n));
  view.setUint32(8, Number(numericTurnId & 0xffff_ffffn));
  view.setUint32(12, sequence);
  view.setUint32(16, firstSample);
  frame.set(pcm, AUDIO_HEADER_BYTES);
  return frame;
}

function decodeOutputFrame(bytes) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const high = BigInt(view.getUint32(4));
  const low = BigInt(view.getUint32(8));
  return {
    version: bytes[0],
    kind: bytes[1],
    flags: bytes[2],
    headerLength: bytes[3],
    turnId: `${((high << 32n) | low).toString(16).padStart(16, "0")}`,
    sequence: view.getUint32(12),
    firstSample: view.getUint32(16),
    pcm: bytes.slice(AUDIO_HEADER_BYTES),
  };
}

function socketInbox(socket) {
  const queued = [];
  const all = [];
  socket.addEventListener("message", (event) => {
    let message;
    if (typeof event.data === "string") {
      message = { control: JSON.parse(event.data), kind: "control" };
    } else if (event.data instanceof ArrayBuffer) {
      message = { bytes: new Uint8Array(event.data), kind: "binary" };
    } else if (ArrayBuffer.isView(event.data)) {
      message = {
        bytes: new Uint8Array(
          event.data.buffer,
          event.data.byteOffset,
          event.data.byteLength,
        ).slice(),
        kind: "binary",
      };
    } else {
      throw new TypeError("unexpected device WebSocket message type");
    }
    queued.push(message);
    all.push(message);
  });

  async function waitFor(predicate, label, waitMs = 8_000) {
    const deadline = Date.now() + waitMs;
    while (Date.now() < deadline) {
      const index = queued.findIndex(predicate);
      if (index !== -1) return queued.splice(index, 1)[0];
      await new Promise((resolve) => setTimeout(resolve, 2));
    }
    throw new Error(
      `timed out waiting for ${label}; queued=${JSON.stringify(
        queued.map((message) =>
          message.kind === "control" ? message.control : `binary:${message.bytes.length}`,
        ),
      )}`,
    );
  }

  return {
    all,
    control(type) {
      return waitFor(
        (message) => message.kind === "control" && message.control.type === type,
        type,
      ).then((message) => message.control);
    },
  };
}

async function runRealtimeTurn(socket, inbox, definition) {
  const messageStart = inbox.all.length;
  socket.send(
    JSON.stringify({ v: 2, type: "turn.start", turn_id: definition.turnId }),
  );
  const ready = await inbox.control("turn.ready");
  expect(ready.turn_id).toBe(definition.turnId);

  const inputFrames = [
    definition.input.slice(0, AUDIO_FRAME_BYTES),
    definition.input.slice(AUDIO_FRAME_BYTES, AUDIO_FRAME_BYTES * 2),
    definition.input.slice(AUDIO_FRAME_BYTES * 2),
  ];
  let firstSample = 0;
  for (const [sequence, pcm] of inputFrames.entries()) {
    const flags = sequence === inputFrames.length - 1 ? 0x01 : 0;
    socket.send(
      encodeInputFrame(definition.turnId, sequence, firstSample, flags, pcm).buffer,
    );
    firstSample += pcm.length / 2;
  }
  socket.send(
    JSON.stringify({
      v: 2,
      type: "turn.commit",
      turn_id: definition.turnId,
      last_seq: inputFrames.length - 1,
    }),
  );

  const inputAck = await inbox.control("input.ack");
  expect(inputAck).toMatchObject({
    seq: inputFrames.length - 1,
    turn_id: definition.turnId,
  });
  expect(await inbox.control("response.start")).toMatchObject({
    turn_id: definition.turnId,
  });
  expect(await inbox.control("tts.start")).toMatchObject({
    sample_rate: 16_000,
    turn_id: definition.turnId,
  });
  const ttsEnd = await inbox.control("tts.end");

  const turnMessages = inbox.all.slice(messageStart);
  const outputFrames = turnMessages
    .filter((message) => message.kind === "binary")
    .map((message) => decodeOutputFrame(message.bytes));
  expect(outputFrames.length).toBeGreaterThan(1);
  expect(ttsEnd.last_seq).toBe(outputFrames.length - 1);

  let expectedFirstSample = 0;
  for (const [index, frame] of outputFrames.entries()) {
    expect(frame).toMatchObject({
      firstSample: expectedFirstSample,
      flags: 0,
      headerLength: AUDIO_HEADER_BYTES,
      kind: 2,
      sequence: index,
      turnId: definition.turnId,
      version: 2,
    });
    expect(frame.pcm.length).toBeGreaterThan(0);
    expect(frame.pcm.length).toBeLessThanOrEqual(AUDIO_FRAME_BYTES);
    if (index < outputFrames.length - 1) {
      expect(frame.pcm.length).toBe(AUDIO_FRAME_BYTES);
    }
    expectedFirstSample += frame.pcm.length / 2;
  }
  expect(
    outputFrames.filter((frame) => frame.pcm.length < AUDIO_FRAME_BYTES).length,
  ).toBeLessThanOrEqual(1);
  expect(concatBytes(outputFrames.map((frame) => frame.pcm))).toEqual(
    definition.output,
  );

  socket.send(
    JSON.stringify({
      v: 2,
      type: "output.ack",
      turn_id: definition.turnId,
      seq: ttsEnd.last_seq,
    }),
  );
  expect(await inbox.control("turn.done")).toMatchObject({
    turn_id: definition.turnId,
  });

  const controls = turnMessages
    .filter((message) => message.kind === "control")
    .map((message) => message.control.type);
  expect(controls).not.toContain("transcript.partial");
  expect(controls).not.toContain("transcript.final");
}

describe("compiled Worker runtime", () => {
  it("reports a configured realtime service without exposing secrets", async () => {
    const response = await exports.default.fetch("https://voice.test/health");
    expect(response.status).toBe(200);
    expect(response.headers.get("cache-control")).toBe("no-store");
    const body = await response.json();
    expect(body).toMatchObject({
      status: "ok",
      service: "ha-voice-hermes-gateway",
      realtime: true,
    });
    expect(JSON.stringify(body)).not.toContain(DEVICE_TOKEN);
  });

  it("rejects an unauthenticated upgrade before creating a session", async () => {
    const response = await exports.default.fetch("https://voice.test/v2/realtime", {
      headers: {
        Connection: "Upgrade",
        "Sec-WebSocket-Protocol": PROTOCOL,
        Upgrade: "websocket",
        "X-Device-Id": DEVICE_ID,
      },
    });
    expect(response.status).toBe(401);
    expect(response.headers.get("www-authenticate")).toBe("Bearer");
  });

  it("rejects an unknown device even when it presents another device's token", async () => {
    const response = await exports.default.fetch("https://voice.test/v2/realtime", {
      headers: {
        Authorization: `Bearer ${DEVICE_TOKEN}`,
        Connection: "Upgrade",
        "Sec-WebSocket-Protocol": PROTOCOL,
        Upgrade: "websocket",
        "X-Device-Id": "voice-pe-unknown",
      },
    });
    expect(response.status).toBe(401);
  });

  it("does not expose the buffered diagnostic route by default", async () => {
    const response = await exports.default.fetch("https://voice.test/v1/voice", {
      method: "POST",
      headers: {
        Authorization: `Bearer ${DEVICE_TOKEN}`,
        "Content-Type": "audio/wav",
        "X-Device-Id": DEVICE_ID,
      },
      body: new Uint8Array([0]),
    });
    expect(response.status).toBe(404);
  });

  it("rejects provider redirects without forwarding credentials", async () => {
    expect(
      (
        await env.PROVIDER_MOCK.fetch(
          new Request("https://provider-control.test/reset", { method: "POST" }),
        )
      ).ok,
    ).toBe(true);
    expect(
      (
        await env.PROVIDER_MOCK.fetch(
          new Request("https://provider-control.test/redirect-stt", {
            method: "POST",
          }),
        )
      ).ok,
    ).toBe(true);

    const socket = await openDevice();
    const error = nextControl(socket, "error");
    socket.send(
      JSON.stringify({
        v: 2,
        type: "turn.start",
        turn_id: "0000000000000031",
      }),
    );
    expect(await error).toMatchObject({
      code: "stt_failed",
      fatal: false,
      turn_id: "0000000000000031",
    });

    const state = await (
      await env.PROVIDER_MOCK.fetch(
        new Request("https://provider-control.test/state"),
      )
    ).json();
    expect(state.turns[0].sttRequest.headers["xi-api-key"]).toBe(
      "test-elevenlabs-api-key",
    );
    expect(state.redirectTargets).toEqual([]);
    socket.close(1000, "test complete");
  });

  it("streams complete realtime turns through mocked STT, Hermes SSE, and TTS", async () => {
    const firstOutput = makeBytes(7_178, 37);
    const secondOutput = makeBytes(5_130, 83);
    const definitions = [
      {
        assistantText: "This is the first streamed spoken sentence. ",
        input: makeBytes(4_606, 11),
        output: firstOutput,
        providerChunks: [
          firstOutput.slice(0, 2_051),
          firstOutput.slice(2_051, 6_148),
          firstOutput.slice(6_148),
        ],
        responseId: "resp_runtime_0000000000000000000001",
        transcript: "Turn one committed transcript.",
        turnId: "0000000000000042",
      },
      {
        assistantText: "This second answer continues the conversation safely. ",
        input: makeBytes(4_606, 19),
        output: secondOutput,
        providerChunks: [
          secondOutput.slice(0, 3_073),
          secondOutput.slice(3_073, 4_098),
          secondOutput.slice(4_098),
        ],
        responseId: "resp_runtime_0000000000000000000002",
        transcript: "Turn two asks for prior context.",
        turnId: "0000000000000043",
      },
    ];
    const resetResponse = await env.PROVIDER_MOCK.fetch(
      new Request("https://provider-control.test/reset", { method: "POST" }),
    );
    expect(resetResponse.ok).toBe(true);
    const { ready, socket } = await openDeviceSession();
    const inbox = socketInbox(socket);

    await runRealtimeTurn(socket, inbox, definitions[0]);
    await runRealtimeTurn(socket, inbox, definitions[1]);

    const stateResponse = await env.PROVIDER_MOCK.fetch(
      new Request("https://provider-control.test/state"),
    );
    expect(stateResponse.ok).toBe(true);
    const providerState = await stateResponse.json();

    for (const [index, turn] of providerState.turns.entries()) {
      const definition = definitions[index];
      expect(turn.sttRequest.method).toBe("GET");
      expect(turn.sttRequest.headers["xi-api-key"]).toBe(
        "test-elevenlabs-api-key",
      );
      const sttUrl = new URL(turn.sttRequest.url);
      expect(sttUrl.searchParams.get("model_id")).toBe(
        "scribe_v2_realtime",
      );
      expect(sttUrl.searchParams.get("audio_format")).toBe("pcm_16000");
      expect(sttUrl.searchParams.get("commit_strategy")).toBe("manual");
      expect(sttUrl.searchParams.get("enable_logging")).toBe("true");

      const sttAudio = turn.sttMessages.filter(
        (message) => message.message_type === "input_audio_chunk",
      );
      expect(sttAudio).toHaveLength(2);
      expect(sttAudio.map((message) => message.commit)).toEqual([false, true]);
      expect(sttAudio.every((message) => message.sample_rate === 16_000)).toBe(
        true,
      );
      expect(
        concatBytes(
          sttAudio.map((message) => base64ToBytes(message.audio_base_64)),
        ),
      ).toEqual(definition.input);

      expect(turn.hermesRequest.method).toBe("POST");
      expect(turn.hermesRequest.headers.authorization).toBe(
        "Bearer test-hermes-api-key",
      );
      expect(turn.hermesRequest.headers["cf-access-client-id"]).toBe(
        "test-access-client-id",
      );
      expect(turn.hermesRequest.headers["cf-access-client-secret"]).toBe(
        "test-access-client-secret",
      );
      expect(turn.hermesRequest.headers["x-hermes-session-key"]).toBe(
        TEST_SESSION_KEY,
      );
      expect(turn.hermesRequest.headers.accept).toBe("text/event-stream");
      expect(turn.hermesRequest.headers["idempotency-key"]).toBe(
        `hv2-${ready.conversation_id}-${definition.turnId}`,
      );
      expect(turn.hermesRequest.body).toMatchObject({
        input: definition.transcript,
        model: "hermes-agent",
        store: true,
        stream: true,
      });
      expect(turn.hermesRequest.body.instructions.length).toBeGreaterThan(20);
      if (index === 0) {
        expect(turn.hermesRequest.body).not.toHaveProperty("previous_response_id");
      } else {
        expect(turn.hermesRequest.body.previous_response_id).toBe(
          definitions[index - 1].responseId,
        );
      }

      expect(turn.ttsRequest.method).toBe("GET");
      expect(turn.ttsRequest.headers["xi-api-key"]).toBe(
        "test-elevenlabs-api-key",
      );
      const ttsUrl = new URL(turn.ttsRequest.url);
      expect(ttsUrl.searchParams.get("model_id")).toBe(
        "eleven_flash_v2_5",
      );
      expect(ttsUrl.searchParams.get("output_format")).toBe("pcm_16000");
      expect(ttsUrl.searchParams.get("auto_mode")).toBe("true");
      expect(ttsUrl.searchParams.get("enable_logging")).toBe("true");
      expect(
        turn.ttsMessages.some(
          (message) =>
            message.context_id === definition.turnId &&
            typeof message.text === "string" &&
            message.text.includes(definition.assistantText.trim()),
        ),
      ).toBe(true);
      expect(
        providerState.order.indexOf(`${definition.turnId}:tts-first-audio`),
      ).toBeLessThan(
        providerState.order.indexOf(`${definition.turnId}:hermes-completed`),
      );
    }

    expect(
      inbox.all
        .filter((message) => message.kind === "control")
        .map((message) => message.control.type),
    ).not.toContain("transcript.final");
    socket.close(1000, "test complete");
  });

  it("rejects redirects, non-SSE success, and non-200 Hermes streams before response.start", async () => {
    const { socket } = await openDeviceSession();
    const inbox = socketInbox(socket);
    const modes = ["non-sse", "no-content", "redirect"];

    for (const [index, mode] of modes.entries()) {
      expect(
        (
          await env.PROVIDER_MOCK.fetch(
            new Request("https://provider-control.test/reset", {
              method: "POST",
            }),
          )
        ).ok,
      ).toBe(true);
      await setProviderMode("hermes-mode", mode);

      const turnId = (0x501n + BigInt(index)).toString(16).padStart(16, "0");
      const messageStart = await startCommittedTurn(
        socket,
        inbox,
        turnId,
        141 + index,
      );
      expect(await inbox.control("error")).toMatchObject({
        code: "hermes_failed",
        fatal: false,
        turn_id: turnId,
      });
      expect(
        inbox.all
          .slice(messageStart)
          .filter((message) => message.kind === "control")
          .map((message) => message.control.type),
      ).not.toContain("response.start");

      const providerState = await (
        await env.PROVIDER_MOCK.fetch(
          new Request("https://provider-control.test/state"),
        )
      ).json();
      expect(providerState.redirectTargets).toEqual([]);

      const requestId = (0x601n + BigInt(index))
        .toString(16)
        .padStart(16, "0");
      expect(
        await sendControl(
          socket,
          { v: 2, type: "conversation.reset", request_id: requestId },
          "conversation.reset.done",
        ),
      ).toMatchObject({ request_id: requestId });
    }

    socket.close(1000, "test complete");
    expect(
      (
        await env.PROVIDER_MOCK.fetch(
          new Request("https://provider-control.test/reset", { method: "POST" }),
        )
      ).ok,
    ).toBe(true);
  });

  it("keeps an authenticated device socket alive across DO eviction", async () => {
    const socket = await openDevice();
    const id = env.VOICE_SESSIONS.idFromName(DEVICE_ID);
    const stub = env.VOICE_SESSIONS.get(id);
    await evictDurableObject(stub);
    const pong = await sendControl(socket, { v: 2, type: "ping" }, "pong");
    expect(pong.v).toBe(2);
    socket.close(1000, "test complete");
  });

  it("persists reset idempotency across DO eviction", async () => {
    const socket = await openDevice();
    const reset = {
      v: 2,
      type: "conversation.reset",
      request_id: "0123456789abcdef",
    };
    const first = await sendControl(socket, reset, "conversation.reset.done");
    const id = env.VOICE_SESSIONS.idFromName(DEVICE_ID);
    const stub = env.VOICE_SESSIONS.get(id);
    await evictDurableObject(stub);
    const second = await sendControl(socket, reset, "conversation.reset.done");
    expect(second.request_id).toBe(reset.request_id);
    expect(second.conversation_id).toBe(first.conversation_id);
    socket.close(1000, "test complete");
  });

  it("replaces an older socket for the same device", async () => {
    const first = await openDevice();
    const closed = new Promise((resolve) => {
      first.addEventListener("close", resolve, { once: true });
    });
    const second = await openDevice();
    const event = await Promise.race([
      closed,
      timeout(2_000, "old device socket was not replaced"),
    ]);
    expect(event.code).toBe(4001);
    second.close(1000, "test complete");
  });

  it("keeps the durable 24-hour usage window across socket replacement", async () => {
    const first = await openDevice();
    for (let index = 0; index < 63; index += 1) {
      const pong = await sendControl(first, { v: 2, type: "ping" }, "pong");
      expect(pong.v).toBe(2);
    }

    const id = env.VOICE_SESSIONS.idFromName(DEVICE_ID);
    const stub = env.VOICE_SESSIONS.get(id);
    const before = await runInDurableObject(stub, (_instance, state) =>
      state.storage.get("usage_budget"),
    );
    expect(before).toMatchObject({ version: 1 });
    expect(before.window_started_unix_ms).toBeGreaterThan(0);
    expect(before.message_count).toBeGreaterThanOrEqual(64);

    const closed = new Promise((resolve) => {
      first.addEventListener("close", resolve, { once: true });
    });
    const second = await openDevice();
    await Promise.race([
      closed,
      timeout(2_000, "old socket was not replaced for usage test"),
    ]);
    const after = await runInDurableObject(stub, (_instance, state) =>
      state.storage.get("usage_budget"),
    );
    expect(after.window_started_unix_ms).toBe(before.window_started_unix_ms);
    expect(after.message_count).toBeGreaterThanOrEqual(before.message_count);
    second.close(1000, "test complete");
  });

  it("stops arbitrary provider chunks at the configured output PCM ceiling", async () => {
    expect(
      (
        await env.PROVIDER_MOCK.fetch(
          new Request("https://provider-control.test/reset", { method: "POST" }),
        )
      ).ok,
    ).toBe(true);
    await setProviderMode("tts-mode", "oversized");
    const { socket } = await openDeviceSession();
    const inbox = socketInbox(socket);
    const turnId = "0000000000000701";
    const messageStart = await startCommittedTurn(socket, inbox, turnId, 171);

    expect(await inbox.control("response.start")).toMatchObject({
      turn_id: turnId,
    });
    expect(await inbox.control("tts.start")).toMatchObject({ turn_id: turnId });
    expect(await inbox.control("error")).toMatchObject({
      fatal: false,
      turn_id: turnId,
    });

    const turnMessages = inbox.all.slice(messageStart);
    const outputBytes = turnMessages
      .filter((message) => message.kind === "binary")
      .reduce(
        (total, message) => total + decodeOutputFrame(message.bytes).pcm.length,
        0,
      );
    expect(outputBytes).toBeGreaterThan(0);
    expect(outputBytes).toBeLessThanOrEqual(16_000 * 2);
    const controls = turnMessages
      .filter((message) => message.kind === "control")
      .map((message) => message.control.type);
    expect(controls).not.toContain("tts.end");
    expect(controls).not.toContain("turn.done");
    socket.close(1000, "test complete");
  });

  it("rejects exhausted upgrades and charges reset and ping before side effects", async () => {
    const id = env.VOICE_SESSIONS.idFromName(DEVICE_ID);
    const stub = env.VOICE_SESSIONS.get(id);
    const original = await runInDurableObject(stub, (_instance, state) =>
      state.storage.get("usage_budget"),
    );
    const windowStarted = Math.max(1, Date.now());
    const putBudget = (messageCount) =>
      runInDurableObject(stub, async (_instance, state) => {
        await state.storage.put("usage_budget", {
          audio_bytes: 0,
          message_count: messageCount,
          turn_attempt_count: 0,
          version: 1,
          window_started_unix_ms: windowStarted,
        });
      });

    await putBudget(1_024);
    const rejected = await fetchDeviceUpgrade();
    expect(rejected.status).toBe(429);
    expect(rejected.webSocket).toBeNull();

    await putBudget(1_023);
    const resetSession = await openDeviceSession();
    const resetInbox = socketInbox(resetSession.socket);
    const resetConversation = resetSession.ready.conversation_id;
    const resetClosed = new Promise((resolve) =>
      resetSession.socket.addEventListener("close", resolve, { once: true }),
    );
    resetSession.socket.send(
      JSON.stringify({
        v: 2,
        type: "conversation.reset",
        request_id: "0000000000000801",
      }),
    );
    expect(await resetInbox.control("error")).toMatchObject({
      code: "queue_overflow",
      fatal: true,
    });
    expect(
      (
        await Promise.race([
          resetClosed,
          timeout(2_000, "quota reset socket did not close"),
        ])
      ).code,
    ).toBe(4008);
    const conversationAfterReset = await runInDurableObject(
      stub,
      (_instance, state) => state.storage.get("conversation_state"),
    );
    expect(conversationAfterReset.conversation_id).toBe(resetConversation);
    expect(conversationAfterReset.last_reset_request_id).not.toBe(
      "0000000000000801",
    );
    expect(
      resetInbox.all
        .filter((message) => message.kind === "control")
        .map((message) => message.control.type),
    ).not.toContain("conversation.reset.done");

    await putBudget(1_023);
    const pingSession = await openDeviceSession();
    const pingInbox = socketInbox(pingSession.socket);
    const pingClosed = new Promise((resolve) =>
      pingSession.socket.addEventListener("close", resolve, { once: true }),
    );
    pingSession.socket.send(JSON.stringify({ v: 2, type: "ping" }));
    expect(await pingInbox.control("error")).toMatchObject({
      code: "queue_overflow",
      fatal: true,
    });
    expect(
      (
        await Promise.race([
          pingClosed,
          timeout(2_000, "quota ping socket did not close"),
        ])
      ).code,
    ).toBe(4008);
    expect(
      pingInbox.all
        .filter((message) => message.kind === "control")
        .map((message) => message.control.type),
    ).not.toContain("pong");

    await runInDurableObject(stub, async (_instance, state) => {
      if (original === undefined) {
        await state.storage.delete("usage_budget");
      } else {
        await state.storage.put("usage_budget", original);
      }
    });
  });
});
