#!/usr/bin/env python3
"""Refresh the pinned native workerd notice bundle from immutable inputs.

Downloaded bytes are never trusted on name or transport alone.  Every decoded
notice and every vendored derivation input must match the digest recorded in
LOCK.json before it can be compared with or written to the repository.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import sys
import tempfile
import urllib.request
import zipfile


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_BUNDLE = (
    ROOT
    / "ha_voice_hermes_gateway"
    / "licenses"
    / "container"
    / "workerd-native"
)
MAX_DOWNLOAD_BYTES = 32 * 1024 * 1024
USER_AGENT = "ha-voice-hermes-workerd-native-notice-refresh/1"


class RefreshError(RuntimeError):
    """A locked source could not be reproduced exactly."""


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def safe_relative_path(value: str) -> PurePosixPath:
    path = PurePosixPath(value)
    if path.is_absolute() or not path.parts or any(part in {"", ".", ".."} for part in path.parts):
        raise RefreshError(f"unsafe bundle path: {value!r}")
    if path.parts[0] not in {"notices", "evidence"}:
        raise RefreshError(f"bundle path must begin with notices/ or evidence/: {value!r}")
    return path


def read_limited(response: object, url: str) -> bytes:
    length_header = getattr(response, "headers").get("Content-Length")
    if length_header is not None and int(length_header) > MAX_DOWNLOAD_BYTES:
        raise RefreshError(f"locked source exceeds size limit: {url}")
    data = getattr(response, "read")(MAX_DOWNLOAD_BYTES + 1)
    if len(data) > MAX_DOWNLOAD_BYTES:
        raise RefreshError(f"locked source exceeds size limit: {url}")
    return data


def download(url: str) -> bytes:
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            return read_limited(response, url)
    except (OSError, ValueError) as error:
        raise RefreshError(f"could not download {url}: {error}") from error


def decode_blob(blob: dict[str, object], raw: bytes) -> bytes:
    encoding = blob.get("encoding", "identity")
    if encoding == "identity":
        return raw
    if encoding == "base64":
        try:
            return base64.b64decode(b"".join(raw.split()), validate=True)
        except ValueError as error:
            raise RefreshError(f"invalid base64 response for {blob['path']}") from error
    if encoding == "zip-member":
        archive_digest = blob.get("archive_sha256")
        if not isinstance(archive_digest, str) or sha256(raw) != archive_digest:
            raise RefreshError(f"archive digest mismatch for {blob['path']}")
        member = blob.get("archive_member")
        if not isinstance(member, str) or not member:
            raise RefreshError(f"missing archive member for {blob['path']}")
        try:
            with zipfile.ZipFile(io.BytesIO(raw)) as archive:
                return archive.read(member)
        except (KeyError, zipfile.BadZipFile) as error:
            raise RefreshError(f"cannot read locked archive member for {blob['path']}: {error}") from error
    if encoding == "text-section":
        source_digest = blob.get("source_sha256")
        if not isinstance(source_digest, str) or sha256(raw) != source_digest:
            raise RefreshError(f"source-file digest mismatch for {blob['path']}")
        start = blob.get("start_marker")
        end = blob.get("end_marker")
        if not isinstance(start, str) or not start or not isinstance(end, str) or not end:
            raise RefreshError(f"missing text-section markers for {blob['path']}")
        start_bytes = start.encode("utf-8")
        end_bytes = end.encode("utf-8")
        if raw.count(start_bytes) != 1 or raw.count(end_bytes) != 1:
            raise RefreshError(f"text-section markers are not unique for {blob['path']}")
        begin = raw.index(start_bytes)
        try:
            finish = raw.index(end_bytes, begin + len(start_bytes))
        except ValueError as error:
            raise RefreshError(f"text-section end precedes its start for {blob['path']}") from error
        return raw[begin:finish]
    raise RefreshError(f"unsupported encoding {encoding!r} for {blob['path']}")


def load_lock(bundle: Path) -> dict[str, object]:
    try:
        data = json.loads((bundle / "LOCK.json").read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise RefreshError(f"cannot read LOCK.json: {error}") from error
    if data.get("schema_version") != 1:
        raise RefreshError("unsupported lock schema")
    blobs = data.get("blobs")
    if not isinstance(blobs, list) or not blobs:
        raise RefreshError("LOCK.json has no blobs")
    return data


def materialize(data: dict[str, object]) -> dict[PurePosixPath, bytes]:
    cache: dict[str, bytes] = {}
    result: dict[PurePosixPath, bytes] = {}
    for raw_blob in data["blobs"]:  # type: ignore[index]
        if not isinstance(raw_blob, dict):
            raise RefreshError("blob entry is not an object")
        path_value = raw_blob.get("path")
        url = raw_blob.get("url")
        digest = raw_blob.get("sha256")
        if not all(isinstance(item, str) and item for item in (path_value, url, digest)):
            raise RefreshError("blob is missing path, url, or sha256")
        path = safe_relative_path(path_value)
        if path in result:
            raise RefreshError(f"duplicate bundle path: {path}")
        if url not in cache:
            cache[url] = download(url)
        decoded = decode_blob(raw_blob, cache[url])
        actual = sha256(decoded)
        if actual != digest:
            raise RefreshError(f"digest mismatch for {path}: expected {digest}, got {actual}")
        result[path] = decoded
    return result


def compare(bundle: Path, files: dict[PurePosixPath, bytes]) -> None:
    failures: list[str] = []
    expected = {Path(*path.parts) for path in files}
    for relative, expected_bytes in files.items():
        target = bundle.joinpath(*relative.parts)
        try:
            actual = target.read_bytes()
        except OSError as error:
            failures.append(f"{relative}: {error}")
            continue
        if actual != expected_bytes:
            failures.append(f"{relative}: checked-in bytes differ from locked upstream bytes")
    for root_name in ("notices", "evidence"):
        root = bundle / root_name
        if not root.exists():
            failures.append(f"{root_name}/: directory is missing")
            continue
        for path in root.rglob("*"):
            if path.is_file() and path.relative_to(bundle) not in expected:
                failures.append(f"{path.relative_to(bundle)}: unexpected file")
    if failures:
        raise RefreshError("refresh check failed:\n  " + "\n  ".join(sorted(failures)))


def update(bundle: Path, files: dict[PurePosixPath, bytes]) -> None:
    bundle.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="workerd-native-refresh.", dir=bundle.parent) as temporary:
        staging = Path(temporary)
        for relative, contents in files.items():
            target = staging.joinpath(*relative.parts)
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(contents)
            os.chmod(target, 0o644)
        for root_name in ("notices", "evidence"):
            source = staging / root_name
            destination = bundle / root_name
            backup = bundle / f".{root_name}.previous"
            if backup.exists():
                shutil.rmtree(backup)
            if destination.exists():
                destination.rename(backup)
            try:
                source.rename(destination)
            except BaseException:
                if backup.exists() and not destination.exists():
                    backup.rename(destination)
                raise
            if backup.exists():
                shutil.rmtree(backup)


def emit_plan(data: dict[str, object]) -> None:
    plan = [
        {
            "path": blob["path"],
            "sha256": blob["sha256"],
            "url": blob["url"],
            **({"archive_member": blob["archive_member"]} if "archive_member" in blob else {}),
            **({"encoding": blob["encoding"]} if "encoding" in blob else {}),
            **({"source_sha256": blob["source_sha256"]} if "source_sha256" in blob else {}),
            **({"start_marker": blob["start_marker"]} if "start_marker" in blob else {}),
            **({"end_marker": blob["end_marker"]} if "end_marker" in blob else {}),
        }
        for blob in data["blobs"]  # type: ignore[index]
    ]
    print(json.dumps(plan, indent=2, sort_keys=True))


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bundle", type=Path, default=DEFAULT_BUNDLE)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument("--check", action="store_true", help="download, verify, and compare without writing")
    action.add_argument("--update", action="store_true", help="replace notices/evidence after every digest matches")
    action.add_argument("--emit-plan", action="store_true", help="print the deterministic fetch plan without network access")
    return parser.parse_args()


def main() -> int:
    arguments = parse_args()
    bundle = arguments.bundle.resolve()
    data = load_lock(bundle)
    if arguments.emit_plan:
        emit_plan(data)
        return 0
    files = materialize(data)
    if arguments.check:
        compare(bundle, files)
        print(f"verified {len(files)} locked upstream workerd native files")
    else:
        update(bundle, files)
        print(f"updated {len(files)} locked upstream workerd native files")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except RefreshError as error:
        print(f"workerd native notice refresh failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
