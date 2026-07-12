#!/usr/bin/env python3
"""Generate the locked wasm32 runtime dependency inventory and notice bundle.

The optimized Worker is committed as object code for the Home Assistant App.
This generator follows Cargo's wasm32 normal-dependency graph, records the
exact locked package metadata, and preserves every top-level license/notice
file shipped in each crate.  It intentionally includes all alternatives for
dual-licensed crates: a slightly larger notice bundle is preferable to silently
choosing a license on a dependency author's behalf.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
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


ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "gateway" / "Cargo.toml"
LOCKFILE = ROOT / "gateway" / "Cargo.lock"
TOOLCHAIN_FILE = ROOT / "rust-toolchain.toml"
OUTPUT = (
    ROOT
    / "ha_voice_hermes_gateway"
    / "licenses"
    / "wasm"
    / "THIRD_PARTY_NOTICES.md"
)
TARGET = "wasm32-unknown-unknown"
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
LICENSE_FILE_RE = re.compile(
    r"^(?:licen[cs]e|copying|notice|copyright|unlicense)(?:[._-].*)?$",
    re.IGNORECASE,
)


class GenerationError(RuntimeError):
    """The notice bundle could not be generated without guessing."""


@dataclass(frozen=True)
class NoticeFile:
    name: str
    content: bytes
    sha256: str


@dataclass(frozen=True)
class Package:
    name: str
    version: str
    license_expression: str
    cargo_checksum: str
    source: str
    repository: str
    notices: tuple[NoticeFile, ...]


@dataclass(frozen=True)
class RustToolchain:
    release: str
    commit_hash: str
    commit_date: str
    llvm_version: str
    pin_sha256: str
    notices: tuple[NoticeFile, ...]


def _run(command: list[str], *, env: dict[str, str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        cwd=ROOT,
        env=env,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )


def _cargo_command() -> tuple[list[str], dict[str, str]]:
    """Return a cargo command, including a workaround for non-proxy rustup.

    A normal rustup installation dispatches based on argv[0]. Some package
    manager installations expose ``cargo`` as a symlink to a rustup binary that
    reports its own version instead. Detect that case without affecting normal
    CI or an explicitly supplied CARGO command.
    """

    env = os.environ.copy()
    command = shlex.split(env.get("CARGO", "cargo"))
    if not command:
        raise GenerationError("CARGO resolved to an empty command")

    probe = _run([*command, "--version"], env=env)
    if probe.returncode != 0:
        raise GenerationError(f"cargo --version failed: {probe.stderr.strip()}")

    if probe.stdout.startswith("rustup ") and "CARGO" not in env:
        rustup = shutil.which("rustup")
        if rustup is None:
            raise GenerationError("cargo resolved to rustup, but rustup is unavailable")
        cargo_path = _run([rustup, "which", "cargo"], env=env)
        rustc_path = _run([rustup, "which", "rustc"], env=env)
        if cargo_path.returncode != 0 or rustc_path.returncode != 0:
            raise GenerationError("rustup could not locate the active cargo/rustc toolchain")
        command = [cargo_path.stdout.strip()]
        env.setdefault("RUSTC", rustc_path.stdout.strip())

    return command, env


def _rustc_command() -> tuple[list[str], dict[str, str]]:
    env = os.environ.copy()
    command = shlex.split(env.get("RUSTC", "rustc"))
    if not command:
        raise GenerationError("RUSTC resolved to an empty command")

    probe = _run([*command, "--version"], env=env)
    if probe.returncode != 0:
        raise GenerationError(f"rustc --version failed: {probe.stderr.strip()}")
    if probe.stdout.startswith("rustup ") and "RUSTC" not in env:
        rustup = shutil.which("rustup")
        if rustup is None:
            raise GenerationError("rustc resolved to rustup, but rustup is unavailable")
        rustc_path = _run([rustup, "which", "rustc"], env=env)
        if rustc_path.returncode != 0:
            raise GenerationError("rustup could not locate the active rustc toolchain")
        command = [rustc_path.stdout.strip()]
    return command, env


def _rust_toolchain() -> RustToolchain:
    try:
        pin_bytes = TOOLCHAIN_FILE.read_bytes()
        pin = tomllib.loads(pin_bytes.decode("utf-8"))
    except (OSError, UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
        raise GenerationError("could not parse rust-toolchain.toml") from error
    toolchain = pin.get("toolchain")
    if not isinstance(toolchain, dict) or toolchain.get("channel") != EXPECTED_RUST_RELEASE:
        raise GenerationError(
            f"rust-toolchain.toml must pin Rust {EXPECTED_RUST_RELEASE}; "
            "update the generator's reviewed provenance constants with the pin"
        )

    rustc, env = _rustc_command()
    verbose = _run([*rustc, "-vV"], env=env)
    if verbose.returncode != 0:
        raise GenerationError(f"rustc -vV failed: {verbose.stderr.strip()}")
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
        raise GenerationError(
            "active rustc does not match the reviewed Rust "
            f"{EXPECTED_RUST_RELEASE} commit {EXPECTED_RUST_COMMIT}"
        )
    if not re.fullmatch(r"\d{4}-\d{2}-\d{2}", commit_date) or not re.fullmatch(
        r"\d+(?:\.\d+)+", llvm_version
    ):
        raise GenerationError("rustc -vV omitted commit-date or LLVM provenance")

    sysroot_result = _run([*rustc, "--print", "sysroot"], env=env)
    if sysroot_result.returncode != 0:
        raise GenerationError(f"rustc --print sysroot failed: {sysroot_result.stderr.strip()}")
    documentation_root = Path(sysroot_result.stdout.strip()).resolve() / "share" / "doc" / "rust"
    notices: list[NoticeFile] = []
    for relative_name in RUST_NOTICE_PATHS:
        path = (documentation_root / relative_name).resolve()
        try:
            path.relative_to(documentation_root)
        except ValueError as error:
            raise GenerationError(f"Rust notice path escapes sysroot: {relative_name}") from error
        try:
            content = path.read_bytes()
        except OSError as error:
            raise GenerationError(f"missing Rust notice file: {relative_name}") from error
        if not content or b"\x00" in content:
            raise GenerationError(f"Rust notice file is empty or non-textual: {relative_name}")
        try:
            content.decode("utf-8")
        except UnicodeDecodeError as error:
            raise GenerationError(f"Rust notice file is not UTF-8: {relative_name}") from error
        notices.append(
            NoticeFile(
                name=relative_name,
                content=content,
                sha256=hashlib.sha256(content).hexdigest(),
            )
        )

    return RustToolchain(
        release=release,
        commit_hash=commit_hash,
        commit_date=commit_date,
        llvm_version=llvm_version,
        pin_sha256=hashlib.sha256(pin_bytes).hexdigest(),
        notices=tuple(notices),
    )


def _metadata() -> dict[str, object]:
    cargo, env = _cargo_command()
    result = _run(
        [
            *cargo,
            "metadata",
            "--locked",
            "--format-version",
            "1",
            "--filter-platform",
            TARGET,
            "--manifest-path",
            str(MANIFEST),
        ],
        env=env,
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip()
        raise GenerationError(f"cargo metadata failed: {detail}")
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise GenerationError("cargo metadata returned invalid JSON") from error


def _normal_dependency_ids(metadata: dict[str, object]) -> tuple[str, set[str]]:
    resolve = metadata.get("resolve")
    if not isinstance(resolve, dict) or not isinstance(resolve.get("root"), str):
        raise GenerationError("cargo metadata did not identify the root package")
    root_id = resolve["root"]
    raw_nodes = resolve.get("nodes")
    if not isinstance(raw_nodes, list):
        raise GenerationError("cargo metadata did not contain a dependency graph")
    nodes = {node["id"]: node for node in raw_nodes if isinstance(node, dict)}
    if root_id not in nodes:
        raise GenerationError("root package is absent from the dependency graph")

    seen = {root_id}
    pending = [root_id]
    while pending:
        node = nodes[pending.pop()]
        for dependency in node.get("deps", []):
            if not isinstance(dependency, dict) or not isinstance(dependency.get("pkg"), str):
                raise GenerationError("cargo metadata contains a malformed dependency")
            dep_kinds = dependency.get("dep_kinds", [])
            is_normal = any(
                isinstance(kind, dict) and kind.get("kind") is None for kind in dep_kinds
            )
            package_id = dependency["pkg"]
            if is_normal and package_id not in seen:
                if package_id not in nodes:
                    raise GenerationError(f"dependency graph is missing {package_id}")
                seen.add(package_id)
                pending.append(package_id)

    seen.remove(root_id)
    if not seen:
        raise GenerationError("wasm32 normal-dependency closure is unexpectedly empty")
    return root_id, seen


def _lock_entries() -> dict[tuple[str, str, str], dict[str, object]]:
    try:
        parsed = tomllib.loads(LOCKFILE.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise GenerationError(f"could not parse {LOCKFILE.relative_to(ROOT)}") from error

    entries: dict[tuple[str, str, str], dict[str, object]] = {}
    for raw in parsed.get("package", []):
        if not isinstance(raw, dict):
            raise GenerationError("Cargo.lock contains a malformed package entry")
        name = raw.get("name")
        version = raw.get("version")
        source = raw.get("source", "")
        if not all(isinstance(value, str) for value in (name, version, source)):
            raise GenerationError("Cargo.lock package identity is malformed")
        key = (name, version, source)
        if key in entries:
            raise GenerationError(f"Cargo.lock contains a duplicate package identity: {key}")
        entries[key] = raw
    return entries


def _validate_notice(name: str, content: bytes) -> NoticeFile:
    if not content or b"\x00" in content:
        raise GenerationError(f"license file is empty or non-textual: {name}")
    try:
        content.decode("utf-8")
    except UnicodeDecodeError as error:
        raise GenerationError(f"license file is not UTF-8: {name}") from error
    return NoticeFile(
        name=name,
        content=content,
        sha256=hashlib.sha256(content).hexdigest(),
    )


def _registry_notice_files(
    package_root: Path, name: str, version: str, checksum: str
) -> tuple[NoticeFile, ...]:
    registry_id = package_root.parent.name
    source_root = package_root.parent.parent
    registry_root = source_root.parent
    if source_root.name != "src" or registry_root.name != "registry":
        raise GenerationError(f"could not locate the registry archive for {name} {version}")
    archive_path = registry_root / "cache" / registry_id / f"{name}-{version}.crate"
    try:
        archive_bytes = archive_path.read_bytes()
    except OSError as error:
        raise GenerationError(f"missing registry archive for {name} {version}") from error
    archive_checksum = hashlib.sha256(archive_bytes).hexdigest()
    if archive_checksum != checksum:
        raise GenerationError(
            f"registry archive checksum mismatch for {name} {version}: "
            f"expected {checksum}, found {archive_checksum}"
        )

    prefix = f"{name}-{version}"
    notices: dict[str, NoticeFile] = {}
    try:
        with tarfile.open(fileobj=io.BytesIO(archive_bytes), mode="r:*") as archive:
            for member in archive.getmembers():
                relative = PurePosixPath(member.name)
                if (
                    len(relative.parts) != 2
                    or relative.parts[0] != prefix
                    or not LICENSE_FILE_RE.fullmatch(relative.name)
                ):
                    continue
                if not member.isfile() or member.size > 2 * 1024 * 1024:
                    raise GenerationError(
                        f"unsupported license archive member for {name} {version}: {relative.name}"
                    )
                if relative.name in notices:
                    raise GenerationError(
                        f"duplicate license archive member for {name} {version}: {relative.name}"
                    )
                extracted = archive.extractfile(member)
                if extracted is None:
                    raise GenerationError(
                        f"could not read license archive member for {name} {version}: {relative.name}"
                    )
                notices[relative.name] = _validate_notice(relative.name, extracted.read())
    except (tarfile.TarError, OSError) as error:
        raise GenerationError(f"could not inspect registry archive for {name} {version}") from error

    if not notices:
        raise GenerationError(f"no top-level license/notice file found for {name} {version}")
    return tuple(notices[key] for key in sorted(notices, key=lambda value: (value.casefold(), value)))


def _notice_files(
    manifest_path: str, name: str, version: str, source: str, checksum: str
) -> tuple[NoticeFile, ...]:
    package_root = Path(manifest_path).resolve().parent
    if not package_root.is_dir():
        raise GenerationError(f"crate source directory is unavailable for {package_root.name}")
    if source.startswith("registry+"):
        return _registry_notice_files(package_root, name, version, checksum)

    candidates = [
        path
        for path in package_root.iterdir()
        if path.is_file() and LICENSE_FILE_RE.fullmatch(path.name)
    ]
    candidates.sort(key=lambda path: (path.name.casefold(), path.name))
    if not candidates:
        raise GenerationError(f"no top-level license/notice file found for {package_root.name}")

    notices: list[NoticeFile] = []
    for path in candidates:
        resolved = path.resolve()
        try:
            resolved.relative_to(package_root)
        except ValueError as error:
            raise GenerationError(f"license path escapes crate source: {path.name}") from error
        notices.append(_validate_notice(path.name, resolved.read_bytes()))
    return tuple(notices)


def _packages() -> list[Package]:
    metadata = _metadata()
    _, dependency_ids = _normal_dependency_ids(metadata)
    raw_packages = metadata.get("packages")
    if not isinstance(raw_packages, list):
        raise GenerationError("cargo metadata did not contain packages")
    by_id = {
        package["id"]: package
        for package in raw_packages
        if isinstance(package, dict) and isinstance(package.get("id"), str)
    }
    lock_entries = _lock_entries()

    packages: list[Package] = []
    for package_id in dependency_ids:
        raw = by_id.get(package_id)
        if raw is None:
            raise GenerationError(f"cargo metadata is missing package {package_id}")
        name = raw.get("name")
        version = raw.get("version")
        source = raw.get("source") or ""
        license_expression = raw.get("license")
        manifest_path = raw.get("manifest_path")
        repository = raw.get("repository") or ""
        if not all(
            isinstance(value, str) and value
            for value in (name, version, license_expression, manifest_path)
        ):
            raise GenerationError(f"package metadata is incomplete for {package_id}")
        if not isinstance(source, str) or not isinstance(repository, str):
            raise GenerationError(f"package source metadata is malformed for {package_id}")

        lock = lock_entries.get((name, version, source))
        if lock is None:
            raise GenerationError(f"{name} {version} is absent from Cargo.lock")
        checksum = lock.get("checksum", "")
        if source.startswith("registry+") and not (
            isinstance(checksum, str) and re.fullmatch(r"[0-9a-f]{64}", checksum)
        ):
            raise GenerationError(f"{name} {version} has no valid locked registry checksum")
        if not isinstance(checksum, str):
            raise GenerationError(f"{name} {version} has a malformed checksum")

        packages.append(
            Package(
                name=name,
                version=version,
                license_expression=license_expression,
                cargo_checksum=checksum or "not provided for non-registry source",
                source=source or "path dependency",
                repository=repository or f"https://crates.io/crates/{name}/{version}",
                notices=_notice_files(manifest_path, name, version, source, checksum),
            )
        )

    packages.sort(key=lambda item: (item.name.casefold(), item.name, item.version, item.source))
    return packages


def _markdown_cell(value: str) -> str:
    return value.replace("|", "\\|").replace("\r", " ").replace("\n", " ")


def _render(packages: list[Package], rust: RustToolchain) -> bytes:
    content_by_hash: dict[str, bytes] = {}
    users_by_hash: dict[str, list[tuple[str, str]]] = {}
    cargo_notice_count = 0
    for package in packages:
        for notice in package.notices:
            cargo_notice_count += 1
            existing = content_by_hash.setdefault(notice.sha256, notice.content)
            if existing != notice.content:
                raise GenerationError(f"SHA-256 collision for notice {notice.sha256}")
            users_by_hash.setdefault(notice.sha256, []).append(
                (f"{package.name} {package.version}", notice.name)
            )
    for notice in rust.notices:
        existing = content_by_hash.setdefault(notice.sha256, notice.content)
        if existing != notice.content:
            raise GenerationError(f"SHA-256 collision for notice {notice.sha256}")
        users_by_hash.setdefault(notice.sha256, []).append(
            (f"Rust {rust.release} standard library/runtime", notice.name)
        )

    header = [
        "# WASM third-party dependency inventory and notices",
        "",
        "> Generated by `scripts/generate-wasm-third-party-notices.py`; do not edit by hand.",
        "> Run the generator with `--update` after any Rust dependency change and",
        "> use `--check` before committing or distributing the optimized WASM.",
        "",
        "This bundle covers the complete Cargo **normal-dependency** closure selected",
        f"for `{TARGET}` from `gateway/Cargo.lock`. It excludes this repository's",
        "first-party gateway package and conservatively preserves every top-level",
        "license, copying, copyright, notice, and unlicense file shipped by each",
        "locked crate, including every alternative offered by dual-licensed crates.",
        "The statically linked Rust standard-library/runtime notices are taken from",
        "the exact pinned compiler distribution and preserved too. Build-only tools",
        "and the separately packaged native `workerd` runtime are not part of this",
        "WASM inventory.",
        "",
        f"- Third-party packages: **{len(packages)}**",
        f"- Cargo source notice files: **{cargo_notice_count}**",
        f"- Rust toolchain notice files: **{len(rust.notices)}**",
        f"- Unique verbatim notice texts: **{len(content_by_hash)}**",
        "",
        "## Rust standard-library/runtime provenance",
        "",
        "The optimized WASM statically links Rust library/runtime material that does",
        "not appear in `Cargo.lock`. `COPYRIGHT-library.html` is the Rust release's",
        "generated standard-library inventory; the additional files preserve the",
        "applicable Rust, Unicode, BSD, and Apache-with-LLVM-exception texts.",
        "Host architecture is deliberately omitted because these notice files and",
        "compiler source revision are identical across the pinned release targets.",
        "",
        f"- `rust-toolchain.toml` SHA-256: `{rust.pin_sha256}`",
        f"- Rust release: `{rust.release}`",
        f"- Rust compiler source commit: `{rust.commit_hash}`",
        f"- Rust compiler commit date: `{rust.commit_date}`",
        f"- LLVM version reported by rustc: `{rust.llvm_version}`",
        f"- Linked target: `{TARGET}`",
        "- Notice references:",
    ]

    for notice in rust.notices:
        header.append(f"  - `{notice.name}`@`{notice.sha256[:16]}`")
    header.extend(
        [
            "",
            "## Exact locked inventory",
            "",
        "Cargo checksum is the checksum recorded in `gateway/Cargo.lock` and is",
        "verified against the exact registry `.crate` archive before reading notices.",
        "Notice references are the original crate filenames followed by the first 16 hex",
            "characters of the source file's SHA-256; the complete digest heads the",
            "corresponding verbatim text below.",
            "",
            "| Package | Version | License expression | Cargo checksum | Upstream | Notice references |",
            "|---|---:|---|---|---|---|",
        ]
    )

    for package in packages:
        references = "; ".join(
            f"`{_markdown_cell(notice.name)}`@`{notice.sha256[:16]}`"
            for notice in package.notices
        )
        header.append(
            "| "
            f"`{_markdown_cell(package.name)}` | "
            f"`{_markdown_cell(package.version)}` | "
            f"`{_markdown_cell(package.license_expression)}` | "
            f"`{_markdown_cell(package.cargo_checksum)}` | "
            f"{_markdown_cell(package.repository)} | "
            f"{references} |"
        )

    header.extend(
        [
            "",
            "## Verbatim notice texts",
            "",
            "Each byte sequence is deduplicated by SHA-256. The text between its",
            "`BEGIN` and `END` markers is copied from the named locked crate file.",
            "If an upstream file lacked a final newline, the generator adds only the",
            "line break needed to separate it from the `END` marker.",
            "",
        ]
    )

    output = bytearray(("\n".join(header) + "\n").encode("utf-8"))
    for digest in sorted(content_by_hash):
        users = sorted(users_by_hash[digest], key=lambda item: (item[0].casefold(), item))
        output.extend(f"### SHA-256 `{digest}`\n\nUsed by:\n\n".encode("utf-8"))
        for owner, filename in users:
            output.extend(f"- `{owner}` — `{filename}`\n".encode("utf-8"))
        output.extend(f"\n--- BEGIN VERBATIM NOTICE {digest} ---\n".encode("utf-8"))
        content = content_by_hash[digest]
        output.extend(content)
        if not content.endswith(b"\n"):
            output.extend(b"\n")
        output.extend(f"--- END VERBATIM NOTICE {digest} ---\n\n".encode("utf-8"))
    return bytes(output)


def generate() -> bytes:
    return _render(_packages(), _rust_toolchain())


def _check(generated: bytes) -> int:
    try:
        current = OUTPUT.read_bytes()
    except FileNotFoundError:
        print(f"Missing {OUTPUT.relative_to(ROOT)}; run this script with --update.", file=sys.stderr)
        return 1
    if current == generated:
        print(f"{OUTPUT.relative_to(ROOT)} is current.")
        return 0
    print(
        f"{OUTPUT.relative_to(ROOT)} is stale: "
        f"expected sha256:{hashlib.sha256(generated).hexdigest()}, "
        f"found sha256:{hashlib.sha256(current).hexdigest()}.\n"
        "Run scripts/generate-wasm-third-party-notices.py --update.",
        file=sys.stderr,
    )
    return 1


def _update(generated: bytes) -> int:
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=OUTPUT.parent, delete=False) as temporary:
        temporary.write(generated)
        temporary_path = Path(temporary.name)
    os.chmod(temporary_path, 0o644)
    os.replace(temporary_path, OUTPUT)
    print(
        f"Updated {OUTPUT.relative_to(ROOT)} "
        f"(sha256:{hashlib.sha256(generated).hexdigest()})."
    )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true", help="compare with the checked-in bundle")
    mode.add_argument("--update", action="store_true", help="replace the checked-in bundle")
    mode.add_argument("--stdout", action="store_true", help="write the generated bundle to stdout")
    arguments = parser.parse_args()

    try:
        generated = generate()
    except (GenerationError, OSError) as error:
        print(f"notice generation failed: {error}", file=sys.stderr)
        return 1

    if arguments.stdout:
        sys.stdout.buffer.write(generated)
        return 0
    if arguments.update:
        return _update(generated)
    return _check(generated)


if __name__ == "__main__":
    raise SystemExit(main())
