#!/usr/bin/env python3
"""Generate the pinned workerd Rust source, license, and notice inventory.

The Home Assistant App redistributes Cloudflare's prebuilt workerd executable.
This deliberately over-inclusive inventory covers every registry, git, and
path package in the exact workerd lockfile, verifies immutable source inputs,
preserves all shipped notice files, and records exact source-form download
URLs. It also includes workerd's pinned Rust standard-library/runtime notices.
"""

from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, replace
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import urllib.error
import urllib.parse
import urllib.request


ROOT = Path(__file__).resolve().parents[1]
OUTPUT_DIRECTORY = (
    ROOT
    / "ha_voice_hermes_gateway"
    / "licenses"
    / "container"
    / "workerd-rust"
)
OUTPUT = OUTPUT_DIRECTORY / "THIRD_PARTY_NOTICES.md"
LOCK_OUTPUT = OUTPUT_DIRECTORY / "UPSTREAM_CARGO.lock"

WORKERD_REPOSITORY = "https://github.com/cloudflare/workerd.git"
WORKERD_TAG = "v1.20260708.1"
WORKERD_COMMIT = "99057e45cb7152cc5efc59b8d2182386fe1e3eec"
WORKERD_LOCK_URL = (
    "https://raw.githubusercontent.com/cloudflare/workerd/"
    f"{WORKERD_COMMIT}/deps/rust/Cargo.lock"
)
WORKERD_LOCK_SHA256 = "d2f4504f29de4419cbc792e27dcdca430cb9a5202cda5c003b7679b5bacbfaf7"
WORKERD_RUST_PIN_URL = (
    "https://raw.githubusercontent.com/cloudflare/workerd/"
    f"{WORKERD_COMMIT}/build/deps/rust.MODULE.bazel"
)
WORKERD_RUST_PIN_SHA256 = "b0ddf5c5dbb280e86af6469f0db6775df671619d9fb8f04f2bd549f1eedbe7c2"
WORKERD_SOURCE_ARCHIVE_URL = (
    "https://codeload.github.com/cloudflare/workerd/tar.gz/"
    f"{WORKERD_COMMIT}"
)
WORKERD_SOURCE_ARCHIVE_SHA256 = (
    "8d35c463c40eacbe6984a6dc5c454ae1e837ead1e21ce18b052034a04e5b40f6"
)

EXPECTED_REGISTRY_PACKAGES = 234
EXPECTED_PACKAGES_WITHOUT_ARCHIVE_NOTICE = 31
EXPECTED_GIT_PACKAGES = 7
EXPECTED_PATH_PACKAGES = 1
EXPECTED_MPL_PACKAGES = {
    ("cssparser", "0.36.0"),
    ("cssparser-macros", "0.6.1"),
    ("dtoa-short", "0.3.5"),
    ("selectors", "0.33.0"),
    ("smartstring", "1.0.1"),
}
REGISTRY_SOURCE = "registry+https://github.com/rust-lang/crates.io-index"

EXPECTED_RUST_RELEASE = "1.96.0"
EXPECTED_RUST_COMMIT = "ac68faa20c58cbccd01ee7208bf3b6e93a7d7f96"
RUST_NOTICE_PATHS = (
    "COPYRIGHT-library.html",
    "licenses/Apache-2.0.txt",
    "licenses/BSD-2-Clause.txt",
    "licenses/LLVM-exception.txt",
    "licenses/MIT.txt",
    "licenses/Unicode-3.0.txt",
)

NOTICE_FILE_RE = re.compile(
    r"^(?:licen[cs]e|copying|notice|copyright|unlicense)(?:[._-].*)?$",
    re.IGNORECASE,
)
MAX_DOWNLOAD_BYTES = 128 * 1024 * 1024
MAX_NOTICE_BYTES = 2 * 1024 * 1024
USER_AGENT = "ha-voice-hermes-workerd-license-generator/1"

# A few crates omit notices from their crates.io archives. Prefer an exact VCS
# notice when one is available. The remaining missing archives are covered by
# an explicitly selected canonical license text below.
EXTERNAL_FALLBACKS = {
    "nugine-simd": (
        "https://raw.githubusercontent.com/Nugine/simd/"
        "d74c030d9dc4f3cae02146d1f497ff62726ef09a/LICENSE",
        "71674605ec4c087fe9eb534e3e4f9e26eb2e4aabcd76a29fd156c6a844d44b3d",
    ),
    "ddbase-bytes-str": (
        "https://raw.githubusercontent.com/dudykr/ddbase/"
        "962bccaa3a774d2c7a699c0fe88355ad761c7978/LICENSE",
        "58d1e17ffe5109a7ae296caafcadfdbe6a7d176f0bc4ab01e12a689b0499d8bd",
    ),
    "ddbase-par-core": (
        "https://raw.githubusercontent.com/dudykr/ddbase/"
        "d46ef439d30d2b64eb87d4e197929548b51b21be/LICENSE",
        "58d1e17ffe5109a7ae296caafcadfdbe6a7d176f0bc4ab01e12a689b0499d8bd",
    ),
    "swc-sourcemap-bsd": (
        "https://raw.githubusercontent.com/swc-project/swc-sourcemap/"
        "64fcb47c16547d40fb37bbe4d8790b520716c741/LICENSE",
        "7516e1cf340213f60d96bca77bb012882dbf80e7cca5922c914174f605d9ef71",
    ),
}
PACKAGE_EXTERNAL_FALLBACK = {
    "base64-simd": "nugine-simd",
    "bytes-str": "ddbase-bytes-str",
    "par-core": "ddbase-par-core",
    "swc_sourcemap": "swc-sourcemap-bsd",
    "vsimd": "nugine-simd",
}

LOL_HTML_REPOSITORY = "https://github.com/cloudflare/lol-html.git"
LOL_HTML_TAG = "v2.7.2"
LOL_HTML_COMMIT = "e3aa54798602dd27250fafde1b5a66f080046252"
LOL_HTML_ARCHIVE_URL = (
    "https://codeload.github.com/cloudflare/lol-html/tar.gz/"
    f"{LOL_HTML_COMMIT}"
)
LOL_HTML_ARCHIVE_SHA256 = (
    "c0a26b7c0b010b1d4fdfc4b652db7c7e791e987f533eddc01988b19576df2428"
)

RUFF_REPOSITORY = "https://github.com/astral-sh/ruff.git"
RUFF_TAG = "0.12.1"
RUFF_COMMIT = "32c54189cb45a9d0409a1140265ce6d5fcec214d"
RUFF_ARCHIVE_URL = (
    "https://codeload.github.com/astral-sh/ruff/tar.gz/"
    f"{RUFF_COMMIT}"
)
RUFF_ARCHIVE_SHA256 = (
    "42a6ae45c09fd5cb400b280e8280dab57ebe9ee45fbccb5232bdcaf3134c8808"
)

EXPECTED_NONREGISTRY_PACKAGES = {
    (
        "direct-cargo-bazel-deps",
        "0.0.1",
        "path dependency",
    ),
    (
        "lol_html",
        "2.7.2",
        "git+https://github.com/cloudflare/lol-html?tag=v2.7.2#"
        f"{LOL_HTML_COMMIT}",
    ),
    (
        "lol_html_c_api",
        "1.3.1",
        "git+https://github.com/cloudflare/lol-html?tag=v2.7.2#"
        f"{LOL_HTML_COMMIT}",
    ),
    *{
        (
            name,
            "0.0.0",
            "git+https://github.com/astral-sh/ruff?tag=0.12.1#"
            f"{RUFF_COMMIT}",
        )
        for name in (
            "ruff_python_ast",
            "ruff_python_parser",
            "ruff_python_trivia",
            "ruff_source_file",
            "ruff_text_size",
        )
    },
}


class GenerationError(RuntimeError):
    """Generation could not continue without unverified provenance."""


@dataclass(frozen=True)
class Notice:
    name: str
    content: bytes
    sha256: str
    provenance: str


@dataclass(frozen=True)
class LockedPackage:
    name: str
    version: str
    checksum: str
    dependencies: tuple[str, ...]

    @property
    def archive_url(self) -> str:
        quoted_name = urllib.parse.quote(self.name, safe="")
        quoted_file = urllib.parse.quote(f"{self.name}-{self.version}", safe="")
        return f"https://static.crates.io/crates/{quoted_name}/{quoted_file}.crate"


@dataclass(frozen=True)
class Package:
    name: str
    version: str
    checksum: str
    license_expression: str
    archive_url: str
    repository: str
    vcs_commit: str
    vcs_path: str
    vcs_dirty: bool
    notices: tuple[Notice, ...]
    used_fallback: bool = False


@dataclass(frozen=True)
class SourceRepository:
    name: str
    repository: str
    tag: str
    commit: str
    archive_url: str
    archive_sha256: str
    prefix: str
    license_path: str
    license_sha256: str
    default_license: str
    package_manifests: tuple[tuple[str, str, str], ...]


@dataclass(frozen=True)
class SourcePackage:
    name: str
    version: str
    lock_source: str
    source_kind: str
    license_expression: str
    license_basis: str
    repository: str
    tag: str
    commit: str
    archive_url: str
    archive_sha256: str
    manifest_path: str
    notices: tuple[Notice, ...]


@dataclass(frozen=True)
class RustToolchain:
    release: str
    commit_hash: str
    commit_date: str
    llvm_version: str
    notices: tuple[Notice, ...]


@dataclass(frozen=True)
class Upstream:
    lock_bytes: bytes
    rust_pin_bytes: bytes
    nonregistry_packages: tuple[tuple[str, str, str], ...]


@dataclass(frozen=True)
class Generation:
    notices: bytes
    lock: bytes
    unresolved_count: int


SOURCE_REPOSITORIES = (
    SourceRepository(
        name="workerd",
        repository=WORKERD_REPOSITORY,
        tag=WORKERD_TAG,
        commit=WORKERD_COMMIT,
        archive_url=WORKERD_SOURCE_ARCHIVE_URL,
        archive_sha256=WORKERD_SOURCE_ARCHIVE_SHA256,
        prefix=f"workerd-{WORKERD_COMMIT}",
        license_path="LICENSE",
        license_sha256="0d542e0c8804e39aa7f37eb00da5a762149dc682d7829451287e11b938e94594",
        default_license="Apache-2.0",
        package_manifests=(
            ("direct-cargo-bazel-deps", "0.0.1", "deps/rust/Cargo.toml"),
        ),
    ),
    SourceRepository(
        name="lol-html",
        repository=LOL_HTML_REPOSITORY,
        tag=LOL_HTML_TAG,
        commit=LOL_HTML_COMMIT,
        archive_url=LOL_HTML_ARCHIVE_URL,
        archive_sha256=LOL_HTML_ARCHIVE_SHA256,
        prefix=f"lol-html-{LOL_HTML_COMMIT}",
        license_path="LICENSE",
        license_sha256="e4ddaa9d7391bb9536fcb8c59b570a8b85a0bf6da54df5b3b26f098f6f99c9cc",
        default_license="BSD-3-Clause",
        package_manifests=(
            ("lol_html", "2.7.2", "Cargo.toml"),
            ("lol_html_c_api", "1.3.1", "c-api/Cargo.toml"),
        ),
    ),
    SourceRepository(
        name="ruff",
        repository=RUFF_REPOSITORY,
        tag=RUFF_TAG,
        commit=RUFF_COMMIT,
        archive_url=RUFF_ARCHIVE_URL,
        archive_sha256=RUFF_ARCHIVE_SHA256,
        prefix=f"ruff-{RUFF_COMMIT}",
        license_path="LICENSE",
        license_sha256="3209c0b7fb7257c05b16433728c1e974bac40000fef171b8e9e89f095623c954",
        default_license="MIT",
        package_manifests=tuple(
            (name, "0.0.0", f"crates/{name}/Cargo.toml")
            for name in (
                "ruff_python_ast",
                "ruff_python_parser",
                "ruff_python_trivia",
                "ruff_source_file",
                "ruff_text_size",
            )
        ),
    ),
)


def _sha256(content: bytes) -> str:
    return hashlib.sha256(content).hexdigest()


def _download(url: str, *, maximum: int = MAX_DOWNLOAD_BYTES) -> bytes:
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            content = response.read(maximum + 1)
    except (urllib.error.URLError, TimeoutError, OSError) as error:
        raise GenerationError(f"could not download pinned source: {url}") from error
    if len(content) > maximum:
        raise GenerationError(f"pinned source exceeds size limit: {url}")
    return content


def _git_tag_commit(repository: str, tag: str) -> str:
    git = shutil.which("git")
    if git is None:
        raise GenerationError("git is required to verify remote source tags")
    environment = os.environ.copy()
    environment["GIT_TERMINAL_PROMPT"] = "0"
    environment["GCM_INTERACTIVE"] = "Never"
    result = subprocess.run(
        [git, "ls-remote", repository, f"refs/tags/{tag}", f"refs/tags/{tag}^{{}}"],
        cwd=ROOT,
        env=environment,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if result.returncode != 0:
        raise GenerationError(f"could not verify remote tag {repository} {tag}")
    lines = [line.split() for line in result.stdout.splitlines() if line.strip()]
    if not lines or any(len(line) != 2 for line in lines):
        raise GenerationError(f"remote tag did not resolve uniquely: {repository} {tag}")
    values = {reference: commit for commit, reference in lines}
    if len(values) != len(lines):
        raise GenerationError(f"remote tag returned duplicate refs: {repository} {tag}")
    direct = values.get(f"refs/tags/{tag}")
    peeled = values.get(f"refs/tags/{tag}^{{}}")
    if direct is None or len(values) > 2:
        raise GenerationError(f"remote tag did not resolve uniquely: {repository} {tag}")
    if peeled is not None:
        raise GenerationError(
            f"remote tag changed from reviewed lightweight form: {repository} {tag}"
        )
    commit = direct
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise GenerationError(f"remote tag returned an invalid commit: {repository} {tag}")
    return commit


def _remote_tag_commit() -> str:
    """Compatibility wrapper used by the workerd drift audit."""

    return _git_tag_commit(WORKERD_REPOSITORY, WORKERD_TAG)


def _upstream() -> Upstream:
    remote_commit = _remote_tag_commit()
    if remote_commit != WORKERD_COMMIT:
        raise GenerationError(
            f"remote {WORKERD_TAG} moved: expected {WORKERD_COMMIT}, found {remote_commit}"
        )

    lock_bytes = _download(WORKERD_LOCK_URL)
    if _sha256(lock_bytes) != WORKERD_LOCK_SHA256:
        raise GenerationError("the pinned workerd deps/rust/Cargo.lock hash changed")
    rust_pin_bytes = _download(WORKERD_RUST_PIN_URL)
    if _sha256(rust_pin_bytes) != WORKERD_RUST_PIN_SHA256:
        raise GenerationError("the pinned workerd Rust toolchain definition hash changed")
    pin_text = rust_pin_bytes.decode("utf-8")
    expected_pin = f'RUST_STABLE_VERSION = "{EXPECTED_RUST_RELEASE}"'
    if expected_pin not in pin_text:
        raise GenerationError("the pinned workerd Rust version is not the reviewed release")

    try:
        parsed = tomllib.loads(lock_bytes.decode("utf-8"))
    except (UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
        raise GenerationError("the pinned workerd lockfile is invalid") from error
    nonregistry_packages: list[tuple[str, str, str]] = []
    for raw in parsed.get("package", []):
        if isinstance(raw, dict) and raw.get("source") != REGISTRY_SOURCE:
            name = raw.get("name")
            version = raw.get("version")
            source = raw.get("source") or "path dependency"
            if all(isinstance(value, str) for value in (name, version, source)):
                nonregistry_packages.append((name, version, source))
    nonregistry_packages.sort(key=lambda item: (item[0].casefold(), item))
    found_nonregistry = set(nonregistry_packages)
    if found_nonregistry != EXPECTED_NONREGISTRY_PACKAGES:
        missing = sorted(EXPECTED_NONREGISTRY_PACKAGES - found_nonregistry)
        unexpected = sorted(found_nonregistry - EXPECTED_NONREGISTRY_PACKAGES)
        raise GenerationError(
            "the reviewed non-registry package set changed: "
            f"missing={missing!r}, unexpected={unexpected!r}"
        )
    return Upstream(lock_bytes, rust_pin_bytes, tuple(nonregistry_packages))


def _locked_packages(lock_bytes: bytes) -> list[LockedPackage]:
    parsed = tomllib.loads(lock_bytes.decode("utf-8"))
    packages: list[LockedPackage] = []
    for raw in parsed.get("package", []):
        if not isinstance(raw, dict) or raw.get("source") != REGISTRY_SOURCE:
            continue
        name = raw.get("name")
        version = raw.get("version")
        checksum = raw.get("checksum")
        dependencies = raw.get("dependencies", [])
        if not (
            isinstance(name, str)
            and isinstance(version, str)
            and isinstance(checksum, str)
            and re.fullmatch(r"[0-9a-f]{64}", checksum)
            and isinstance(dependencies, list)
            and all(isinstance(value, str) for value in dependencies)
        ):
            raise GenerationError("a registry package in the workerd lockfile is malformed")
        packages.append(LockedPackage(name, version, checksum, tuple(dependencies)))
    packages.sort(key=lambda item: (item.name.casefold(), item.name, item.version, item.checksum))
    if len(packages) != EXPECTED_REGISTRY_PACKAGES:
        raise GenerationError(
            f"expected {EXPECTED_REGISTRY_PACKAGES} registry crates, found {len(packages)}"
        )
    identities = {(item.name, item.version, item.checksum) for item in packages}
    if len(identities) != len(packages):
        raise GenerationError("the workerd lockfile contains duplicate registry identities")
    return packages


def _cache_directory() -> Path:
    explicit = os.environ.get("WORKERD_RUST_CRATE_CACHE")
    if explicit:
        return Path(explicit).expanduser().resolve()
    xdg = os.environ.get("XDG_CACHE_HOME")
    base = Path(xdg).expanduser() if xdg else Path.home() / ".cache"
    return (base / "ha-voice-hermes" / "workerd-rust-crates").resolve()


def _archive(locked: LockedPackage, cache: Path) -> bytes:
    cache.mkdir(parents=True, exist_ok=True)
    path = cache / f"{locked.name}-{locked.version}-{locked.checksum}.crate"
    if path.exists():
        content = path.read_bytes()
        found = _sha256(content)
        if found != locked.checksum:
            raise GenerationError(
                f"cached crate checksum mismatch for {locked.name} {locked.version}: "
                f"expected {locked.checksum}, found {found}"
            )
        return content

    content = _download(locked.archive_url)
    found = _sha256(content)
    if found != locked.checksum:
        raise GenerationError(
            f"downloaded crate checksum mismatch for {locked.name} {locked.version}: "
            f"expected {locked.checksum}, found {found}"
        )
    with tempfile.NamedTemporaryFile(dir=cache, delete=False) as temporary:
        temporary.write(content)
        temporary_path = Path(temporary.name)
    os.chmod(temporary_path, 0o644)
    os.replace(temporary_path, path)
    return content


def _archives(locked: list[LockedPackage]) -> dict[tuple[str, str, str], bytes]:
    cache = _cache_directory()
    workers = min(12, max(1, len(locked)))
    with ThreadPoolExecutor(max_workers=workers) as executor:
        contents = list(executor.map(lambda item: _archive(item, cache), locked))
    return {
        (item.name, item.version, item.checksum): content
        for item, content in zip(locked, contents, strict=True)
    }


def _notice(name: str, content: bytes, provenance: str) -> Notice:
    if not content or len(content) > MAX_NOTICE_BYTES or b"\x00" in content:
        raise GenerationError(f"notice is empty, oversized, or non-textual: {name}")
    try:
        content.decode("utf-8")
    except UnicodeDecodeError as error:
        raise GenerationError(f"notice is not UTF-8: {name}") from error
    return Notice(name, content, _sha256(content), provenance)


def _package(locked: LockedPackage, archive_bytes: bytes) -> Package:
    prefix = f"{locked.name}-{locked.version}"
    try:
        with tarfile.open(fileobj=io.BytesIO(archive_bytes), mode="r:*") as archive:
            members: dict[str, tarfile.TarInfo] = {}
            for member in archive.getmembers():
                if not member.isfile():
                    continue
                if member.name in members:
                    raise GenerationError(
                        f"crate archive contains a duplicate file: {locked.name}/{member.name}"
                    )
                members[member.name] = member
            manifest_name = f"{prefix}/Cargo.toml"
            manifest_member = members.get(manifest_name)
            if manifest_member is None or manifest_member.size > MAX_NOTICE_BYTES:
                raise GenerationError(f"crate manifest is missing for {locked.name} {locked.version}")
            manifest_file = archive.extractfile(manifest_member)
            if manifest_file is None:
                raise GenerationError(f"crate manifest cannot be read for {locked.name}")
            manifest = tomllib.loads(manifest_file.read().decode("utf-8"))
            metadata = manifest.get("package")
            if not isinstance(metadata, dict):
                raise GenerationError(f"crate package metadata is missing for {locked.name}")
            if metadata.get("name") != locked.name or metadata.get("version") != locked.version:
                raise GenerationError(f"crate identity differs from Cargo.lock for {locked.name}")
            license_expression = metadata.get("license")
            if not isinstance(license_expression, str) or not license_expression.strip():
                raise GenerationError(f"crate has no license expression: {locked.name}")
            repository = metadata.get("repository") or (
                f"https://crates.io/crates/{locked.name}/{locked.version}"
            )
            if not isinstance(repository, str):
                raise GenerationError(f"crate repository metadata is malformed: {locked.name}")

            declared_license = metadata.get("license-file")
            if declared_license is not None and not isinstance(declared_license, str):
                raise GenerationError(f"crate license-file metadata is malformed: {locked.name}")
            notice_members: dict[str, object] = {}
            for member_name, member in members.items():
                relative = PurePosixPath(member_name)
                if (
                    len(relative.parts) == 2
                    and relative.parts[0] == prefix
                    and NOTICE_FILE_RE.fullmatch(relative.name)
                ):
                    notice_members[relative.name] = member
            if declared_license:
                declared = PurePosixPath(declared_license)
                if declared.is_absolute() or ".." in declared.parts:
                    raise GenerationError(f"crate license-file escapes archive: {locked.name}")
                member_name = str(PurePosixPath(prefix) / declared)
                member = members.get(member_name)
                if member is None:
                    raise GenerationError(f"declared crate license-file is absent: {locked.name}")
                notice_members[str(declared)] = member

            notices: list[Notice] = []
            for relative_name in sorted(
                notice_members, key=lambda value: (value.casefold(), value)
            ):
                member = notice_members[relative_name]
                if not hasattr(member, "size") or member.size > MAX_NOTICE_BYTES:
                    raise GenerationError(f"crate notice is oversized: {locked.name}/{relative_name}")
                extracted = archive.extractfile(member)
                if extracted is None:
                    raise GenerationError(f"crate notice cannot be read: {locked.name}/{relative_name}")
                notices.append(
                    _notice(
                        relative_name,
                        extracted.read(),
                        f"exact crates.io archive file {relative_name}",
                    )
                )

            vcs_commit = ""
            vcs_path = ""
            vcs_dirty = False
            vcs_member = members.get(f"{prefix}/.cargo_vcs_info.json")
            if vcs_member is not None:
                if vcs_member.size > MAX_NOTICE_BYTES:
                    raise GenerationError(f"crate VCS metadata is oversized: {locked.name}")
                vcs_file = archive.extractfile(vcs_member)
                if vcs_file is None:
                    raise GenerationError(f"crate VCS metadata cannot be read: {locked.name}")
                vcs = json.loads(vcs_file.read())
                git = vcs.get("git")
                if not isinstance(git, dict) or not isinstance(git.get("sha1"), str):
                    raise GenerationError(f"crate VCS metadata is malformed: {locked.name}")
                vcs_commit = git["sha1"]
                if not re.fullmatch(r"[0-9a-f]{40}", vcs_commit):
                    raise GenerationError(f"crate VCS provenance is untrusted: {locked.name}")
                raw_dirty = git.get("dirty", False)
                if not isinstance(raw_dirty, bool):
                    raise GenerationError(f"crate VCS dirty flag is malformed: {locked.name}")
                vcs_dirty = raw_dirty
                raw_path = vcs.get("path_in_vcs", "")
                if not isinstance(raw_path, str):
                    raise GenerationError(f"crate VCS path is malformed: {locked.name}")
                vcs_path = raw_path
    except (tarfile.TarError, UnicodeDecodeError, tomllib.TOMLDecodeError, json.JSONDecodeError) as error:
        raise GenerationError(f"could not inspect crate archive: {locked.name}") from error

    return Package(
        name=locked.name,
        version=locked.version,
        checksum=locked.checksum,
        license_expression=license_expression,
        archive_url=locked.archive_url,
        repository=repository,
        vcs_commit=vcs_commit,
        vcs_path=vcs_path,
        vcs_dirty=vcs_dirty,
        notices=tuple(notices),
    )


def _external_fallback_notices() -> dict[str, Notice]:
    notices: dict[str, Notice] = {}
    for key, (url, expected_hash) in EXTERNAL_FALLBACKS.items():
        content = _download(url, maximum=MAX_NOTICE_BYTES)
        found = _sha256(content)
        if found != expected_hash:
            raise GenerationError(
                f"external fallback notice changed for {key}: expected {expected_hash}, found {found}"
            )
        notices[key] = _notice(f"fallback/{key}", content, f"pinned fallback source {url}")
    return notices


def _run(command: list[str], environment: dict[str, str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        cwd=ROOT,
        env=environment,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )


def _rustc_command() -> tuple[list[str], dict[str, str]]:
    environment = os.environ.copy()
    command = shlex.split(environment.get("RUSTC", "rustc"))
    if not command:
        raise GenerationError("RUSTC resolved to an empty command")
    probe = _run([*command, "--version"], environment)
    if probe.returncode != 0:
        raise GenerationError("rustc --version failed")
    if probe.stdout.startswith("rustup ") and "RUSTC" not in environment:
        rustup = shutil.which("rustup")
        if rustup is None:
            raise GenerationError("rustc resolved to rustup, but rustup is unavailable")
        located = _run([rustup, "which", "rustc"], environment)
        if located.returncode != 0:
            raise GenerationError("rustup could not locate the active rustc toolchain")
        command = [located.stdout.strip()]
    return command, environment


def _rust_toolchain() -> RustToolchain:
    rustc, environment = _rustc_command()
    verbose = _run([*rustc, "-vV"], environment)
    if verbose.returncode != 0:
        raise GenerationError("rustc -vV failed")
    fields: dict[str, str] = {}
    for line in verbose.stdout.splitlines():
        if ": " in line:
            key, value = line.split(": ", 1)
            fields[key] = value
    release = fields.get("release", "")
    commit_hash = fields.get("commit-hash", "")
    commit_date = fields.get("commit-date", "")
    llvm_version = fields.get("LLVM version", "")
    if release != EXPECTED_RUST_RELEASE or commit_hash != EXPECTED_RUST_COMMIT:
        raise GenerationError("active rustc does not match workerd's reviewed Rust 1.96.0 pin")
    if not re.fullmatch(r"\d{4}-\d{2}-\d{2}", commit_date) or not re.fullmatch(
        r"\d+(?:\.\d+)+", llvm_version
    ):
        raise GenerationError("rustc omitted commit-date or LLVM provenance")
    sysroot = _run([*rustc, "--print", "sysroot"], environment)
    if sysroot.returncode != 0:
        raise GenerationError("rustc --print sysroot failed")
    documentation_root = Path(sysroot.stdout.strip()).resolve() / "share" / "doc" / "rust"
    notices: list[Notice] = []
    for relative_name in RUST_NOTICE_PATHS:
        path = (documentation_root / relative_name).resolve()
        try:
            path.relative_to(documentation_root)
        except ValueError as error:
            raise GenerationError(f"Rust notice path escapes sysroot: {relative_name}") from error
        try:
            content = path.read_bytes()
        except OSError as error:
            raise GenerationError(f"missing Rust notice: {relative_name}") from error
        notices.append(
            _notice(
                f"rust/{relative_name}",
                content,
                f"Rust {release} distribution file share/doc/rust/{relative_name}",
            )
        )
    return RustToolchain(release, commit_hash, commit_date, llvm_version, tuple(notices))


def _apply_notice_fallbacks(
    packages: list[Package], rust: RustToolchain, external: dict[str, Notice]
) -> list[Package]:
    missing = [package for package in packages if not package.notices]
    if len(missing) != EXPECTED_PACKAGES_WITHOUT_ARCHIVE_NOTICE:
        raise GenerationError(
            "the number of crates without archive notices changed: "
            f"expected {EXPECTED_PACKAGES_WITHOUT_ARCHIVE_NOTICE}, found {len(missing)}"
        )
    apache = next(
        (notice for notice in rust.notices if notice.name == "rust/licenses/Apache-2.0.txt"),
        None,
    )
    cssparser = next(
        (package for package in packages if package.name == "cssparser" and package.version == "0.36.0"),
        None,
    )
    mpl = None if cssparser is None else next(
        (notice for notice in cssparser.notices if notice.sha256 == "fab3dd6bdab226f1c08630b1dd917e11fcb4ec5e1e020e2c16f83a0a13863e85"),
        None,
    )
    if apache is None or mpl is None:
        raise GenerationError("canonical Apache or MPL fallback notice is unavailable")

    completed: list[Package] = []
    for package in packages:
        if package.notices:
            completed.append(package)
            continue
        external_key = PACKAGE_EXTERNAL_FALLBACK.get(package.name)
        if external_key:
            fallback = external.get(external_key)
            if fallback is None:
                raise GenerationError(f"external notice fallback is missing: {package.name}")
        elif package.license_expression == "MPL-2.0":
            fallback = replace(
                mpl,
                name="fallback/MPL-2.0.txt",
                provenance="canonical MPL-2.0 text from exact cssparser 0.36.0 crate archive",
            )
        elif "Apache-2.0" in package.license_expression:
            fallback = replace(
                apache,
                name="fallback/Apache-2.0.txt",
                provenance=(
                    "canonical Apache-2.0 text from workerd's pinned Rust 1.96.0 distribution; "
                    "selected under the crate's stated OR expression where applicable"
                ),
            )
        else:
            raise GenerationError(f"no reviewed notice fallback exists for {package.name}")
        completed.append(replace(package, notices=(fallback,), used_fallback=True))
    return completed


def _packages(lock_bytes: bytes, rust: RustToolchain) -> list[Package]:
    locked = _locked_packages(lock_bytes)
    archives = _archives(locked)
    packages = [
        _package(item, archives[(item.name, item.version, item.checksum)]) for item in locked
    ]
    external = _external_fallback_notices()
    packages = _apply_notice_fallbacks(packages, rust, external)
    mpl = {
        (package.name, package.version)
        for package in packages
        if "MPL-2.0" in package.license_expression
    }
    if mpl != EXPECTED_MPL_PACKAGES:
        raise GenerationError(f"reviewed MPL package set changed: {sorted(mpl)}")
    return packages


def _source_archive(repository: SourceRepository) -> bytes:
    cache = _cache_directory() / "git-sources"
    cache.mkdir(parents=True, exist_ok=True)
    path = cache / f"{repository.name}-{repository.commit}-{repository.archive_sha256}.tar.gz"
    if path.exists():
        content = path.read_bytes()
        found = _sha256(content)
        if found != repository.archive_sha256:
            raise GenerationError(
                f"cached source archive checksum mismatch for {repository.name}: "
                f"expected {repository.archive_sha256}, found {found}"
            )
        return content
    content = _download(repository.archive_url)
    found = _sha256(content)
    if found != repository.archive_sha256:
        raise GenerationError(
            f"source archive checksum mismatch for {repository.name}: "
            f"expected {repository.archive_sha256}, found {found}"
        )
    with tempfile.NamedTemporaryFile(dir=cache, delete=False) as temporary:
        temporary.write(content)
        temporary_path = Path(temporary.name)
    os.chmod(temporary_path, 0o644)
    os.replace(temporary_path, path)
    return content


def _source_archive_files(
    repository: SourceRepository, content: bytes
) -> dict[str, bytes]:
    wanted = {
        repository.license_path,
        *(manifest for _, _, manifest in repository.package_manifests),
    }
    if repository.name == "ruff":
        wanted.add("Cargo.toml")
    found: dict[str, bytes] = {}
    seen: set[str] = set()
    try:
        with tarfile.open(fileobj=io.BytesIO(content), mode="r:gz") as archive:
            for member in archive.getmembers():
                path = PurePosixPath(member.name)
                if path.is_absolute() or ".." in path.parts:
                    raise GenerationError(
                        f"source archive contains an escaping path: {repository.name}"
                    )
                if member.name in seen:
                    raise GenerationError(
                        f"source archive contains a duplicate member: "
                        f"{repository.name}/{member.name}"
                    )
                seen.add(member.name)
                if not member.isfile() or not path.parts or path.parts[0] != repository.prefix:
                    continue
                relative = str(PurePosixPath(*path.parts[1:]))
                if relative not in wanted:
                    continue
                if member.size > MAX_NOTICE_BYTES:
                    raise GenerationError(
                        f"reviewed source file is oversized: {repository.name}/{relative}"
                    )
                extracted = archive.extractfile(member)
                if extracted is None:
                    raise GenerationError(
                        f"reviewed source file cannot be read: {repository.name}/{relative}"
                    )
                found[relative] = extracted.read()
    except tarfile.TarError as error:
        raise GenerationError(
            f"could not inspect exact source archive for {repository.name}"
        ) from error
    missing = sorted(wanted - found.keys())
    if missing:
        raise GenerationError(
            f"exact source archive omitted reviewed paths for {repository.name}: {missing!r}"
        )
    return found


def _source_manifest_value(
    metadata: dict[str, object],
    workspace_metadata: dict[str, object],
    key: str,
) -> tuple[str, str] | None:
    value = metadata.get(key)
    if isinstance(value, str):
        return value, f"package manifest `{key}`"
    if isinstance(value, dict) and value.get("workspace") is True:
        inherited = workspace_metadata.get(key)
        if not isinstance(inherited, str):
            raise GenerationError(f"workspace-inherited package {key} is missing")
        return inherited, f"workspace.package `{key}`"
    if value is None:
        return None
    raise GenerationError(f"source package {key} metadata is malformed")


def _verify_source_repository_tag(repository: SourceRepository) -> None:
    remote_commit = _git_tag_commit(repository.repository, repository.tag)
    if remote_commit != repository.commit:
        raise GenerationError(
            f"remote {repository.name} tag {repository.tag} moved: "
            f"expected {repository.commit}, found {remote_commit}"
        )


def _source_packages(upstream: Upstream) -> tuple[list[SourcePackage], int]:
    lock_sources = {
        (name, version): source for name, version, source in upstream.nonregistry_packages
    }
    packages: list[SourcePackage] = []
    for repository in SOURCE_REPOSITORIES:
        if repository.name != "workerd":
            _verify_source_repository_tag(repository)
        archive = _source_archive(repository)
        files = _source_archive_files(repository, archive)
        license_content = files[repository.license_path]
        license_hash = _sha256(license_content)
        if license_hash != repository.license_sha256:
            raise GenerationError(
                f"root license hash changed for {repository.name}: "
                f"expected {repository.license_sha256}, found {license_hash}"
            )
        root_manifest: dict[str, object] = {}
        if "Cargo.toml" in files:
            try:
                root_manifest = tomllib.loads(files["Cargo.toml"].decode("utf-8"))
            except (UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
                raise GenerationError(
                    f"root source manifest is invalid for {repository.name}"
                ) from error
        raw_workspace = root_manifest.get("workspace", {})
        workspace = raw_workspace if isinstance(raw_workspace, dict) else {}
        raw_workspace_metadata = workspace.get("package", {})
        workspace_metadata = (
            raw_workspace_metadata if isinstance(raw_workspace_metadata, dict) else {}
        )
        notice = _notice(
            f"{repository.name}/{repository.license_path}",
            license_content,
            (
                f"exact {repository.name} commit {repository.commit} source archive "
                f"file {repository.license_path}"
            ),
        )
        for name, version, manifest_path in repository.package_manifests:
            lock_source = lock_sources.get((name, version))
            if lock_source is None:
                raise GenerationError(
                    f"reviewed source package is absent from Cargo.lock: {name} {version}"
                )
            try:
                manifest = tomllib.loads(files[manifest_path].decode("utf-8"))
            except (UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
                raise GenerationError(f"source manifest is invalid: {manifest_path}") from error
            metadata = manifest.get("package")
            if not isinstance(metadata, dict):
                raise GenerationError(f"source package metadata is missing: {manifest_path}")
            if metadata.get("name") != name or metadata.get("version") != version:
                raise GenerationError(
                    f"source package identity differs from Cargo.lock: {manifest_path}"
                )
            declared_license = _source_manifest_value(
                metadata, workspace_metadata, "license"
            )
            if declared_license is None:
                license_expression = repository.default_license
                license_basis = (
                    f"repository-root `{repository.license_path}` applies to this "
                    "non-published package; its manifest has no license field"
                )
            else:
                license_expression, license_basis = declared_license
            if license_expression != repository.default_license:
                raise GenerationError(
                    f"reviewed license expression changed for {name}: {license_expression}"
                )
            declared_repository = _source_manifest_value(
                metadata, workspace_metadata, "repository"
            )
            repository_url = (
                declared_repository[0]
                if declared_repository is not None
                else repository.repository.removesuffix(".git")
            )
            packages.append(
                SourcePackage(
                    name=name,
                    version=version,
                    lock_source=lock_source,
                    source_kind="path" if lock_source == "path dependency" else "git",
                    license_expression=license_expression,
                    license_basis=license_basis,
                    repository=repository_url,
                    tag=repository.tag,
                    commit=repository.commit,
                    archive_url=repository.archive_url,
                    archive_sha256=repository.archive_sha256,
                    manifest_path=manifest_path,
                    notices=(notice,),
                )
            )

    packages.sort(key=lambda item: (item.name.casefold(), item.name, item.version))
    resolved = {(item.name, item.version, item.lock_source) for item in packages}
    expected = set(upstream.nonregistry_packages)
    unresolved = expected - resolved
    unexpected = resolved - expected
    git_count = sum(item.source_kind == "git" for item in packages)
    path_count = sum(item.source_kind == "path" for item in packages)
    if unexpected:
        raise GenerationError(f"unexpected source package coverage: {sorted(unexpected)!r}")
    if git_count != EXPECTED_GIT_PACKAGES or path_count != EXPECTED_PATH_PACKAGES:
        raise GenerationError(
            "reviewed source package counts changed: "
            f"git={git_count}, path={path_count}"
        )
    return packages, len(unresolved)


def _cell(value: str) -> str:
    return value.replace("|", "\\|").replace("\r", " ").replace("\n", " ")


def _render(
    upstream: Upstream,
    packages: list[Package],
    source_packages: list[SourcePackage],
    rust: RustToolchain,
    unresolved_count: int,
) -> bytes:
    content_by_hash: dict[str, bytes] = {}
    users_by_hash: dict[str, list[tuple[str, str, str]]] = {}
    for package in packages:
        for notice in package.notices:
            existing = content_by_hash.setdefault(notice.sha256, notice.content)
            if existing != notice.content:
                raise GenerationError(f"SHA-256 collision for notice {notice.sha256}")
            users_by_hash.setdefault(notice.sha256, []).append(
                (f"{package.name} {package.version}", notice.name, notice.provenance)
            )
    for package in source_packages:
        for notice in package.notices:
            existing = content_by_hash.setdefault(notice.sha256, notice.content)
            if existing != notice.content:
                raise GenerationError(f"SHA-256 collision for notice {notice.sha256}")
            users_by_hash.setdefault(notice.sha256, []).append(
                (f"{package.name} {package.version}", notice.name, notice.provenance)
            )
    for notice in rust.notices:
        existing = content_by_hash.setdefault(notice.sha256, notice.content)
        if existing != notice.content:
            raise GenerationError(f"SHA-256 collision for notice {notice.sha256}")
        users_by_hash.setdefault(notice.sha256, []).append(
            (f"Rust {rust.release} standard library/runtime", notice.name, notice.provenance)
        )

    archive_notice_count = sum(not package.used_fallback for package in packages)
    source_notice_files = sum(len(package.notices) for package in packages)
    header = [
        "# workerd Rust third-party source inventory and notices",
        "",
        "> Generated by `scripts/generate-workerd-rust-third-party-notices.py`; do not edit.",
        "> `--release-check` verifies live remote tags, immutable source hashes, crate",
        "> checksums, Rust pin, zero unresolved packages, source directions, and checked-in bytes.",
        "",
        "This bundle deliberately includes all crates.io packages in workerd's exact",
        "Rust lockfile and every exact git/path entry, whether or not binary reachability",
        "can be proven after native link-time optimization. Over-inclusion avoids using",
        "symbol stripping as a",
        "license decision. The separately licensed native C++ dependencies and workerd",
        "project source are inventoried elsewhere in the container notice bundle.",
        "The registry and non-registry counts remain separate so git/path packages are",
        "not misrepresented as crates.io releases.",
        "",
        "## Immutable upstream provenance",
        "",
        f"- Repository: `{WORKERD_REPOSITORY}`",
        f"- Tag: `{WORKERD_TAG}` (verified live as a lightweight tag)",
        f"- Commit: `{WORKERD_COMMIT}`",
        f"- `deps/rust/Cargo.lock`: {WORKERD_LOCK_URL}",
        f"- Lockfile SHA-256: `{WORKERD_LOCK_SHA256}`",
        f"- `build/deps/rust.MODULE.bazel`: {WORKERD_RUST_PIN_URL}",
        f"- Rust pin SHA-256: `{WORKERD_RUST_PIN_SHA256}`",
        f"- Registry crates inventoried: **{len(packages)}**",
        f"- Exact git packages inventoried: **{sum(item.source_kind == 'git' for item in source_packages)}**",
        f"- Exact path packages inventoried: **{sum(item.source_kind == 'path' for item in source_packages)}**",
        f"- Unresolved lockfile packages: **{unresolved_count}**",
        f"- Exact crate notice files/references after fallback: **{source_notice_files}**",
        f"- Crates with their own archive notice: **{archive_notice_count}**",
        f"- Reviewed notice fallbacks: **{sum(item.used_fallback for item in packages)}**",
        f"- Unique verbatim notice texts including Rust: **{len(content_by_hash)}**",
        "",
        "## Exact git and path package inventory",
        "",
        "Every non-registry lock entry is resolved below to an exact commit archive,",
        "package manifest path, declared or repository-root license, and verbatim notice.",
        "The archive SHA-256 binds the complete Source Code Form. Download the exact URL,",
        "verify it with `sha256sum FILE.tar.gz`, then extract it with",
        "`tar -xzf FILE.tar.gz`. Preserve the repository-relative manifest and license",
        "paths shown here when providing corresponding source.",
        "",
        "`direct-cargo-bazel-deps` is workerd's metadata-only synthetic package for Bazel",
        "Cargo dependency resolution. Its exact manifest says this explicitly and points",
        "at `fake.rs`; that file is absent from the pinned upstream tree because this",
        "synthetic crate is not compiled. The exact manifest and repository Apache-2.0",
        "license are nevertheless covered here.",
        "",
        "| Package | Kind | License / basis | Exact Source Code Form | Archive SHA-256 | Tag / commit | Package path | Notice |",
        "|---|---|---|---|---|---|---|---|",
    ]
    for package in source_packages:
        notice = package.notices[0]
        header.append(
            f"| `{package.name} {package.version}` | `{package.source_kind}` | "
            f"`{_cell(package.license_expression)}` — {_cell(package.license_basis)} | "
            f"{package.archive_url} | `{package.archive_sha256}` | "
            f"`{package.tag}` / `{package.commit}` | `{package.manifest_path}` | "
            f"`{notice.name}`@`{notice.sha256[:16]}` |"
        )

    header.extend(
        [
            "",
            "Lock-source identities covered by the table above:",
            "",
        ]
    )
    for name, version, source in upstream.nonregistry_packages:
        header.append(f"- `{name} {version}` — `{source}`")

    header.extend(
        [
            "",
            "## Rust standard-library/runtime provenance",
            "",
            "The exact workerd commit pins Rust 1.96.0 (LLVM 22). The release-generated",
            "Rust standard-library copyright inventory and applicable Rust, Unicode,",
            "BSD, and Apache-with-LLVM-exception texts are reproduced below.",
            "",
            f"- Rust release: `{rust.release}`",
            f"- Rust compiler source commit: `{rust.commit_hash}`",
            f"- Rust compiler commit date: `{rust.commit_date}`",
            f"- LLVM version reported by rustc: `{rust.llvm_version}`",
            "- Notice references:",
        ]
    )
    for notice in rust.notices:
        header.append(f"  - `{notice.name}`@`{notice.sha256[:16]}`")

    header.extend(
        [
            "",
            "## MPL-2.0 source-code availability",
            "",
            "The exact crates.io `.crate` links below are the unmodified Source Code",
            "Form inputs selected by workerd's lockfile. Download the named archive and",
            "verify its SHA-256 against this table; `.crate` files are gzip-compressed",
            "tar archives and can be extracted with `tar -xzf FILE.crate`. This bundle",
            "and these directions must remain available with distributions of the",
            "executable. A distributor that modifies MPL-covered files must make the",
            "corresponding modified Source Code Form available under MPL-2.0 and retain",
            "the required notices. No modifications to these registry sources are made",
            "by this repository.",
            "",
            "| Package | License expression | Exact Source Code Form | SHA-256 | Repository |",
            "|---|---|---|---|---|",
        ]
    )
    for package in packages:
        if "MPL-2.0" not in package.license_expression:
            continue
        header.append(
            f"| `{package.name} {package.version}` | `{_cell(package.license_expression)}` | "
            f"{package.archive_url} | `{package.checksum}` | {_cell(package.repository)} |"
        )

    header.extend(
        [
            "",
            "## Exact crates.io inventory",
            "",
            "Every source archive URL is checksum-bound by the exact upstream lockfile.",
            "`VCS` is the optional commit/path embedded by Cargo in the crate archive.",
            "A notice marked `fallback` means that the exact archive omitted a top-level",
            "notice; the bundle transparently uses the reviewed source stated in that",
            "notice's provenance rather than pretending the file was present.",
            "",
            "| Package | Version | License expression | Cargo checksum | Exact source archive | Repository / VCS | Notice references |",
            "|---|---:|---|---|---|---|---|",
        ]
    )
    for package in packages:
        vcs = package.repository
        if package.vcs_commit:
            vcs += f" @ `{package.vcs_commit}`"
            if package.vcs_path:
                vcs += f" / `{package.vcs_path}`"
            if package.vcs_dirty:
                vcs += " / `publisher marked dirty`"
        references = "; ".join(
            f"`{_cell(notice.name)}`@`{notice.sha256[:16]}`" for notice in package.notices
        )
        header.append(
            f"| `{package.name}` | `{package.version}` | "
            f"`{_cell(package.license_expression)}` | `{package.checksum}` | "
            f"{package.archive_url} | {_cell(vcs)} | {references} |"
        )

    header.extend(
        [
            "",
            "## Reviewed notice fallbacks",
            "",
            "These exact crate archives contained no top-level notice/license file.",
            "Their selected fallback and its provenance are recorded explicitly:",
            "",
        ]
    )
    for package in packages:
        if not package.used_fallback:
            continue
        notice = package.notices[0]
        header.append(
            f"- `{package.name} {package.version}` (`{_cell(package.license_expression)}`): "
            f"`{notice.name}` sha256:`{notice.sha256}` — {_cell(notice.provenance)}."
        )

    header.extend(
        [
            "",
            "## Verbatim notice texts",
            "",
            "Texts are deduplicated by complete SHA-256. Bytes between each `BEGIN`",
            "and `END` marker are preserved from the named checked source. If a source",
            "lacked a final newline, the generator adds only the separator before `END`.",
            "",
        ]
    )

    output = bytearray(("\n".join(header) + "\n").encode("utf-8"))
    for digest in sorted(content_by_hash):
        users = sorted(users_by_hash[digest], key=lambda item: (item[0].casefold(), item))
        output.extend(f"### SHA-256 `{digest}`\n\nUsed by:\n\n".encode("utf-8"))
        for owner, name, provenance in users:
            output.extend(
                f"- `{owner}` — `{name}` — {_cell(provenance)}\n".encode("utf-8")
            )
        output.extend(f"\n--- BEGIN VERBATIM NOTICE {digest} ---\n".encode("utf-8"))
        content = content_by_hash[digest]
        output.extend(content)
        if not content.endswith(b"\n"):
            output.extend(b"\n")
        output.extend(f"--- END VERBATIM NOTICE {digest} ---\n\n".encode("utf-8"))
    return bytes(output)


def generate() -> Generation:
    upstream = _upstream()
    rust = _rust_toolchain()
    packages = _packages(upstream.lock_bytes, rust)
    source_packages, unresolved_count = _source_packages(upstream)
    notices = _render(
        upstream, packages, source_packages, rust, unresolved_count
    )
    return Generation(notices, upstream.lock_bytes, unresolved_count)


def _check(notices: bytes, lock: bytes) -> int:
    failures: list[str] = []
    for path, expected in ((OUTPUT, notices), (LOCK_OUTPUT, lock)):
        try:
            current = path.read_bytes()
        except FileNotFoundError:
            failures.append(f"missing {path.relative_to(ROOT)}")
            continue
        if current != expected:
            failures.append(
                f"stale {path.relative_to(ROOT)}: expected sha256:{_sha256(expected)}, "
                f"found sha256:{_sha256(current)}"
            )
    if failures:
        print("\n".join(failures), file=sys.stderr)
        print(
            "Run scripts/generate-workerd-rust-third-party-notices.py --update.",
            file=sys.stderr,
        )
        return 1
    print(f"{OUTPUT_DIRECTORY.relative_to(ROOT)} is current and provenance-verified.")
    return 0


def _atomic_write(path: Path, content: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as temporary:
        temporary.write(content)
        temporary_path = Path(temporary.name)
    os.chmod(temporary_path, 0o644)
    os.replace(temporary_path, path)


def _update(notices: bytes, lock: bytes) -> int:
    _atomic_write(OUTPUT, notices)
    _atomic_write(LOCK_OUTPUT, lock)
    print(
        f"Updated {OUTPUT_DIRECTORY.relative_to(ROOT)} "
        f"(notices sha256:{_sha256(notices)}, lock sha256:{_sha256(lock)})."
    )
    return 0


def _release_check(generation: Generation) -> int:
    if generation.unresolved_count != 0:
        print(
            "workerd Rust release check failed: "
            f"{generation.unresolved_count} unresolved lockfile package(s)",
            file=sys.stderr,
        )
        return 1
    result = _check(generation.notices, generation.lock)
    if result != 0:
        return result
    print("workerd Rust release check passed with zero unresolved packages.")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true", help="verify checked-in outputs")
    mode.add_argument(
        "--release-check",
        action="store_true",
        help="verify checked-in outputs and require zero unresolved packages",
    )
    mode.add_argument("--update", action="store_true", help="replace checked-in outputs")
    mode.add_argument("--stdout", action="store_true", help="write the notice bundle to stdout")
    arguments = parser.parse_args()
    try:
        generation = generate()
    except (GenerationError, OSError, ValueError) as error:
        print(f"workerd Rust notice generation failed: {error}", file=sys.stderr)
        return 1
    if arguments.stdout:
        sys.stdout.buffer.write(generation.notices)
        return 0
    if arguments.update:
        return _update(generation.notices, generation.lock)
    if arguments.release_check:
        return _release_check(generation)
    return _check(generation.notices, generation.lock)


if __name__ == "__main__":
    raise SystemExit(main())
