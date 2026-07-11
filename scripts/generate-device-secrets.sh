#!/usr/bin/env bash
set -euo pipefail

output=${1:-firmware/secrets.yaml}

for name in WIFI_SSID WIFI_PASSWORD HERMES_GATEWAY_URL; do
  if [[ -z ${!name:-} ]]; then
    printf 'Required environment variable %s is not set.\n' "$name" >&2
    exit 2
  fi
done

if [[ $HERMES_GATEWAY_URL != wss://* ]]; then
  printf 'HERMES_GATEWAY_URL must begin with wss://.\n' >&2
  exit 2
fi

if [[ -e $output ]]; then
  printf 'Refusing to overwrite %s.\n' "$output" >&2
  exit 3
fi

yaml_quote() {
  local value=${1//\'/\'\'}
  printf "'%s'" "$value"
}

umask 077
ota_password=$(openssl rand -hex 32)
api_encryption_key=$(openssl rand -base64 32 | tr -d '\n')
device_token=$(openssl rand -hex 32)

mkdir -p "$(dirname "$output")"
{
  printf 'wifi_ssid: '
  yaml_quote "$WIFI_SSID"
  printf '\nwifi_password: '
  yaml_quote "$WIFI_PASSWORD"
  printf '\nota_password: '
  yaml_quote "$ota_password"
  printf '\napi_encryption_key: '
  yaml_quote "$api_encryption_key"
  printf '\nhermes_gateway_url: '
  yaml_quote "$HERMES_GATEWAY_URL"
  printf '\nhermes_device_token: '
  yaml_quote "$device_token"
  printf '\n'
} >"$output"

chmod 600 "$output"
printf 'Created %s with mode 0600. Add its hermes_device_token to the Worker per-device token map.\n' "$output"
