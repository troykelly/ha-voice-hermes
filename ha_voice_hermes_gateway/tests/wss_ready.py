#!/usr/bin/env python3
"""Perform the Voice PE realtime-v2 hello/ready exchange over TLS.

The probe deliberately has no command-line options: credentials therefore cannot
be placed in the process argument vector.  Configuration is read from environment
variables, a single JSON object on non-interactive stdin, or a hidden token prompt.
On success stdout contains exactly the durable conversation ID and a newline.
"""

from __future__ import annotations

import base64
import getpass
import hashlib
import json
import os
import re
import secrets
import socket
import ssl
import struct
import sys
from dataclasses import dataclass
from typing import Mapping


SUBPROTOCOL = "hermes-voice.realtime.v2"
MAX_HTTP_HEADERS = 32 * 1024
MAX_FRAME_PAYLOAD = 64 * 1024
CONVERSATION_ID = re.compile(r"^[A-Za-z0-9._-]{1,64}$")
DEVICE_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")


class ProbeError(Exception):
    """A deliberately non-sensitive probe failure."""


@dataclass(frozen=True)
class Settings:
    host: str
    port: int
    device_id: str
    token: str
    ca_file: str | None
    server_name: str
    timeout: float


def _stdin_settings() -> Mapping[str, object]:
    if sys.stdin.isatty():
        return {}
    try:
        raw = sys.stdin.buffer.read(64 * 1024 + 1)
    except OSError as exc:
        raise ProbeError("could not read probe configuration") from exc
    if len(raw) > 64 * 1024:
        raise ProbeError("probe configuration is too large")
    if not raw.strip():
        return {}
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ProbeError("stdin must contain one JSON configuration object") from exc
    if not isinstance(value, dict):
        raise ProbeError("stdin must contain one JSON configuration object")
    return value


def _setting(
    env: Mapping[str, str],
    stdin: Mapping[str, object],
    env_name: str,
    stdin_name: str,
) -> str | None:
    value: object | None = env.get(env_name)
    if value is None:
        value = stdin.get(stdin_name)
    if value is None:
        return None
    if not isinstance(value, (str, int, float)):
        raise ProbeError(f"{stdin_name} must be a string or number")
    return str(value)


def load_settings() -> Settings:
    env = os.environ
    stdin = _stdin_settings()
    host = _setting(env, stdin, "HERMES_GATEWAY_HOST", "host") or "127.0.0.1"
    port_text = _setting(env, stdin, "HERMES_GATEWAY_PORT", "port") or "8443"
    device_id = _setting(env, stdin, "HERMES_GATEWAY_DEVICE_ID", "device_id")
    token = _setting(env, stdin, "HERMES_GATEWAY_DEVICE_TOKEN", "token")
    ca_file = _setting(env, stdin, "HERMES_GATEWAY_CA_FILE", "ca_file")
    server_name = (
        _setting(env, stdin, "HERMES_GATEWAY_SERVER_NAME", "server_name") or host
    )
    timeout_text = _setting(env, stdin, "HERMES_GATEWAY_TIMEOUT", "timeout") or "10"

    if host.startswith("[") or host.endswith("]"):
        if not (
            host.startswith("[")
            and host.endswith("]")
            and host.count("[") == 1
            and host.count("]") == 1
        ):
            raise ProbeError("host is invalid")
        host = host[1:-1]

    if token is None and sys.stdin.isatty():
        token = getpass.getpass("Device token: ")
    if device_id is None:
        raise ProbeError("device_id is required")
    if token is None:
        raise ProbeError("device token is required")
    if not DEVICE_ID.fullmatch(device_id):
        raise ProbeError("device_id is invalid")
    if not (32 <= len(token) <= 512) or any(
        ord(char) < 0x21 or ord(char) > 0x7E for char in token
    ):
        raise ProbeError("device token must be 32-512 visible ASCII characters")
    if any(char in host for char in "\r\n\0") or not host:
        raise ProbeError("host is invalid")
    if any(char in server_name for char in "\r\n\0") or not server_name:
        raise ProbeError("TLS server name is invalid")
    try:
        port = int(port_text, 10)
        timeout = float(timeout_text)
    except ValueError as exc:
        raise ProbeError("port and timeout must be numeric") from exc
    if not (1 <= port <= 65535):
        raise ProbeError("port is outside the valid range")
    if not (0.1 <= timeout <= 120):
        raise ProbeError("timeout is outside the valid range")

    return Settings(host, port, device_id, token, ca_file, server_name, timeout)


def _read_exact(stream: ssl.SSLSocket, length: int) -> bytes:
    result = bytearray()
    while len(result) < length:
        chunk = stream.recv(length - len(result))
        if not chunk:
            raise ProbeError("gateway closed the connection unexpectedly")
        result.extend(chunk)
    return bytes(result)


def _read_http_response(stream: ssl.SSLSocket) -> tuple[int, dict[str, str]]:
    raw = bytearray()
    while b"\r\n\r\n" not in raw:
        chunk = stream.recv(1)
        if not chunk:
            raise ProbeError("gateway closed during the WebSocket upgrade")
        raw.extend(chunk)
        if len(raw) > MAX_HTTP_HEADERS:
            raise ProbeError("WebSocket upgrade headers are too large")
    try:
        lines = bytes(raw[:-4]).decode("iso-8859-1").split("\r\n")
        parts = lines[0].split(" ", 2)
        if len(parts) < 2 or not parts[0].startswith("HTTP/1."):
            raise ValueError("unsupported HTTP status line")
        status = int(parts[1])
    except (UnicodeDecodeError, IndexError, ValueError) as exc:
        raise ProbeError("gateway returned an invalid HTTP response") from exc
    headers: dict[str, str] = {}
    for line in lines[1:]:
        if not line or ":" not in line:
            raise ProbeError("gateway returned malformed upgrade headers")
        name, value = line.split(":", 1)
        name = name.strip().lower()
        if not name:
            raise ProbeError("gateway returned malformed upgrade headers")
        value = value.strip()
        if name in headers:
            if name in {"sec-websocket-accept", "sec-websocket-protocol"}:
                raise ProbeError("gateway returned ambiguous upgrade headers")
            headers[name] = f"{headers[name]},{value}"
        else:
            headers[name] = value
    return status, headers


def _has_token(value: str | None, expected: str) -> bool:
    return value is not None and expected.lower() in {
        token.strip().lower() for token in value.split(",")
    }


def _masked_frame(opcode: int, payload: bytes) -> bytes:
    if len(payload) > MAX_FRAME_PAYLOAD:
        raise ProbeError("outgoing WebSocket frame is too large")
    first = 0x80 | opcode
    length = len(payload)
    if length < 126:
        header = bytes((first, 0x80 | length))
    elif length <= 0xFFFF:
        header = bytes((first, 0x80 | 126)) + struct.pack("!H", length)
    else:
        header = bytes((first, 0x80 | 127)) + struct.pack("!Q", length)
    mask = secrets.token_bytes(4)
    masked = bytes(value ^ mask[index % 4] for index, value in enumerate(payload))
    return header + mask + masked


def _read_frame(stream: ssl.SSLSocket) -> tuple[int, bytes]:
    first, second = _read_exact(stream, 2)
    if first & 0x70 or not first & 0x80:
        raise ProbeError("gateway returned an unsupported fragmented WebSocket frame")
    if second & 0x80:
        raise ProbeError("gateway returned an invalid masked server frame")
    opcode = first & 0x0F
    length = second & 0x7F
    if length == 126:
        length = struct.unpack("!H", _read_exact(stream, 2))[0]
    elif length == 127:
        length = struct.unpack("!Q", _read_exact(stream, 8))[0]
        if length >> 63:
            raise ProbeError("gateway returned an invalid WebSocket length")
    if length > MAX_FRAME_PAYLOAD:
        raise ProbeError("gateway returned an oversized WebSocket frame")
    if opcode >= 8 and length > 125:
        raise ProbeError("gateway returned an oversized WebSocket control frame")
    return opcode, _read_exact(stream, length)


def probe(settings: Settings) -> str:
    context = ssl.create_default_context(cafile=settings.ca_file)
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    websocket_key = base64.b64encode(secrets.token_bytes(16)).decode("ascii")
    if ":" in settings.host and not settings.host.startswith("["):
        host_header = f"[{settings.host}]:{settings.port}"
    else:
        host_header = f"{settings.host}:{settings.port}"
    request = (
        "GET /v2/realtime HTTP/1.1\r\n"
        f"Host: {host_header}\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        f"Sec-WebSocket-Key: {websocket_key}\r\n"
        "Sec-WebSocket-Version: 13\r\n"
        f"Sec-WebSocket-Protocol: {SUBPROTOCOL}\r\n"
        f"X-Device-Id: {settings.device_id}\r\n"
        f"Authorization: Bearer {settings.token}\r\n"
        "\r\n"
    ).encode("ascii")

    try:
        with socket.create_connection(
            (settings.host, settings.port), timeout=settings.timeout
        ) as raw_socket:
            with context.wrap_socket(
                raw_socket, server_hostname=settings.server_name
            ) as tls_socket:
                tls_socket.settimeout(settings.timeout)
                tls_socket.sendall(request)
                status, headers = _read_http_response(tls_socket)
                if status != 101:
                    raise ProbeError("gateway rejected the WebSocket upgrade")
                expected_accept = base64.b64encode(
                    hashlib.sha1(
                        (websocket_key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode(
                            "ascii"
                        )
                    ).digest()
                ).decode("ascii")
                if headers.get("sec-websocket-accept") != expected_accept:
                    raise ProbeError("gateway returned an invalid WebSocket accept value")
                if not _has_token(headers.get("upgrade"), "websocket") or not _has_token(
                    headers.get("connection"), "upgrade"
                ):
                    raise ProbeError("gateway did not confirm the WebSocket upgrade")
                if headers.get("sec-websocket-protocol") != SUBPROTOCOL:
                    raise ProbeError("gateway did not select realtime protocol v2")

                hello = json.dumps(
                    {
                        "v": 2,
                        "type": "hello",
                        "firmware": "ha-voice-hermes/app-smoke-test",
                        "input": "pcm_s16le_16000_mono",
                        "output": "pcm_s16le_16000_mono",
                        "barge_in": True,
                    },
                    separators=(",", ":"),
                ).encode("utf-8")
                tls_socket.sendall(_masked_frame(0x1, hello))

                for _ in range(16):
                    opcode, payload = _read_frame(tls_socket)
                    if opcode == 0x8:
                        raise ProbeError("gateway closed before the ready message")
                    if opcode == 0x9:
                        tls_socket.sendall(_masked_frame(0xA, payload))
                        continue
                    if opcode != 0x1:
                        raise ProbeError("gateway returned an unexpected WebSocket frame")
                    try:
                        control = json.loads(payload.decode("utf-8"))
                    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
                        raise ProbeError("gateway returned malformed JSON") from exc
                    if not isinstance(control, dict):
                        raise ProbeError("gateway returned a non-object control message")
                    if control.get("type") == "error":
                        raise ProbeError("gateway returned a protocol error")
                    if control.get("type") != "ready":
                        continue
                    conversation_id = control.get("conversation_id")
                    if control.get("v") != 2 or not isinstance(conversation_id, str):
                        raise ProbeError("gateway returned an invalid ready message")
                    if not CONVERSATION_ID.fullmatch(conversation_id):
                        raise ProbeError("gateway returned an invalid conversation ID")
                    return conversation_id
                raise ProbeError("gateway did not return ready in the control-message budget")
    except (OSError, ssl.SSLError, socket.timeout) as exc:
        raise ProbeError("TLS WebSocket probe failed") from exc


def main() -> int:
    if len(sys.argv) != 1:
        print("error: this probe accepts configuration only via environment or stdin", file=sys.stderr)
        return 2
    try:
        conversation_id = probe(load_settings())
    except ProbeError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    print(conversation_id)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
