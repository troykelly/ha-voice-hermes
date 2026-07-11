#!/usr/bin/env python3
"""Exercise the exact Hermes Responses contract used by the voice gateway."""

from __future__ import annotations

import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid


MAX_STREAM_BYTES = 2 * 1024 * 1024


def required(name: str) -> str:
    value = os.environ.get(name, "").strip()
    if not value:
        raise SystemExit(f"{name} is required")
    return value


BASE = required("HERMES_BASE_URL").rstrip("/")
KEY = required("HERMES_API_KEY")
MODEL = os.environ.get("HERMES_MODEL", "hermes-agent")
SESSION_KEY = os.environ.get(
    "HERMES_CONTRACT_SESSION_KEY", f"contract-test:{uuid.uuid4()}"
)


def request(path: str, *, method: str = "GET", body: dict | None = None):
    data = None if body is None else json.dumps(body).encode("utf-8")
    headers = {
        "Authorization": f"Bearer {KEY}",
        "X-Hermes-Session-Key": SESSION_KEY,
        "Accept": "application/json, text/event-stream",
    }
    if data is not None:
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(
        f"{BASE}{path}", data=data, headers=headers, method=method
    )
    return urllib.request.urlopen(req, timeout=90)


def json_request(path: str, *, method: str = "GET", body: dict | None = None):
    with request(path, method=method, body=body) as response:
        payload = response.read(MAX_STREAM_BYTES + 1)
        if len(payload) > MAX_STREAM_BYTES:
            raise RuntimeError(f"oversized JSON response from {path}")
        return response.status, response.headers, json.loads(payload)


def stream_response(previous_response_id: str | None = None):
    body = {
        "model": MODEL,
        "input": "Reply with exactly: contract-ok",
        "instructions": "Do not call tools. Reply with exactly contract-ok.",
        "stream": True,
        "store": True,
    }
    if previous_response_id:
        body["previous_response_id"] = previous_response_id

    response_id = None
    completed = False
    event_types: list[str] = []
    consumed = 0
    with request("/v1/responses", method="POST", body=body) as response:
        if response.status != 200:
            raise RuntimeError(
                f"Hermes streaming Responses returned HTTP {response.status}, expected 200"
            )
        if not response.headers.get_content_type() == "text/event-stream":
            raise RuntimeError("Hermes did not return an SSE response")
        session_id = response.headers.get("X-Hermes-Session-Id")
        event_name = "message"
        data_lines: list[str] = []
        for raw in response:
            consumed += len(raw)
            if consumed > MAX_STREAM_BYTES:
                raise RuntimeError("Hermes SSE exceeded the contract limit")
            line = raw.decode("utf-8").rstrip("\r\n")
            if line.startswith(":"):
                continue
            if line.startswith("event:"):
                event_name = line[6:].strip()
                continue
            if line.startswith("data:"):
                data_lines.append(line[5:].lstrip())
                continue
            if line:
                continue
            if not data_lines:
                event_name = "message"
                continue
            event = json.loads("\n".join(data_lines))
            event_types.append(event_name)
            if event_name == "response.created":
                response_id = event.get("response", {}).get("id")
            elif event_name == "response.completed":
                envelope = event.get("response", {})
                completed = envelope.get("status") == "completed"
                response_id = response_id or envelope.get("id")
            elif event_name in {"response.failed", "error"}:
                raise RuntimeError(f"Hermes emitted terminal event {event_name}")
            event_name = "message"
            data_lines = []

    required_events = {"response.created", "response.completed"}
    if not required_events.issubset(event_types) or not completed or not response_id:
        raise RuntimeError(
            f"incomplete Responses stream: events={event_types}, completed={completed}"
        )
    return response_id, session_id


def main() -> int:
    status, _, health = json_request("/health")
    if status != 200 or health.get("status") != "ok":
        raise RuntimeError("Hermes health check failed")

    status, _, capabilities = json_request("/v1/capabilities")
    features = capabilities.get("features", {})
    if status != 200 or not features.get("responses_api") or not features.get(
        "responses_streaming"
    ):
        raise RuntimeError("Hermes lacks the required streaming Responses capability")
    if features.get("session_key_header") != "X-Hermes-Session-Key":
        raise RuntimeError("Hermes does not advertise long-term-memory session keys")

    created: list[str] = []
    try:
        first, first_session = stream_response()
        created.append(first)
        status, _, stored = json_request(f"/v1/responses/{first}")
        if status != 200 or stored.get("id") != first or stored.get("status") != "completed":
            raise RuntimeError("Hermes did not durably retrieve the completed response")

        second, second_session = stream_response(first)
        created.append(second)
        if first_session and second_session and first_session != second_session:
            raise RuntimeError("Hermes changed transcript session within a response chain")

        invalid = "resp_" + "0" * 28
        try:
            json_request(
                "/v1/responses",
                method="POST",
                body={
                    "model": MODEL,
                    "input": "contract probe",
                    "previous_response_id": invalid,
                    "store": True,
                },
            )
            raise RuntimeError("Hermes accepted an unknown previous_response_id")
        except urllib.error.HTTPError as error:
            payload = json.loads(error.read(MAX_STREAM_BYTES))
            message = str(payload.get("error", {}).get("message", ""))
            if error.code != 404 or not message.startswith("Previous response not found:"):
                raise RuntimeError("Hermes previous-response 404 contract changed") from error

        print(
            json.dumps(
                {
                    "status": "ok",
                    "responses_streaming": True,
                    "previous_response_chain": True,
                    "durable_retrieval": True,
                    "structured_missing_head": True,
                    "checked_at": int(time.time()),
                },
                separators=(",", ":"),
            )
        )
        return 0
    finally:
        for response_id in reversed(created):
            try:
                json_request(f"/v1/responses/{response_id}", method="DELETE")
            except Exception:
                pass


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as error:
        print(f"Hermes contract check failed: {error}", file=sys.stderr)
        raise SystemExit(1)
