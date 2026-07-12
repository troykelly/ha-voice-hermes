#!/usr/bin/env python3
"""Fail closed when the Home Assistant App notice inventory drifts."""

from __future__ import annotations

import hashlib
import json
import pathlib
import re
import sys


ROOT = pathlib.Path(__file__).resolve().parents[1]
APP = ROOT / "ha_voice_hermes_gateway"
LICENSE_DIR = APP / "licenses" / "container"
MANIFEST = LICENSE_DIR / "COMPONENTS.json"
DOCKERFILE = APP / "Dockerfile"
PACKAGE_LOCK = APP / "package-lock.json"
DEBIAN_SOURCE_LOCK = LICENSE_DIR / "debian-sources.lock.json"
WORKERD_NATIVE_LOCK = LICENSE_DIR / "workerd-native" / "LOCK.json"
WORKERD_RUST_NOTICES = LICENSE_DIR / "workerd-rust" / "THIRD_PARTY_NOTICES.md"
DEBIAN_SNAPSHOT = "20260712T000000Z"


def fail(message: str) -> None:
    raise SystemExit(f"container notice verification failed: {message}")


def sha256(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main() -> None:
    data = json.loads(MANIFEST.read_text(encoding="utf-8"))
    if data.get("schema_version") != 1:
        fail("unsupported manifest schema")

    base = data.get("home_assistant_base", {})
    reference = base.get("reference")
    if not isinstance(reference, str) or "@sha256:" not in reference:
        fail("Home Assistant base is not digest-pinned")
    if not re.fullmatch(r"https://github\.com/[^/]+/[^/]+/tree/[0-9a-f]{40}", str(base.get("source", ""))):
        fail("Home Assistant base source is not commit-pinned")

    dockerfile = DOCKERFILE.read_text(encoding="utf-8")
    from_lines = re.findall(r"^FROM\s+(\S+)", dockerfile, flags=re.MULTILINE)
    if reference not in from_lines:
        fail("Dockerfile base reference differs from the reviewed manifest")
    base_digest = reference.rsplit("@", 1)[-1]
    if f'org.opencontainers.image.base.digest="{base_digest}"' not in dockerfile:
        fail("flattened image does not retain its reviewed base digest label")
    if f'io.hass.base.version="{base.get("version")}"' not in dockerfile:
        fail("flattened image does not retain its reviewed Home Assistant base version")
    if "FROM scratch" not in dockerfile or "COPY --from=app-rootfs / /" not in dockerfile:
        fail("Home Assistant root filesystem is no longer flattened through scratch")
    if "rm -f /usr/bin/tempio" not in dockerfile or "test ! -e /usr/bin/tempio" not in dockerfile:
        fail("the unused inherited tempio binary is not removed before flattening")
    if "COPY licenses /usr/share/licenses/ha-voice-hermes" not in dockerfile:
        fail("Dockerfile no longer embeds the reviewed notice directory")
    if "COPY licenses/container/workerd-native/LOCK.json ./workerd-native-lock.json" not in dockerfile:
        fail("Dockerfile no longer binds the copied workerd binary to the native lock")
    if "workerd binary digest mismatch" not in dockerfile:
        fail("Dockerfile no longer verifies the architecture-specific workerd binary digest")
    for archive in ("debian", "debian-security"):
        snapshot = f"snapshot.debian.org/archive/{archive}/{DEBIAN_SNAPSHOT}"
        if snapshot not in dockerfile:
            fail(f"Dockerfile no longer uses the reviewed {archive} package snapshot")
    for package_pin in (
        "gcc=4:12.2.0-3",
        "libc6-dev=2.36-9+deb12u14",
        "jq=1.6-2.1+deb12u2",
        "libjq1=1.6-2.1+deb12u2",
    ):
        if package_pin not in dockerfile:
            fail(f"Dockerfile no longer installs reviewed package pin {package_pin}")
    if dockerfile.count("Acquire::Check-Valid-Until=false") != 4:
        fail("Dockerfile snapshot update/install commands drifted")

    package_lock = json.loads(PACKAGE_LOCK.read_text(encoding="utf-8"))
    debian_lock = json.loads(DEBIAN_SOURCE_LOCK.read_text(encoding="utf-8"))
    native_lock = json.loads(WORKERD_NATIVE_LOCK.read_text(encoding="utf-8"))
    rust_notices = WORKERD_RUST_NOTICES.read_text(encoding="utf-8")
    native_legal_gate = native_lock.get("legal_gate")
    native_components = native_lock.get("components")
    native_blobs = native_lock.get("blobs")
    if (
        native_lock.get("schema_version") != 1
        or native_lock.get("unresolved") != []
        or not isinstance(native_legal_gate, dict)
        or native_legal_gate.get("status") != "covered"
        or not isinstance(native_components, list)
        or len(native_components) != 24
        or not isinstance(native_blobs, list)
        or len(native_blobs) != 113
    ):
        fail("native workerd legal gate is incomplete")
    if "Unresolved lockfile packages: **0**" not in rust_notices:
        fail("workerd Rust notice bundle still has unresolved packages")
    if debian_lock.get("schema_version") != 1:
        fail("unsupported Debian source-lock schema")
    if debian_lock.get("base_reference") != reference:
        fail("Debian source lock is not bound to the reviewed Home Assistant base")
    debian_sources = debian_lock.get("sources")
    if not isinstance(debian_sources, list) or len(debian_sources) != debian_lock.get("source_package_count"):
        fail("Debian source lock has inconsistent source-package counts")
    source_identities = {
        (entry.get("name"), entry.get("version"))
        for entry in debian_sources
        if isinstance(entry, dict)
    }
    if len(source_identities) != len(debian_sources) or any(not all(identity) for identity in source_identities):
        fail("Debian source lock has duplicate or malformed source identities")
    workerd_versions = {
        component["version"]
        for component in data.get("components", [])
        if component.get("name") == "workerd"
    }
    if len(workerd_versions) != 1:
        fail("manifest must contain exactly one workerd version")
    expected_workerd = next(iter(workerd_versions))
    locked_workerd = package_lock.get("packages", {}).get("node_modules/workerd", {}).get("version")
    declared_workerd = package_lock.get("packages", {}).get("", {}).get("dependencies", {}).get("workerd")
    if locked_workerd != expected_workerd or declared_workerd != expected_workerd:
        fail("workerd manifest, declaration, and lock versions differ")

    seen: set[tuple[str, str]] = set()
    runtime_paths: set[str] = set()
    components = data.get("components")
    if not isinstance(components, list) or not components:
        fail("component inventory is empty")
    for component in components:
        required = ("name", "version", "license", "source", "notice_file", "notice_sha256")
        missing = [key for key in required if not component.get(key)]
        if missing:
            fail(f"component is missing {', '.join(missing)}")
        source = component["source"]
        immutable_github = re.fullmatch(
            r"https://github\.com/[^/]+/[^/]+/tree/[0-9a-f]{40}", source
        )
        immutable_bearssl = re.fullmatch(
            r"https://www\.bearssl\.org/gitweb/\?p=BearSSL;a=commit;h=[0-9a-f]{40}",
            source,
        )
        if immutable_github is None and immutable_bearssl is None:
            fail(f"component source is not commit-pinned for {component['name']}")
        identity = (component["name"], component["version"])
        if identity in seen:
            fail(f"duplicate component {identity[0]} {identity[1]}")
        seen.add(identity)
        runtime_path = component.get("runtime_path")
        embedded_in = component.get("embedded_in")
        if (runtime_path is None) == (embedded_in is None):
            fail(f"{identity[0]} must have exactly one runtime_path or embedded_in")
        if runtime_path is not None:
            if not isinstance(runtime_path, str) or not runtime_path.startswith("/") or ".." in pathlib.PurePosixPath(runtime_path).parts:
                fail(f"unsafe runtime path for {identity[0]}")
            if runtime_path in runtime_paths:
                fail(f"two components unexpectedly share runtime path {runtime_path}")
            runtime_paths.add(runtime_path)
        elif not isinstance(embedded_in, str) or not embedded_in:
            fail(f"invalid embedded_in relationship for {identity[0]}")
        notice = (LICENSE_DIR / component["notice_file"]).resolve()
        allowed_root = (APP / "licenses").resolve()
        if not notice.is_relative_to(allowed_root) or not notice.is_file():
            fail(f"missing or unsafe notice for {identity[0]}")
        if sha256(notice) != component["notice_sha256"]:
            fail(f"notice hash drift for {identity[0]}")

    required_components = {
        "BearSSL",
        "bashio",
        "execline",
        "s6",
        "s6-dns",
        "s6-linux-init",
        "s6-linux-utils",
        "s6-networking",
        "s6-overlay",
        "s6-overlay-helpers",
        "s6-portable-utils",
        "s6-rc",
        "skalibs",
        "workerd",
    }
    names = {name for name, _ in seen}
    if names != required_components:
        fail(f"reviewed component set changed: expected {sorted(required_components)}, got {sorted(names)}")
    print(
        f"verified {len(components)} container components and notices plus "
        f"{len(debian_sources)} Debian source packages"
    )


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, TypeError, json.JSONDecodeError) as error:
        fail(str(error))
