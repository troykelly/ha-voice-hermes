#!/usr/bin/env bash
set -euo pipefail

ref=${1:-${RELEASE_REF:-}}
repository=${RELEASE_REPOSITORY:-https://github.com/troykelly/ha-voice-hermes.git}

if [[ -z $ref || $ref == main || $ref == master ]]; then
  printf 'Usage: %s <immutable-release-tag>\n' "$0" >&2
  exit 2
fi

if [[ ! $ref =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
  printf 'Release ref must be a stable vMAJOR.MINOR.PATCH tag.\n' >&2
  exit 2
fi

work=$(mktemp -d "${TMPDIR:-/tmp}/ha-voice-hermes-release.XXXXXX")
trap 'rm -rf "$work"' EXIT

git clone --quiet --depth 1 --branch "$ref" "$repository" "$work/source"
cd "$work/source"

if [[ $(git cat-file -t "$ref") != tag ]]; then
  printf 'Release ref must be an annotated tag, not a lightweight tag.\n' >&2
  exit 4
fi
git verify-tag "$ref"

test -f firmware/hermes-voice-pe.yaml
test -f firmware/packages/hermes-voice-pe-base.yaml
test -f firmware/components/hermes_voice/__init__.py
test -f firmware/components/hermes_voice/hermes_voice.cpp
test -f firmware/components/hermes_voice/hermes_voice.h
test -f firmware/models/SHA256SUMS

if command -v sha256sum >/dev/null 2>&1; then
  (cd firmware/models && sha256sum --check SHA256SUMS)
else
  (cd firmware/models && shasum -a 256 --check SHA256SUMS)
fi

if rg -n '@(main|master)$|ref: (main|master)$|type: local' firmware/hermes-voice-pe.yaml; then
  printf 'Public package contains a mutable or local component source.\n' >&2
  exit 4
fi

if rg -n '!secret' firmware/hermes-voice-pe.yaml firmware/packages/hermes-voice-pe-base.yaml; then
  printf 'Public zero-secret package unexpectedly depends on adopter secrets.\n' >&2
  exit 4
fi

configured_ref=$(sed -n 's/^  hermes_component_ref: //p' firmware/hermes-voice-pe.yaml)
if [[ $configured_ref != "$ref" ]]; then
  printf 'Public package ref %s does not match release tag %s.\n' "$configured_ref" "$ref" >&2
  exit 4
fi

esphome config firmware/hermes-voice-pe.factory.yaml >/dev/null
esphome config firmware/hermes-voice-pe.yaml >/dev/null

adopted_yaml="$work/adopted-hermes-voice-pe.yaml"
python3 - "$adopted_yaml" "$ref" <<'PY'
import sys
from esphome.components.dashboard_import import import_config

path, ref = sys.argv[1:]
import_config(
    path=path,
    name="hermes-voice-pe-release-test",
    friendly_name="Hermes Voice PE Release Test",
    project_name="local.ha-voice-hermes",
    import_url=(
        "github://troykelly/ha-voice-hermes/firmware/"
        f"hermes-voice-pe.yaml@{ref}"
    ),
    network="wifi",
    encryption=True,
)
PY

WIFI_SSID=release-test-wifi \
WIFI_PASSWORD=release-test-password \
HERMES_GATEWAY_URL=wss://release-test.invalid/v2/realtime \
  ./scripts/generate-device-secrets.sh "$work/secrets.yaml" >/dev/null

(
  cd "$work"
  esphome config "$adopted_yaml" >/dev/null
  esphome compile "$adopted_yaml" >/dev/null
)

printf 'Release source %s passed signed-tag, anonymous-clone, model-integrity, Dashboard Import, and adopted compile checks.\n' "$ref"
