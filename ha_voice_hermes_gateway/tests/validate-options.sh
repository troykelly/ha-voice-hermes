#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
validator="${root}/rootfs/usr/local/bin/validate-options.jq"
work="$(mktemp -d "${TMPDIR:-/tmp}/hvh-options.XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT

base="${work}/base.json"
cat >"$base" <<'JSON'
{
  "certfile":"fullchain.pem",
  "keyfile":"privkey.pem",
  "certificate_poll_seconds":60,
  "allow_private_upstreams":false,
  "hermes_base_url":"https://hermes.example.invalid",
  "hermes_api_key":"hermes-test-key",
  "hermes_model":"hermes-agent",
  "hermes_profile_id":"voice-safe",
  "hermes_binding_revision":"v1",
  "elevenlabs_api_key":"elevenlabs-test-key",
  "elevenlabs_voice_id":"test-voice",
  "realtime_stt_model":"scribe_v2_realtime",
  "tts_model":"eleven_flash_v2_5",
  "elevenlabs_enable_logging":false,
  "conversation_idle_seconds":900,
  "realtime_max_audio_seconds":30,
  "realtime_max_output_seconds":30,
  "max_connection_audio_seconds":900,
  "max_turns_per_connection":256,
  "max_messages_per_connection":16384,
  "diagnostic_v1_enabled":false,
  "devices":[{
    "device_id":"voice-pe-test-a",
    "device_token":"not-a-secret-device-token-a-00000000",
    "hermes_session_key":"agent:test:shared-room"
  }]
}
JSON

accept() {
  local name="$1"
  local filter="$2"
  jq "$filter" "$base" >"${work}/${name}.json"
  jq -e -f "$validator" "${work}/${name}.json" >/dev/null
}

reject() {
  local name="$1"
  local filter="$2"
  jq "$filter" "$base" >"${work}/${name}.json"
  if jq -e -f "$validator" "${work}/${name}.json" >/dev/null 2>&1; then
    echo "Invalid App option fixture was accepted: ${name}" >&2
    exit 1
  fi
}

accept baseline '.'
accept shared_session_scope '.devices += [{
  "device_id":"voice-pe-test-b",
  "device_token":"not-a-secret-device-token-b-00000000",
  "hermes_session_key":"agent:test:shared-room"
}]'

reject access_half_pair '.cf_access_client_id = "client-id"'
reject duplicate_device_id '.devices += [(.devices[0] | .device_token = "not-a-secret-device-token-b-00000000")]'
reject duplicate_device_token '.devices += [(.devices[0] | .device_id = "voice-pe-test-b")]'
reject missing_session_scope 'del(.devices[0].hermes_session_key)'
reject short_device_token '.devices[0].device_token = "too-short"'
reject unicode_session_scope '.devices[0].hermes_session_key = "room:厨房"'
reject spaced_session_scope '.devices[0].hermes_session_key = "room kitchen"'
reject unicode_hermes_key '.hermes_api_key = "hermes-😀"'
reject unicode_elevenlabs_key '.elevenlabs_api_key = "elevenlabs-你好"'
reject unicode_access_secret '.cf_access_client_id = "client-id" | .cf_access_client_secret = "secret-😀"'
reject insecure_hermes_url '.hermes_base_url = "http://hermes.example.invalid"'
reject credentialed_hermes_url '.hermes_base_url = "https://user@hermes.example.invalid"'
reject queried_hermes_url '.hermes_base_url = "https://hermes.example.invalid?target=other"'
reject private_opt_in_string '.allow_private_upstreams = "true"'
reject connection_audio_below_turn '.max_connection_audio_seconds = 29'
reject unknown_option '.unexpected_secret = "value"'
