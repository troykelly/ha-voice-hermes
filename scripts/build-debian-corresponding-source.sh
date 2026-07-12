#!/usr/bin/env bash
# Download and archive exact Debian source packages for an already-built App.

set -Eeuo pipefail
IFS=$'\n\t'
umask 022

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
readonly ROOT
readonly LOCK="${ROOT}/ha_voice_hermes_gateway/licenses/container/debian-sources.lock.json"
readonly SNAPSHOT_BASE="20260610T000000Z"
readonly SNAPSHOT_SECURITY="20260712T000000Z"

fail() {
  printf 'Debian corresponding-source build failed: %s\n' "$1" >&2
  exit 1
}

if [[ $# -ne 2 || -z ${1:-} || -z ${2:-} || $1 == -* || $2 == -* ]]; then
  printf 'usage: %s IMAGE OUTPUT.tar.gz\n' "${0##*/}" >&2
  exit 2
fi

readonly IMAGE="$1"
readonly OUTPUT="$2"
[[ $OUTPUT == *.tar.gz ]] || fail "output must end in .tar.gz"
command -v docker >/dev/null 2>&1 || fail "docker is unavailable"
[[ -s $LOCK ]] || fail "Debian source lock is missing"
docker image inspect "$IMAGE" >/dev/null 2>&1 || fail "App image is unavailable"

output_dir="$(cd -- "$(dirname -- "$OUTPUT")" && pwd -P)"
readonly output_dir
output_name="$(basename -- "$OUTPUT")"
readonly output_name
[[ $output_name =~ ^[A-Za-z0-9][A-Za-z0-9._-]*\.tar\.gz$ ]] \
  || fail "output basename contains unsupported characters"
work="$(mktemp -d "${TMPDIR:-/tmp}/ha-voice-hermes-source.XXXXXXXX")"
readonly work
cleanup() {
  if [[ -n ${archive_tmp:-} ]]; then
    rm -f -- "$archive_tmp"
  fi
  rm -rf -- "$work"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
mkdir -p -- "$work/bundle/sources"
cp -- "$LOCK" "$work/bundle/debian-sources.lock.json"

docker run --rm \
  --network bridge \
  --security-opt no-new-privileges \
  --entrypoint /bin/bash \
  --volume "$work/bundle:/out" \
  "$IMAGE" -Eeuo pipefail -c '
    export DEBIAN_FRONTEND=noninteractive
    export LC_ALL=C
    key=/usr/share/keyrings/debian-archive-keyring.gpg
    source_list=/etc/apt/sources.list.d/ha-voice-hermes-source-snapshots.list
    base_stamp='"$SNAPSHOT_BASE"'
    security_stamp='"$SNAPSHOT_SECURITY"'
    {
      for stamp in "$base_stamp" "$security_stamp"; do
        printf "deb-src [check-valid-until=no signed-by=%s] https://snapshot.debian.org/archive/debian/%s bookworm main\n" "$key" "$stamp"
        printf "deb-src [check-valid-until=no signed-by=%s] https://snapshot.debian.org/archive/debian/%s bookworm-updates main\n" "$key" "$stamp"
        printf "deb-src [check-valid-until=no signed-by=%s] https://snapshot.debian.org/archive/debian-security/%s bookworm-security main\n" "$key" "$stamp"
      done
    } > "$source_list"
    apt-get \
      -o Dir::Etc::sourcelist="$source_list" \
      -o Dir::Etc::sourceparts=- \
      -o Acquire::Check-Valid-Until=false \
      update >/dev/null

    cd /out/sources
    expected=$(jq -er ".source_package_count" /out/debian-sources.lock.json)
    while IFS=$'"'"'\t'"'"' read -r name version; do
      [[ $name =~ ^[a-z0-9][a-z0-9+.-]+$ ]]
      [[ $version =~ ^[^[:space:]/]+$ ]]
      apt-get \
        -o Dir::Etc::sourcelist="$source_list" \
        -o Dir::Etc::sourceparts=- \
        -o Acquire::Check-Valid-Until=false \
        source --download-only --only-source "${name}=${version}" >/dev/null
    done < <(jq -r '"'"'.sources[] | [.name,.version] | @tsv'"'"' /out/debian-sources.lock.json)

    actual=$(find . -maxdepth 1 -type f -name "*.dsc" | wc -l | tr -d "[:space:]")
    test "$actual" = "$expected"
    cd /out
    find sources -maxdepth 1 -type f -print0 \
      | sort -z \
      | xargs -0 sha256sum > SHA256SUMS
    test "$(wc -l < SHA256SUMS | tr -d "[:space:]")" -gt "$expected"
    sha256sum --check SHA256SUMS >/dev/null
    chmod -R a=rX,u+w /out
  '

cat >"$work/bundle/README.txt" <<EOF
This archive accompanies the ha-voice-hermes Home Assistant App image.

It contains the exact Debian source-package files selected by the locked
binary image, plus SHA256SUMS and the binary-to-source package inventory.
APT authenticated the source indices using Debian's archive keyring and the
immutable Debian snapshots ${SNAPSHOT_BASE} and ${SNAPSHOT_SECURITY}.

To inspect a source package, verify SHA256SUMS and then use:
  dpkg-source -x sources/<package>_<version>.dsc

This archive covers Debian packages only. Project, WASM, workerd, Bashio,
s6/skarnet, and other non-dpkg source and notice locations are documented in
the release's separate licence bundle and source manifest.
EOF

archive_tmp="$(mktemp "${output_dir}/.${output_name}.tmp.XXXXXXXX")"
readonly archive_tmp
docker run --rm \
  --network none \
  --read-only \
  --cap-drop ALL \
  --security-opt no-new-privileges \
  --user "$(id -u):$(id -g)" \
  --env ARCHIVE_TMP_NAME="$(basename -- "$archive_tmp")" \
  --entrypoint /bin/bash \
  --volume "$work:/work:ro" \
  --volume "$output_dir:/release" \
  "$IMAGE" -Eeuo pipefail -c '
    tar \
      --sort=name \
      --mtime="UTC 1970-01-01" \
      --owner=0 \
      --group=0 \
      --numeric-owner \
      -C /work \
      -cf - bundle \
      | gzip -n -9 > "/release/${ARCHIVE_TMP_NAME}"
    gzip -t "/release/${ARCHIVE_TMP_NAME}"
  '
mv -f -- "$archive_tmp" "${output_dir}/${output_name}"
printf 'created %s\n' "${output_dir}/${output_name}"
