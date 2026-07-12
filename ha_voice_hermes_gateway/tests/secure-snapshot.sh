#!/usr/bin/env bash
set -Eeuo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
source_file="${root}/rootfs/usr/local/bin/secure-snapshot.c"

# The helper deliberately uses Linux open/stat hardening interfaces and is
# compiled in the Debian image and Ubuntu CI. Keep local macOS validation
# useful without pretending the helper is a portable host utility.
if [[ $(uname -s) != Linux ]]; then
  printf '%s\n' 'secure-snapshot behavioral checks skipped (Linux required)'
  exit 0
fi

work="$(mktemp -d "${TMPDIR:-/tmp}/hvh-snapshot.XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT

cc -std=c17 -O2 -Wall -Wextra -Werror -o "${work}/secure-snapshot" "$source_file"
mkdir "${work}/source" "${work}/destination"
printf '%s' 'private-value' >"${work}/source/options.json"
chmod 0600 "${work}/source/options.json"

"${work}/secure-snapshot" \
  "${work}/source" options.json "${work}/destination" snapshot.json 64
cmp "${work}/source/options.json" "${work}/destination/snapshot.json"
[[ $(stat -c '%a' "${work}/destination/snapshot.json") == 400 ]]

expect_rejection() {
  local name="$1"
  shift
  if "$@" >/dev/null 2>&1; then
    printf 'unsafe snapshot case was accepted: %s\n' "$name" >&2
    exit 1
  fi
}

ln -s options.json "${work}/source/link.json"
expect_rejection source_symlink \
  "${work}/secure-snapshot" "${work}/source" link.json "${work}/destination" link-copy 64
expect_rejection traversal \
  "${work}/secure-snapshot" "${work}/source" ../source/options.json "${work}/destination" traversal-copy 64
expect_rejection oversize \
  "${work}/secure-snapshot" "${work}/source" options.json "${work}/destination" oversize-copy 4

chmod 0660 "${work}/source/options.json"
expect_rejection shared_writable \
  "${work}/secure-snapshot" "${work}/source" options.json "${work}/destination" writable-copy 64

printf '%s\n' 'secure-snapshot behavioral checks passed'
