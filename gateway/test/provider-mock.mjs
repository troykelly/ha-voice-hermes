function makeBytes(length, seed) {
  return Uint8Array.from({ length }, (_, index) => (index * 29 + seed) % 251);
}

function bytesToBase64(bytes) {
  let binary = "";
  for (let offset = 0; offset < bytes.length; offset += 8_192) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + 8_192));
  }
  return btoa(binary);
}

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, reject, resolve };
}

const firstOutput = makeBytes(7_178, 37);
const secondOutput = makeBytes(5_130, 83);
const DEFINITIONS = [
  {
    assistantText: "This is the first streamed spoken sentence. ",
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

let runtime;

function reset() {
  runtime = {
    hermesIndex: 0,
    hermesMode: "sse",
    order: [],
    redirectTargets: [],
    sttRedirect: false,
    sttIndex: 0,
    ttsIndex: 0,
    ttsMode: "normal",
    turns: DEFINITIONS.map((definition) => ({
      ...definition,
      completionGate: deferred(),
      hermesRequest: undefined,
      sttMessages: [],
      sttRequest: undefined,
      ttsMessages: [],
      ttsRequest: undefined,
    })),
  };
}

reset();

function snapshot() {
  return {
    order: runtime.order,
    redirectTargets: runtime.redirectTargets,
    turns: runtime.turns.map((turn) => ({
      hermesRequest: turn.hermesRequest,
      responseId: turn.responseId,
      sttMessages: turn.sttMessages,
      sttRequest: turn.sttRequest,
      ttsMessages: turn.ttsMessages,
      ttsRequest: turn.ttsRequest,
      turnId: turn.turnId,
    })),
  };
}

function enqueueFragmentedSse(controller, text, pattern) {
  const bytes = new TextEncoder().encode(text);
  let offset = 0;
  let patternIndex = 0;
  while (offset < bytes.length) {
    const length = pattern[patternIndex % pattern.length];
    controller.enqueue(bytes.slice(offset, offset + length));
    offset += length;
    patternIndex += 1;
  }
}

function websocketUpgrade(onProviderSocket) {
  const pair = new WebSocketPair();
  const gatewaySocket = pair[0];
  const providerSocket = pair[1];
  providerSocket.accept();
  onProviderSocket(providerSocket);
  return new Response(null, { status: 101, webSocket: gatewaySocket });
}

function requestRecord(request, url, body) {
  return {
    body,
    headers: Object.fromEntries(request.headers),
    method: request.method,
    url: url.toString(),
  };
}

function mockStt(request, url) {
  const turn = runtime.turns[runtime.sttIndex++];
  if (!turn) throw new Error("unexpected extra STT connection");
  turn.sttRequest = requestRecord(request, url);
  if (runtime.sttRedirect) {
    return new Response(null, {
      headers: { Location: "https://redirect-target.test/capture" },
      status: 302,
    });
  }
  return websocketUpgrade((providerSocket) => {
    providerSocket.addEventListener("message", (event) => {
      const message = JSON.parse(event.data);
      turn.sttMessages.push(message);
      if (message.commit) {
        providerSocket.send(
          JSON.stringify({
            message_type: "partial_transcript",
            text: "mutable partial must remain private",
          }),
        );
        providerSocket.send(
          JSON.stringify({
            message_type: "committed_transcript",
            text: turn.transcript,
          }),
        );
      }
    });
    setTimeout(() => {
      providerSocket.send(JSON.stringify({ message_type: "session_started" }));
    }, 0);
  });
}

function mockTts(request, url) {
  const turn = runtime.turns[runtime.ttsIndex++];
  if (!turn) throw new Error("unexpected extra TTS connection");
  turn.ttsRequest = requestRecord(request, url);
  return websocketUpgrade((providerSocket) => {
    let firstAudioSent = false;
    providerSocket.addEventListener("message", (event) => {
      const message = JSON.parse(event.data);
      turn.ttsMessages.push(message);
      if (
        !firstAudioSent &&
        typeof message.text === "string" &&
        message.text.trim().length > 0
      ) {
        firstAudioSent = true;
        runtime.order.push(`${turn.turnId}:tts-first-audio`);
        if (runtime.ttsMode === "oversized") {
          for (const chunk of [makeBytes(2_051, 101), makeBytes(29_949, 103), makeBytes(2, 107)]) {
            providerSocket.send(
              JSON.stringify({
                audio: bytesToBase64(chunk),
                contextId: message.context_id,
                is_final: false,
              }),
            );
          }
          return;
        }
        providerSocket.send(
          JSON.stringify({
            audio: bytesToBase64(turn.providerChunks[0]),
            contextId: turn.turnId,
            is_final: false,
          }),
        );
        turn.completionGate.resolve();
      }
      if (message.flush && runtime.ttsMode === "normal") {
        for (const chunk of turn.providerChunks.slice(1)) {
          providerSocket.send(
            JSON.stringify({
              audio: bytesToBase64(chunk),
              contextId: turn.turnId,
              is_final: false,
            }),
          );
        }
        providerSocket.send(
          JSON.stringify({ contextId: turn.turnId, isFinal: true }),
        );
      }
      if (message.close_socket) {
        providerSocket.close(1000, "mock TTS complete");
      }
    });
  });
}

async function mockHermes(request, url) {
  const turn = runtime.turns[runtime.hermesIndex++];
  if (!turn) throw new Error("unexpected extra Hermes request");
  turn.hermesRequest = requestRecord(request, url, await request.json());
  if (runtime.hermesMode === "redirect") {
    return new Response(null, {
      headers: { Location: "https://redirect-target.test/capture" },
      status: 302,
    });
  }
  if (runtime.hermesMode === "no-content") {
    return new Response(null, { status: 204 });
  }
  if (runtime.hermesMode === "non-sse") {
    return new Response("not an event stream", {
      headers: { "Content-Type": "text/plain" },
      status: 200,
    });
  }
  const initial =
    `event: response.created\ndata: ${JSON.stringify({ response: { id: turn.responseId } })}\n\n` +
    `event: response.output_text.delta\ndata: ${JSON.stringify({ delta: turn.assistantText })}\n\n`;
  const completed = `event: response.completed\ndata: ${JSON.stringify({ response: { id: turn.responseId, status: "completed" } })}\n\n`;
  const body = new ReadableStream({
    start(controller) {
      queueMicrotask(() => {
        enqueueFragmentedSse(controller, initial, [1, 7, 2, 13, 3, 5, 11]);
        turn.completionGate.promise.then(
          () => {
            runtime.order.push(`${turn.turnId}:hermes-completed`);
            enqueueFragmentedSse(controller, completed, [3, 1, 17, 4, 9]);
            controller.close();
          },
          (error) => controller.error(error),
        );
      });
    },
  });
  return new Response(body, {
    headers: {
      "Content-Type": "text/event-stream",
      "X-Hermes-Session-Id": "runtime-session-chain",
    },
    status: 200,
  });
}

export async function handleProviderRequest(request) {
  const url = new URL(request.url);
  if (url.hostname === "provider-control.test") {
    if (url.pathname === "/reset" && request.method === "POST") {
      reset();
      return Response.json({ status: "ok" });
    }
    if (url.pathname === "/state" && request.method === "GET") {
      return Response.json(snapshot());
    }
    if (url.pathname === "/redirect-stt" && request.method === "POST") {
      runtime.sttRedirect = true;
      return Response.json({ status: "ok" });
    }
    if (url.pathname === "/hermes-mode" && request.method === "POST") {
      const { mode } = await request.json();
      if (!["sse", "redirect", "no-content", "non-sse"].includes(mode)) {
        return new Response("invalid Hermes mode", { status: 400 });
      }
      runtime.hermesMode = mode;
      return Response.json({ status: "ok" });
    }
    if (url.pathname === "/tts-mode" && request.method === "POST") {
      const { mode } = await request.json();
      if (!["normal", "oversized"].includes(mode)) {
        return new Response("invalid TTS mode", { status: 400 });
      }
      runtime.ttsMode = mode;
      return Response.json({ status: "ok" });
    }
    return new Response("Not found", { status: 404 });
  }
  if (
    url.hostname === "api.elevenlabs.io" &&
    url.pathname.endsWith("/speech-to-text/realtime")
  ) {
    return mockStt(request, url);
  }
  if (
    url.hostname === "api.elevenlabs.io" &&
    url.pathname.includes("/multi-stream-input")
  ) {
    return mockTts(request, url);
  }
  if (
    url.hostname === "hermes.example.com" &&
    url.pathname === "/v1/responses"
  ) {
    return mockHermes(request, url);
  }
  runtime.redirectTargets.push(requestRecord(request, url));
  return new Response("unexpected outbound request", { status: 502 });
}

export default { fetch: handleProviderRequest };
