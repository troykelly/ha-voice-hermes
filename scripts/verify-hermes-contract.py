#!/usr/bin/env python3
"""Exercise the exact Hermes Responses contract used by the voice gateway."""

from __future__ import annotations

import json
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid


MAX_STREAM_BYTES = 2 * 1024 * 1024
RESPONSE_ID_RE = re.compile(r"resp_[A-Za-z0-9_-]{1,123}")
MODEL_RE = re.compile(r"[A-Za-z0-9._/:@-]{1,160}")


def required(name: str) -> str:
    value = os.environ.get(name, "").strip()
    if not value:
        raise SystemExit(f"{name} is required")
    return value


def visible_ascii(name: str, value: str, *, maximum: int = 512) -> str:
    encoded = value.encode("utf-8")
    if not 1 <= len(encoded) <= maximum or any(byte < 0x21 or byte > 0x7E for byte in encoded):
        raise SystemExit(f"{name} must contain 1-{maximum} visible ASCII bytes")
    return value


def normalized_base_url(value: str) -> str:
    parsed = urllib.parse.urlsplit(value)
    if parsed.scheme.lower() != "https" or not parsed.hostname:
        raise SystemExit("HERMES_BASE_URL must be an absolute https:// URL")
    if parsed.username is not None or parsed.password is not None:
        raise SystemExit("HERMES_BASE_URL must not contain user information")
    if parsed.query or parsed.fragment:
        raise SystemExit("HERMES_BASE_URL must not contain a query or fragment")
    if any(ord(character) < 0x21 or ord(character) > 0x7E for character in value):
        raise SystemExit("HERMES_BASE_URL must contain only visible ASCII characters")
    return value.rstrip("/")


class RejectRedirects(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise urllib.error.HTTPError(req.full_url, code, "redirect rejected", headers, fp)


BASE = normalized_base_url(required("HERMES_BASE_URL"))
KEY = visible_ascii("HERMES_API_KEY", required("HERMES_API_KEY"), maximum=4096)
MODEL = os.environ.get("HERMES_MODEL", "hermes-agent")
if not MODEL_RE.fullmatch(MODEL):
    raise SystemExit("HERMES_MODEL must match the gateway's 1-160 byte identifier syntax")
SESSION_KEY = visible_ascii(
    "HERMES_CONTRACT_SESSION_KEY",
    os.environ.get("HERMES_CONTRACT_SESSION_KEY", f"contract-test:{uuid.uuid4()}"),
    maximum=256,
)
OPENER = urllib.request.build_opener(RejectRedirects())


def request(path: str, *, method: str = "GET", body: dict | None = None):
    if not path.startswith("/"):
        raise ValueError("request path must be absolute")
    data = None if body is None else json.dumps(body, separators=(",", ":")).encode("utf-8")
    headers = {
        "Authorization": f"Bearer {KEY}",
        "X-Hermes-Session-Key": SESSION_KEY,
        "Accept": "application/json, text/event-stream",
    }
    if data is not None:
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(f"{BASE}{path}", data=data, headers=headers, method=method)
    return OPENER.open(req, timeout=90)


def read_limited(response, context: str) -> bytes:
    payload = response.read(MAX_STREAM_BYTES + 1)
    if len(payload) > MAX_STREAM_BYTES:
        raise RuntimeError(f"oversized response from {context}")
    return payload


def decode_json(payload: bytes, context: str) -> dict:
    try:
        value = json.loads(payload)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise RuntimeError(f"invalid JSON response from {context}") from error
    if not isinstance(value, dict):
        raise RuntimeError(f"non-object JSON response from {context}")
    return value


def json_request(path: str, *, method: str = "GET", body: dict | None = None):
    with request(path, method=method, body=body) as response:
        if response.headers.get_content_type().lower() != "application/json":
            raise RuntimeError(f"non-JSON media type from {path}")
        return response.status, response.headers, decode_json(read_limited(response, path), path)


def stream_response(previous_response_id: str | None = None) -> tuple[str, str]:
    body = {
        "model": MODEL,
        "input": "Reply with exactly: contract-ok",
        "instructions": "Do not call tools. Reply with exactly contract-ok.",
        "stream": True,
        "store": True,
    }
    if previous_response_id:
        body["previous_response_id"] = previous_response_id

    response_id: str | None = None
    completed = False
    terminal_seen = False
    event_types: list[str] = []
    text_deltas: list[str] = []
    consumed = 0
    with request("/v1/responses", method="POST", body=body) as response:
        if response.status != 200:
            raise RuntimeError(f"Hermes streaming Responses returned HTTP {response.status}, expected 200")
        if response.headers.get_content_type().lower() != "text/event-stream":
            raise RuntimeError("Hermes did not return an SSE response")
        session_id = response.headers.get("X-Hermes-Session-Id", "").strip()
        if not session_id:
            raise RuntimeError("Hermes omitted X-Hermes-Session-Id")
        visible_ascii("X-Hermes-Session-Id", session_id)

        event_name = "message"
        data_lines: list[str] = []
        while True:
            raw = response.readline(MAX_STREAM_BYTES - consumed + 1)
            if not raw:
                break
            consumed += len(raw)
            if consumed > MAX_STREAM_BYTES:
                raise RuntimeError("Hermes SSE exceeded the contract limit")
            try:
                line = raw.decode("utf-8").rstrip("\r\n")
            except UnicodeDecodeError as error:
                raise RuntimeError("Hermes SSE was not valid UTF-8") from error
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
            if terminal_seen:
                raise RuntimeError("Hermes emitted an event after the terminal response event")
            event = decode_json("\n".join(data_lines).encode("utf-8"), f"SSE event {event_name}")
            event_types.append(event_name)
            if event_name == "response.created":
                created_id = event.get("response", {}).get("id")
                if response_id is not None or not isinstance(created_id, str) or not RESPONSE_ID_RE.fullmatch(created_id):
                    raise RuntimeError("Hermes emitted an invalid or duplicate response.created ID")
                response_id = created_id
            elif event_name == "response.output_text.delta":
                delta = event.get("delta")
                if not isinstance(delta, str):
                    raise RuntimeError("Hermes emitted a non-string output_text delta")
                text_deltas.append(delta)
            elif event_name == "response.completed":
                envelope = event.get("response", {})
                completed_id = envelope.get("id")
                completed = envelope.get("status") == "completed"
                if not completed or response_id is None or completed_id != response_id:
                    raise RuntimeError("Hermes completed envelope did not match the created response")
                terminal_seen = True
            elif event_name in {"response.failed", "response.incomplete", "error"}:
                raise RuntimeError(f"Hermes emitted terminal event {event_name}")
            event_name = "message"
            data_lines = []

        if data_lines:
            raise RuntimeError("Hermes SSE ended with an unterminated event")

    if event_types.count("response.created") != 1 or event_types.count("response.completed") != 1:
        raise RuntimeError(f"incomplete Responses stream: events={event_types}, completed={completed}")
    if "".join(text_deltas).strip() != "contract-ok":
        raise RuntimeError("Hermes streaming text-delta contract changed")
    assert response_id is not None
    return response_id, session_id


def is_missing_previous_error(payload: dict, expected_id: str) -> bool:
    error = payload.get("error")
    if not isinstance(error, dict):
        return False
    message = error.get("message")
    if error.get("code") == "previous_response_not_found":
        return isinstance(message, str) and expected_id in message
    return (
        error.get("type") == "invalid_request_error"
        and error.get("param") is None
        and error.get("code") is None
        and message == f"Previous response not found: {expected_id}"
    )


def main() -> int:
    status, _, health = json_request("/health")
    if status != 200 or health.get("status") != "ok":
        raise RuntimeError("Hermes health check failed")

    status, _, capabilities = json_request("/v1/capabilities")
    features = capabilities.get("features")
    if not isinstance(features, dict) or status != 200 or not features.get("responses_api") or not features.get(
        "responses_streaming"
    ):
        raise RuntimeError("Hermes lacks the required streaming Responses capability")
    if features.get("session_key_header") != "X-Hermes-Session-Key":
        raise RuntimeError("Hermes does not advertise long-term-memory session keys")

    created: list[str] = []
    try:
        first, first_session = stream_response()
        created.append(first)
        status, _, stored = json_request(f"/v1/responses/{urllib.parse.quote(first, safe='')}")
        if status != 200 or stored.get("id") != first or stored.get("status") != "completed":
            raise RuntimeError("Hermes did not durably retrieve the completed response")

        second, second_session = stream_response(first)
        created.append(second)
        if first_session != second_session:
            raise RuntimeError("Hermes changed transcript session within a response chain")

        invalid = "resp_" + "0" * 28
        try:
            json_request(
                "/v1/responses",
                method="POST",
                body={"model": MODEL, "input": "contract probe", "previous_response_id": invalid, "store": True},
            )
            raise RuntimeError("Hermes accepted an unknown previous_response_id")
        except urllib.error.HTTPError as error:
            payload = decode_json(read_limited(error, "missing previous-response error"), "missing previous-response error")
            if error.code != 404 or not is_missing_previous_error(payload, invalid):
                raise RuntimeError("Hermes previous-response 404 contract changed") from error

        print(
            json.dumps(
                {
                    "status": "ok",
                    "responses_streaming": True,
                    "exact_text_deltas": True,
                    "previous_response_chain": True,
                    "stable_response_session": True,
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
                json_request(f"/v1/responses/{urllib.parse.quote(response_id, safe='')}", method="DELETE")
            except Exception:
                pass


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as error:
        print(f"Hermes contract check failed: {error}", file=sys.stderr)
        raise SystemExit(1)
