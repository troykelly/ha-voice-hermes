#!/usr/bin/env bash
# End-to-end TLS, realtime-v2 persistence, and secret-containment checks for
# the Home Assistant App image. The sole argument is an already-built image.

set -Eeuo pipefail
IFS=$'\n\t'
umask 077

readonly CONTAINER_PORT="${APP_CONTAINER_PORT:-8443}"
readonly HEALTH_PATH="${APP_HEALTH_PATH:-/healthz}"
readonly START_TIMEOUT="${APP_START_TIMEOUT:-90}"
readonly FAILURE_TIMEOUT="${APP_FAILURE_TIMEOUT:-15}"
readonly DOCKER="${DOCKER_BIN:-docker}"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIR
readonly WSS_PROBE="${SCRIPT_DIR}/wss_ready.py"

fail() {
  printf 'FAIL: %s\n' "$1" >&2
  exit 1
}

note() {
  printf 'ok: %s\n' "$1"
}

if [[ $# -ne 1 || -z ${1:-} || $1 == -* ]]; then
  printf 'usage: %s IMAGE_TAG\n' "${0##*/}" >&2
  exit 2
fi
readonly IMAGE="$1"

[[ $CONTAINER_PORT =~ ^[0-9]{1,5}$ ]] || fail "container port must be numeric"
((10#$CONTAINER_PORT >= 1 && 10#$CONTAINER_PORT <= 65535)) \
  || fail "container port is outside the valid range"
[[ $START_TIMEOUT =~ ^[1-9][0-9]{0,3}$ ]] || fail "start timeout is invalid"
[[ $FAILURE_TIMEOUT =~ ^[1-9][0-9]{0,2}$ ]] || fail "failure timeout is invalid"
[[ $HEALTH_PATH == /* && $HEALTH_PATH != *$'\r'* && $HEALTH_PATH != *$'\n'* ]] \
  || fail "health path is invalid"

for command in "$DOCKER" curl grep openssl python3; do
  command -v "$command" >/dev/null 2>&1 || fail "required command is unavailable"
done
[[ -f $WSS_PROBE ]] || fail "the TLS WebSocket probe is missing"
"$DOCKER" image inspect "$IMAGE" >/dev/null 2>&1 || fail "image is not available locally"

TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/ha-voice-hermes-app.XXXXXXXX")"
readonly TEST_ROOT
readonly DATA_DIR="${TEST_ROOT}/data"
readonly SSL_DIR="${TEST_ROOT}/ssl"
readonly PKI_DIR="${TEST_ROOT}/pki"
readonly ARTIFACT_DIR="${TEST_ROOT}/artifacts"
readonly SECRETS_FILE="${TEST_ROOT}/secret-patterns"
readonly PROBE_INPUT="${TEST_ROOT}/probe.json"
readonly HEALTH_OUTPUT="${ARTIFACT_DIR}/health.json"
containers=()

cleanup() {
  local name
  for name in "${containers[@]}"; do
    "$DOCKER" rm --force "$name" >/dev/null 2>&1 || true
  done
  rm -rf -- "$TEST_ROOT"
}
trap cleanup EXIT
trap 'exit 130' HUP INT TERM

mkdir -p -- "$DATA_DIR" "$SSL_DIR" "$PKI_DIR" "$ARTIFACT_DIR"
# Docker Desktop bind mounts preserve these host modes. /data is a writable
# Supervisor-owned volume in production, so let the image create its isolated
# workerd state directory while keeping options.json itself mode 0600.
chmod 0777 "$DATA_DIR"
chmod 0755 "$SSL_DIR"

generate_ca() {
  openssl req -x509 -newkey rsa:2048 -nodes -sha256 -days 2 \
    -subj '/CN=HA Voice Hermes smoke-test CA' \
    -addext 'basicConstraints=critical,CA:TRUE' \
    -addext 'keyUsage=critical,keyCertSign,cRLSign' \
    -addext 'subjectKeyIdentifier=hash' \
    -keyout "${PKI_DIR}/ca.key" -out "${PKI_DIR}/ca.crt" \
    >/dev/null 2>&1
}

generate_server_certificate() {
  local generation="$1"
  local key="${PKI_DIR}/server-${generation}.key"
  local request="${PKI_DIR}/server-${generation}.csr"
  local certificate="${PKI_DIR}/server-${generation}.crt"
  local extensions="${PKI_DIR}/server-${generation}.ext"

  printf '%s\n' \
    'basicConstraints=critical,CA:FALSE' \
    'keyUsage=critical,digitalSignature,keyEncipherment' \
    'extendedKeyUsage=serverAuth' \
    'subjectAltName=DNS:localhost,IP:127.0.0.1' >"$extensions"
  openssl req -new -newkey rsa:2048 -nodes -sha256 \
    -subj '/CN=localhost' -keyout "$key" -out "$request" \
    >/dev/null 2>&1
  if [[ -f ${PKI_DIR}/ca.srl ]]; then
    openssl x509 -req -sha256 -days 2 -in "$request" \
      -CA "${PKI_DIR}/ca.crt" -CAkey "${PKI_DIR}/ca.key" \
      -CAserial "${PKI_DIR}/ca.srl" -extfile "$extensions" \
      -out "$certificate" >/dev/null 2>&1
  else
    openssl x509 -req -sha256 -days 2 -in "$request" \
      -CA "${PKI_DIR}/ca.crt" -CAkey "${PKI_DIR}/ca.key" \
      -CAcreateserial -extfile "$extensions" \
      -out "$certificate" >/dev/null 2>&1
  fi
  chmod 0600 "$key"
}

install_server_certificate() {
  local generation="$1"
  local next_chain="${SSL_DIR}/.fullchain.pem.next"
  local next_key="${SSL_DIR}/.privkey.pem.next"

  {
    command cat "${PKI_DIR}/server-${generation}.crt"
    command cat "${PKI_DIR}/ca.crt"
  } >"$next_chain"
  cp -- "${PKI_DIR}/server-${generation}.key" "$next_key"
  chmod 0644 "$next_chain"
  chmod 0600 "$next_key"
  mv -f -- "$next_chain" "${SSL_DIR}/fullchain.pem"
  mv -f -- "$next_key" "${SSL_DIR}/privkey.pem"
}

generate_test_options() {
  python3 - "$DATA_DIR" "$SECRETS_FILE" "$PROBE_INPUT" <<'PY'
import json
import os
import pathlib
import secrets
import sys

data_dir, patterns_path, probe_path = map(pathlib.Path, sys.argv[1:])
device_id = "voice-pe-smoke-test"
device_token = "device_" + secrets.token_urlsafe(48)
hermes_key = "hermes_" + secrets.token_urlsafe(48)
elevenlabs_key = "eleven_" + secrets.token_urlsafe(48)
session_key = "session_" + secrets.token_urlsafe(40)

options = {
    "certfile": "fullchain.pem",
    "keyfile": "privkey.pem",
    "certificate_poll_seconds": 5,
    "allow_private_upstreams": False,
    "hermes_base_url": "https://hermes.example.invalid",
    "hermes_api_key": hermes_key,
    "hermes_model": "hermes-agent",
    "hermes_profile_id": "voice-safe-smoke-test",
    "hermes_binding_revision": "smoke-v1",
    "elevenlabs_api_key": elevenlabs_key,
    "elevenlabs_voice_id": "smoke-test-voice",
    "realtime_stt_model": "scribe_v2_realtime",
    "tts_model": "eleven_flash_v2_5",
    "elevenlabs_enable_logging": False,
    "conversation_idle_seconds": 900,
    "realtime_max_audio_seconds": 30,
    "realtime_max_output_seconds": 30,
    "max_connection_audio_seconds": 900,
    "max_turns_per_connection": 256,
    "max_messages_per_connection": 16384,
    "diagnostic_v1_enabled": False,
    "devices": [{
        "device_id": device_id,
        "device_token": device_token,
        "hermes_session_key": session_key,
    }],
}

def write_private(path: pathlib.Path, value: str) -> None:
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as output:
        output.write(value)

write_private(data_dir / "options.json", json.dumps(options, separators=(",", ":")))
write_private(patterns_path, "\n".join((device_token, hermes_key, elevenlabs_key, session_key)) + "\n")
write_private(probe_path, json.dumps({"device_id": device_id, "token": device_token}, separators=(",", ":")))
PY
}

generate_ca
cp -- "${PKI_DIR}/ca.crt" "${PKI_DIR}/ca-bundle.pem"
generate_server_certificate 1
install_server_certificate 1
generate_test_options
SUPERVISOR_CANARY="supervisor_$(openssl rand -hex 32)"
readonly SUPERVISOR_CANARY

contains_secret() {
  local path="$1"
  [[ -f $path ]] || return 1
  LC_ALL=C grep -aF -q -f "$SECRETS_FILE" "$path"
}

assert_secret_free() {
  local path="$1"
  local label="$2"
  if contains_secret "$path"; then
    fail "$label contains secret material"
  fi
}

container_running() {
  [[ $("$DOCKER" inspect --format '{{.State.Running}}' "$1" 2>/dev/null || true) == true ]]
}

emit_sanitized_logs() {
  local name="$1"
  local raw="${ARTIFACT_DIR}/${name}.startup-failure.logs"
  "$DOCKER" logs "$name" >"$raw" 2>&1 || true
  python3 - "$raw" "$SECRETS_FILE" <<'PY' >&2
import pathlib
import sys

log = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8", errors="replace")
patterns = pathlib.Path(sys.argv[2]).read_text(encoding="utf-8").splitlines()
for pattern in patterns:
    if pattern:
        log = log.replace(pattern, "[REDACTED]")
lines = log.splitlines()
print("sanitized App startup log:")
print("\n".join(lines[-80:]))
PY
}

published_port() {
  local name="$1"
  local mapping=""
  local attempt
  for ((attempt = 0; attempt < 50; attempt++)); do
    mapping=$("$DOCKER" port "$name" "${CONTAINER_PORT}/tcp" 2>/dev/null | head -n 1 || true)
    [[ -n $mapping ]] && break
    sleep 0.1
  done
  [[ $mapping == *:* ]] || return 1
  printf '%s\n' "${mapping##*:}"
}

start_good_container() {
  local name="$1"
  local cid
  cid=$("$DOCKER" run --detach --name "$name" \
    --publish "127.0.0.1::${CONTAINER_PORT}" \
    --env "SUPERVISOR_TOKEN=${SUPERVISOR_CANARY}" \
    --tmpfs /tmp:rw,nosuid,nodev,noexec \
    --volume "${DATA_DIR}:/data" \
    --volume "${PKI_DIR}/ca-bundle.pem:/etc/ssl/certs/ca-certificates.crt:ro" \
    --volume "${SSL_DIR}:/ssl:ro" \
    "$IMAGE") || fail "could not start App container"
  [[ -n $cid ]] || fail "Docker did not return a container ID"
  containers+=("$name")
}

health_once() {
  local port="$1"
  local output="$2"
  curl --fail --silent --show-error --noproxy '*' \
    --cacert "${PKI_DIR}/ca.crt" \
    --resolve "localhost:${port}:127.0.0.1" \
    "https://localhost:${port}${HEALTH_PATH}" \
    --output "$output" 2>/dev/null || return 1
  python3 - "$output" <<'PY' >/dev/null 2>&1
import json
import pathlib
import sys

value = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
if value != {"status": "ok"}:
    raise SystemExit(1)
PY
}

wait_for_health() {
  local name="$1"
  local port="$2"
  local output="$3"
  local deadline=$((SECONDS + START_TIMEOUT))
  while ((SECONDS < deadline)); do
    if ! container_running "$name"; then
      emit_sanitized_logs "$name"
      fail "App stopped before becoming healthy"
    fi
    health_once "$port" "$output" && return 0
    sleep 1
  done
  emit_sanitized_logs "$name"
  fail "App did not become healthy before the timeout"
}

wait_for_runtime_cleanup() {
  local name="$1"
  local deadline=$((SECONDS + 10))
  local consecutive_empty=0
  local found
  while ((SECONDS <= deadline)); do
    found=$("$DOCKER" exec --user 0 "$name" sh -c \
      'find /tmp/ha-voice-hermes -xdev -type f -print -quit 2>/dev/null' || true)
    if [[ -z $found ]]; then
      consecutive_empty=$((consecutive_empty + 1))
      ((consecutive_empty >= 2)) && return 0
    else
      consecutive_empty=0
    fi
    sleep 0.1
  done
  fail "ephemeral runtime configuration was not removed"
}

probe_conversation() {
  local port="$1"
  HERMES_GATEWAY_HOST=127.0.0.1 \
  HERMES_GATEWAY_PORT="$port" \
  HERMES_GATEWAY_SERVER_NAME=localhost \
  HERMES_GATEWAY_CA_FILE="${PKI_DIR}/ca.crt" \
  HERMES_GATEWAY_TIMEOUT=15 \
    python3 "$WSS_PROBE" <"$PROBE_INPUT"
}

peer_certificate_fingerprint() {
  local port="$1"
  python3 - "$port" "${PKI_DIR}/ca.crt" <<'PY'
import hashlib
import socket
import ssl
import sys

port = int(sys.argv[1])
context = ssl.create_default_context(cafile=sys.argv[2])
context.minimum_version = ssl.TLSVersion.TLSv1_2
with socket.create_connection(("127.0.0.1", port), timeout=5) as raw:
    with context.wrap_socket(raw, server_hostname="localhost") as tls:
        certificate = tls.getpeercert(binary_form=True)
print(hashlib.sha256(certificate).hexdigest())
PY
}

installed_certificate_fingerprint() {
  openssl x509 -in "${SSL_DIR}/fullchain.pem" -outform DER 2>/dev/null \
    | openssl dgst -sha256 -binary 2>/dev/null \
    | openssl base64 -A
}

served_certificate_fingerprint_b64() {
  local port="$1"
  peer_certificate_fingerprint "$port" \
    | python3 -c 'import base64,sys; print(base64.b64encode(bytes.fromhex(sys.stdin.read().strip())).decode())'
}

assert_dedicated_workerd_uid() {
  local name="$1"
  local report="${ARTIFACT_DIR}/${name}.workerd-uids"
  # This is a literal script for the container; host-side expansion would be a
  # bug, especially during the secret-containment audit.
  # shellcheck disable=SC2016
  "$DOCKER" exec --user 0 "$name" sh -c '
    awk -F: '\''{ print "account=" $1 ":" $3 }'\'' /etc/passwd
    for proc in /proc/[0-9]*; do
      [ -r "$proc/comm" ] || continue
      [ "$(cat "$proc/comm" 2>/dev/null)" = workerd ] || continue
      awk '\''/^Uid:/ { print "actual=" $2 }'\'' "$proc/status"
    done
  ' >"$report" 2>/dev/null || fail "workerd does not have a dedicated account"

  python3 - "$report" <<'PY' >/dev/null || fail "workerd is not running under its dedicated non-root UID"
import pathlib
import sys

lines = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8").splitlines()
actual = {line.split("=", 1)[1] for line in lines if line.startswith("actual=")}
if len(actual) != 1 or "0" in actual:
    raise SystemExit(1)
uid = next(iter(actual))
accounts = [
    line.split("=", 1)[1].rsplit(":", 1)[0]
    for line in lines
    if line.startswith("account=") and line.rsplit(":", 1)[-1] == uid
]
if len(accounts) != 1 or accounts[0] in {"root", "nobody"}:
    raise SystemExit(1)
PY
}

snapshot_proc() {
  local name="$1"
  local output="$2"
  # shellcheck disable=SC2016
  "$DOCKER" exec --user 0 "$name" sh -c '
    for path in /proc/[0-9]*/cmdline /proc/[0-9]*/environ; do
      [ -r "$path" ] || continue
      printf "FILE %s\n" "$path"
      tr "\000" "\n" <"$path" 2>/dev/null || true
    done
  ' >"$output" 2>/dev/null
}

assert_gateway_supervisor_environment_is_clean() {
  local name="$1"
  local output="${ARTIFACT_DIR}/${name}.gateway-supervisor-environment"
  # The container init necessarily inherits Supervisor-provided variables.
  # The gateway supervisor itself must cross an explicit empty-environment
  # boundary before it begins processing credentials.
  # shellcheck disable=SC2016
  "$DOCKER" exec --user 0 "$name" sh -c '
    found=0
    needle=/usr/local/bin/gateway-
    needle="${needle}supervisor"
    for proc in /proc/[0-9]*; do
      [ -r "$proc/cmdline" ] || continue
      command_line=$(tr "\000" " " <"$proc/cmdline" 2>/dev/null || true)
      case "$command_line" in
        *"$needle"*)
          found=1
          printf "PROCESS_FOUND\n"
          tr "\000" "\n" <"$proc/environ" 2>/dev/null || true
          ;;
      esac
    done
    [ "$found" -eq 1 ]
  ' >"$output" 2>/dev/null || fail "gateway supervisor process was not found"
  if LC_ALL=C grep -aFq -- "$SUPERVISOR_CANARY" "$output"; then
    fail "gateway supervisor retained the injected Supervisor token"
  fi
}

snapshot_runtime_files() {
  local name="$1"
  local output="$2"
  # shellcheck disable=SC2016
  "$DOCKER" exec --user 0 "$name" sh -c '
    find /run /tmp -xdev -type f -exec sh -c '\''
      for path do
        printf "FILE %s\n" "$path"
        cat "$path" 2>/dev/null || true
      done
    '\'' sh {} + 2>/dev/null
  ' >"$output" 2>/dev/null
}

snapshot_workerd_state() {
  local name="$1"
  local output="$2"
  # shellcheck disable=SC2016
  "$DOCKER" exec --user 0 "$name" sh -c '
    find /data -xdev -type f ! -path /data/options.json -exec sh -c '\''
      for path do
        printf "FILE %s\n" "$path"
        cat "$path" 2>/dev/null || true
      done
    '\'' sh {} + 2>/dev/null
  ' >"$output" 2>/dev/null
}

audit_running_container() {
  local name="$1"
  local health_output="$2"
  local logs="${ARTIFACT_DIR}/${name}.logs"
  local proc_snapshot="${ARTIFACT_DIR}/${name}.proc"
  local runtime_snapshot="${ARTIFACT_DIR}/${name}.runtime"
  local state_snapshot="${ARTIFACT_DIR}/${name}.state"

  "$DOCKER" logs "$name" >"$logs" 2>&1 || true
  snapshot_proc "$name" "$proc_snapshot"
  snapshot_runtime_files "$name" "$runtime_snapshot"
  snapshot_workerd_state "$name" "$state_snapshot"
  assert_dedicated_workerd_uid "$name"
  assert_gateway_supervisor_environment_is_clean "$name"

  assert_secret_free "$logs" "App logs"
  assert_secret_free "$health_output" "health response"
  assert_secret_free "$proc_snapshot" "process environment or arguments"
  assert_secret_free "$runtime_snapshot" "runtime temporary files"
  assert_secret_free "$state_snapshot" "workerd persistent state"
}

audit_stopped_logs() {
  local name="$1"
  local label="$2"
  local logs="${ARTIFACT_DIR}/${name}.stopped.logs"
  "$DOCKER" logs "$name" >"$logs" 2>&1 || true
  assert_secret_free "$logs" "$label shutdown logs"
}

readonly PRIMARY_NAME="hvh-smoke-${RANDOM}-$$"
start_good_container "$PRIMARY_NAME"
PRIMARY_PORT=$(published_port "$PRIMARY_NAME") || fail "could not discover the published TLS port"
readonly PRIMARY_PORT
wait_for_health "$PRIMARY_NAME" "$PRIMARY_PORT" "$HEALTH_OUTPUT"
wait_for_runtime_cleanup "$PRIMARY_NAME"
if ! FIRST_CONVERSATION=$(probe_conversation "$PRIMARY_PORT"); then
  emit_sanitized_logs "$PRIMARY_NAME"
  fail "authenticated realtime-v2 hello/ready failed"
fi
[[ $FIRST_CONVERSATION =~ ^[A-Za-z0-9._-]{1,64}$ ]] || fail "probe returned an invalid conversation ID"
audit_running_container "$PRIMARY_NAME" "$HEALTH_OUTPUT"
note "TLS health, WSS authentication, non-root UID, and secret-containment checks passed"

# Exercise the launcher's certificate watcher. Both files are atomically replaced
# under the existing read-only-in-container /ssl bind mount. The local DO state
# and durable conversation must survive the deliberate workerd restart.
OLD_CERTIFICATE=$(served_certificate_fingerprint_b64 "$PRIMARY_PORT")
generate_server_certificate 2
install_server_certificate 2
NEW_CERTIFICATE=$(installed_certificate_fingerprint)
[[ $OLD_CERTIFICATE != "$NEW_CERTIFICATE" ]] || fail "rotation generated the same certificate fingerprint"

rotation_deadline=$((SECONDS + START_TIMEOUT))
served=""
while ((SECONDS < rotation_deadline)); do
  container_running "$PRIMARY_NAME" || fail "App stopped during certificate rotation"
  served=$(served_certificate_fingerprint_b64 "$PRIMARY_PORT" 2>/dev/null || true)
  [[ $served == "$NEW_CERTIFICATE" ]] && break
  sleep 1
done
[[ $served == "$NEW_CERTIFICATE" ]] || fail "rotated certificate was not served before the timeout"
wait_for_health "$PRIMARY_NAME" "$PRIMARY_PORT" "$HEALTH_OUTPUT"
wait_for_runtime_cleanup "$PRIMARY_NAME"
if ! ROTATED_CONVERSATION=$(probe_conversation "$PRIMARY_PORT"); then
  emit_sanitized_logs "$PRIMARY_NAME"
  fail "WSS reconnect after certificate rotation failed"
fi
[[ $ROTATED_CONVERSATION == "$FIRST_CONVERSATION" ]] || fail "certificate rotation changed the durable conversation"
audit_running_container "$PRIMARY_NAME" "$HEALTH_OUTPUT"
note "certificate rotation reloaded TLS and retained the conversation"

# Remove and recreate the container while retaining /data. This is the closest
# Docker analogue to an App restart/upgrade and validates localDisk persistence.
"$DOCKER" stop --time 20 "$PRIMARY_NAME" >/dev/null
audit_stopped_logs "$PRIMARY_NAME" "primary App"
"$DOCKER" rm "$PRIMARY_NAME" >/dev/null
containers=()
readonly RESTART_NAME="hvh-restart-${RANDOM}-$$"
start_good_container "$RESTART_NAME"
RESTART_PORT=$(published_port "$RESTART_NAME") || fail "could not discover the restarted App port"
readonly RESTART_PORT
wait_for_health "$RESTART_NAME" "$RESTART_PORT" "$HEALTH_OUTPUT"
wait_for_runtime_cleanup "$RESTART_NAME"
if ! RESTART_CONVERSATION=$(probe_conversation "$RESTART_PORT"); then
  emit_sanitized_logs "$RESTART_NAME"
  fail "WSS reconnect after App restart failed"
fi
[[ $RESTART_CONVERSATION == "$FIRST_CONVERSATION" ]] || fail "App restart lost the durable conversation"
audit_running_container "$RESTART_NAME" "$HEALTH_OUTPUT"
note "App restart retained the durable conversation"

IMAGE_HISTORY="${ARTIFACT_DIR}/image.history"
"$DOCKER" image history --no-trunc "$IMAGE" >"$IMAGE_HISTORY" 2>&1
assert_secret_free "$IMAGE_HISTORY" "image history"

set_option() {
  local options_file="$1"
  local key="$2"
  local value="$3"
  python3 - "$options_file" "$key" "$value" <<'PY'
import json
import os
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
value = json.loads(path.read_text(encoding="utf-8"))
value[sys.argv[2]] = sys.argv[3]
temporary = path.with_suffix(".tmp")
fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
with os.fdopen(fd, "w", encoding="utf-8") as output:
    json.dump(value, output, separators=(",", ":"))
os.replace(temporary, path)
PY
}

wait_for_container_stop() {
  local name="$1"
  local label="$2"
  local deadline=$((SECONDS + START_TIMEOUT))
  while ((SECONDS < deadline)); do
    if ! container_running "$name"; then
      return 0
    fi
    sleep 0.25
  done
  fail "$label did not stop the App before the timeout"
}

# An incomplete certificate renewal must not take down a still-valid embedded
# certificate when the options snapshot is unchanged. A simultaneous option
# change is different: it must stop the App so revoked credentials cannot stay
# live behind a broken certificate update.
ACTIVE_CERTIFICATE=$(served_certificate_fingerprint_b64 "$RESTART_PORT")
printf '%s\n' 'not a certificate' >"${SSL_DIR}/.fullchain.pem.invalid"
chmod 0644 "${SSL_DIR}/.fullchain.pem.invalid"
mv -f -- "${SSL_DIR}/.fullchain.pem.invalid" "${SSL_DIR}/fullchain.pem"

retention_deadline=$((SECONDS + START_TIMEOUT))
retention_seen=false
while ((SECONDS < retention_deadline)); do
  container_running "$RESTART_NAME" || fail "App stopped for a certificate-only invalid renewal"
  if "$DOCKER" logs "$RESTART_NAME" 2>&1 \
      | grep -Fq 'retaining the active validated certificate'; then
    retention_seen=true
    break
  fi
  sleep 1
done
[[ $retention_seen == true ]] || fail "certificate-only retention branch was not observed"
if "$DOCKER" exec --user 0 "$RESTART_NAME" /usr/local/bin/gateway-healthcheck \
    >/dev/null 2>&1; then
  fail "invalid replacement certificate files did not mark App health degraded"
fi
health_once "$RESTART_PORT" "$HEALTH_OUTPUT" \
  || fail "active TLS health failed during an incomplete certificate renewal"
[[ $(served_certificate_fingerprint_b64 "$RESTART_PORT") == "$ACTIVE_CERTIFICATE" ]] \
  || fail "an incomplete renewal replaced the active certificate"
audit_running_container "$RESTART_NAME" "$HEALTH_OUTPUT"
note "incomplete certificate renewal retained the active validated certificate"

install_server_certificate 2
recovery_deadline=$((SECONDS + START_TIMEOUT))
while ((SECONDS < recovery_deadline)); do
  container_running "$RESTART_NAME" || fail "App stopped while the certificate source recovered"
  if "$DOCKER" exec --user 0 "$RESTART_NAME" /usr/local/bin/gateway-healthcheck \
      >/dev/null 2>&1; then
    break
  fi
  sleep 1
done
"$DOCKER" exec --user 0 "$RESTART_NAME" /usr/local/bin/gateway-healthcheck \
  >/dev/null 2>&1 || fail "restored active certificate source did not clear degraded health"

# Re-enter the degraded branch before changing credentials. This makes the
# options+TLS combined failure deterministic and proves the grace clock cannot
# silently turn the old credential into a valid runtime generation.
printf '%s\n' 'not a certificate' >"${SSL_DIR}/.fullchain.pem.invalid"
chmod 0644 "${SSL_DIR}/.fullchain.pem.invalid"
mv -f -- "${SSL_DIR}/.fullchain.pem.invalid" "${SSL_DIR}/fullchain.pem"
degraded_deadline=$((SECONDS + START_TIMEOUT))
while ((SECONDS < degraded_deadline)); do
  if ! "$DOCKER" exec --user 0 "$RESTART_NAME" /usr/local/bin/gateway-healthcheck \
      >/dev/null 2>&1; then
    break
  fi
  sleep 1
done
if "$DOCKER" exec --user 0 "$RESTART_NAME" /usr/local/bin/gateway-healthcheck \
    >/dev/null 2>&1; then
  fail "second invalid certificate source did not mark App health degraded"
fi

ROTATED_HERMES_KEY="hermes_rotated_$(openssl rand -hex 32)"
printf '%s\n' "$ROTATED_HERMES_KEY" >>"$SECRETS_FILE"
set_option "${DATA_DIR}/options.json" hermes_api_key "$ROTATED_HERMES_KEY"
wait_for_container_stop "$RESTART_NAME" "options plus invalid certificate update"
[[ $("$DOCKER" inspect --format '{{.State.ExitCode}}' "$RESTART_NAME") -ne 0 ]] \
  || fail "options plus invalid certificate update exited successfully"
audit_stopped_logs "$RESTART_NAME" "options plus invalid certificate App"
"$DOCKER" rm "$RESTART_NAME" >/dev/null
containers=()
install_server_certificate 2
note "options plus invalid certificate update stopped instead of retaining old credentials"

# Exercise the independent invalid-options branch with valid TLS files.
readonly INVALID_OPTIONS_NAME="hvh-invalid-options-${RANDOM}-$$"
start_good_container "$INVALID_OPTIONS_NAME"
INVALID_OPTIONS_PORT=$(published_port "$INVALID_OPTIONS_NAME") \
  || fail "could not discover the invalid-options test port"
wait_for_health "$INVALID_OPTIONS_NAME" "$INVALID_OPTIONS_PORT" "$HEALTH_OUTPUT"
set_option "${DATA_DIR}/options.json" hermes_base_url 'http://hermes.example.invalid'
wait_for_container_stop "$INVALID_OPTIONS_NAME" "invalid live options update"
[[ $("$DOCKER" inspect --format '{{.State.ExitCode}}' "$INVALID_OPTIONS_NAME") -ne 0 ]] \
  || fail "invalid live options update exited successfully"
audit_stopped_logs "$INVALID_OPTIONS_NAME" "invalid live options App"
"$DOCKER" rm "$INVALID_OPTIONS_NAME" >/dev/null
containers=()
set_option "${DATA_DIR}/options.json" hermes_base_url 'https://hermes.example.invalid'
note "invalid live options stopped the App so active credentials could not linger"

make_failure_case() {
  local label="$1"
  local root="${TEST_ROOT}/failure-${label}"
  mkdir -p -- "${root}/data" "${root}/ssl"
  chmod 0777 "${root}/data"
  chmod 0755 "${root}/ssl"
  cp -- "${DATA_DIR}/options.json" "${root}/data/options.json"
  chmod 0600 "${root}/data/options.json"
  printf '%s\n' "$root"
}

expect_start_failure() {
  local label="$1"
  local root="$2"
  local name="hvh-fail-${label}-${RANDOM}-$$"
  local cid running exit_code deadline logs
  logs="${ARTIFACT_DIR}/failure-${label}.logs"

  cid=$("$DOCKER" run --detach --name "$name" \
    --tmpfs /tmp:rw,nosuid,nodev,noexec \
    --volume "${root}/data:/data" \
    --volume "${PKI_DIR}/ca-bundle.pem:/etc/ssl/certs/ca-certificates.crt:ro" \
    --volume "${root}/ssl:/ssl:ro" \
    "$IMAGE") || fail "could not create negative-test container"
  [[ -n $cid ]] || fail "Docker did not return a negative-test container ID"
  containers+=("$name")
  deadline=$((SECONDS + FAILURE_TIMEOUT))
  running=true
  while ((SECONDS < deadline)); do
    running=$("$DOCKER" inspect --format '{{.State.Running}}' "$name" 2>/dev/null || true)
    [[ $running == false ]] && break
    sleep 0.25
  done
  [[ $running == false ]] || fail "$label configuration did not fail closed"
  exit_code=$("$DOCKER" inspect --format '{{.State.ExitCode}}' "$name")
  [[ $exit_code =~ ^[0-9]+$ && $exit_code -ne 0 ]] || fail "$label certificate configuration exited successfully"
  "$DOCKER" logs "$name" >"$logs" 2>&1 || true
  assert_secret_free "$logs" "$label startup logs"
  "$DOCKER" rm "$name" >/dev/null
  containers=("${containers[@]/$name}")
}

# Missing chain.
case_root=$(make_failure_case missing-cert)
cp -- "${SSL_DIR}/privkey.pem" "${case_root}/ssl/privkey.pem"
expect_start_failure missing-cert "$case_root"

# Missing private key.
case_root=$(make_failure_case missing-key)
cp -- "${SSL_DIR}/fullchain.pem" "${case_root}/ssl/fullchain.pem"
expect_start_failure missing-key "$case_root"

# A valid certificate paired with the wrong private key.
case_root=$(make_failure_case mismatched-key)
cp -- "${SSL_DIR}/fullchain.pem" "${case_root}/ssl/fullchain.pem"
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 \
  -out "${case_root}/ssl/privkey.pem" >/dev/null 2>&1
expect_start_failure mismatched-key "$case_root"

# A currently valid, key-matched self-signed leaf is still unusable by the
# public-CA-only Voice PE and must fail before workerd starts.
case_root=$(make_failure_case untrusted-chain)
openssl req -x509 -newkey rsa:2048 -nodes -sha256 -days 2 \
  -subj '/CN=untrusted.local' \
  -addext 'basicConstraints=critical,CA:FALSE' \
  -addext 'keyUsage=critical,digitalSignature,keyEncipherment' \
  -addext 'extendedKeyUsage=serverAuth' \
  -keyout "${case_root}/ssl/privkey.pem" \
  -out "${case_root}/ssl/fullchain.pem" >/dev/null 2>&1
expect_start_failure untrusted-chain "$case_root"

# Path traversal must be rejected before any filesystem open.
case_root=$(make_failure_case traversal)
set_option "${case_root}/data/options.json" certfile '../data/options.json'
cp -- "${SSL_DIR}/privkey.pem" "${case_root}/ssl/privkey.pem"
expect_start_failure traversal "$case_root"

# Even an in-root symlink to a valid regular certificate is forbidden. This
# avoids swap/race ambiguity between path validation and workerd startup.
case_root=$(make_failure_case symlink)
cp -- "${SSL_DIR}/fullchain.pem" "${case_root}/ssl/real-fullchain.pem"
ln -s real-fullchain.pem "${case_root}/ssl/fullchain.pem"
cp -- "${SSL_DIR}/privkey.pem" "${case_root}/ssl/privkey.pem"
expect_start_failure symlink "$case_root"

# A FIFO must be identified with lstat and rejected without blocking on open.
case_root=$(make_failure_case fifo)
mkfifo "${case_root}/ssl/fullchain.pem"
cp -- "${SSL_DIR}/privkey.pem" "${case_root}/ssl/privkey.pem"
expect_start_failure fifo "$case_root"

note "missing, untrusted, mismatched, traversal, symlink, and FIFO certificates all failed closed"
note "Docker App smoke/security test passed"
