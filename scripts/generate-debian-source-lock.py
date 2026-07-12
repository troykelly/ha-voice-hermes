#!/usr/bin/env python3
"""Bind the flattened App image to exact Debian corresponding-source versions."""

from __future__ import annotations

import argparse
import json
import pathlib
import subprocess
import sys


ROOT = pathlib.Path(__file__).resolve().parents[1]
OUTPUT = (
    ROOT
    / "ha_voice_hermes_gateway"
    / "licenses"
    / "container"
    / "debian-sources.lock.json"
)
BASE_REFERENCE = (
    "ghcr.io/home-assistant/base-debian:bookworm@"
    "sha256:8c7a9e207425e79b6b2ed1628a2b6727fa6e518d9fdddcbe3b1ac20440e70492"
)


class SourceLockError(RuntimeError):
    """Image package metadata was incomplete or inconsistent."""


def image_rows(image: str) -> list[tuple[str, str, str, str]]:
    command = [
        "docker",
        "run",
        "--rm",
        "--network",
        "none",
        "--read-only",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--entrypoint",
        "/bin/sh",
        image,
        "-eu",
        "-c",
        (
            "for package in $(dpkg-query -W -f='${Package}\\n'); do "
            "test -r /usr/share/doc/${package}/copyright; done; "
            "dpkg-query -W -f='${binary:Package}\\t${Version}\\t"
            "${source:Package}\\t${source:Version}\\n'"
        ),
    ]
    result = subprocess.run(
        command,
        cwd=ROOT,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip()
        raise SourceLockError(f"could not inspect App image: {detail}")
    rows: list[tuple[str, str, str, str]] = []
    for line in result.stdout.splitlines():
        fields = line.split("\t")
        if len(fields) != 4 or not all(fields):
            raise SourceLockError(f"malformed dpkg-query row: {line!r}")
        binary, binary_version, source, source_version = fields
        rows.append((binary.split(":", 1)[0], binary_version, source, source_version))
    if not rows:
        raise SourceLockError("App image contains no dpkg packages")
    return rows


def render(image: str) -> bytes:
    rows = image_rows(image)
    sources: dict[str, dict[str, object]] = {}
    binaries: set[tuple[str, str]] = set()
    for binary, binary_version, source, source_version in rows:
        binary_identity = (binary, binary_version)
        if binary_identity in binaries:
            raise SourceLockError(f"duplicate binary package row: {binary} {binary_version}")
        binaries.add(binary_identity)
        entry = sources.setdefault(
            source,
            {"name": source, "version": source_version, "binary_packages": []},
        )
        if entry["version"] != source_version:
            raise SourceLockError(
                f"source package {source} has conflicting versions "
                f"{entry['version']} and {source_version}"
            )
        binary_list = entry["binary_packages"]
        assert isinstance(binary_list, list)
        binary_list.append({"name": binary, "version": binary_version})

    for entry in sources.values():
        binary_list = entry["binary_packages"]
        assert isinstance(binary_list, list)
        binary_list.sort(key=lambda package: (package["name"], package["version"]))

    document = {
        "schema_version": 1,
        "distribution": "Debian 12 (bookworm)",
        "base_reference": BASE_REFERENCE,
        "binary_package_count": len(binaries),
        "source_package_count": len(sources),
        "sources": sorted(sources.values(), key=lambda entry: (entry["name"], entry["version"])),
    }
    return (json.dumps(document, indent=2, ensure_ascii=False) + "\n").encode("utf-8")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--image", required=True, help="already-built native App image")
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--check", action="store_true")
    mode.add_argument("--stdout", action="store_true")
    mode.add_argument("--update", action="store_true")
    arguments = parser.parse_args()
    generated = render(arguments.image)

    if arguments.stdout:
        sys.stdout.buffer.write(generated)
        return
    if arguments.update:
        OUTPUT.parent.mkdir(parents=True, exist_ok=True)
        OUTPUT.write_bytes(generated)
        print(f"updated {OUTPUT.relative_to(ROOT)}")
        return
    try:
        current = OUTPUT.read_bytes()
    except OSError as error:
        raise SourceLockError(f"missing checked-in source lock: {OUTPUT}") from error
    if current != generated:
        raise SourceLockError(
            "Debian package/source closure drifted; regenerate and review the source lock"
        )
    print(
        f"verified Debian source lock for {json.loads(current)['source_package_count']} source packages"
    )


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, TypeError, json.JSONDecodeError, SourceLockError) as error:
        raise SystemExit(f"Debian source lock generation failed: {error}") from error
