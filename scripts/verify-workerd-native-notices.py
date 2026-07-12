#!/usr/bin/env python3
"""Offline integrity and release-gate verification for native workerd notices."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import sys


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_BUNDLE = (
    ROOT
    / "ha_voice_hermes_gateway"
    / "licenses"
    / "container"
    / "workerd-native"
)
DEFAULT_PACKAGE_LOCK = ROOT / "ha_voice_hermes_gateway" / "package-lock.json"
EXPECTED_COMPONENTS = {
    "abseil-cpp",
    "ada-url",
    "boringssl",
    "brotli",
    "capnp-cpp",
    "dragonbox",
    "fast-float",
    "fp16",
    "gcc-runtime",
    "highway",
    "icu",
    "llvm-libc",
    "llvm-runtime",
    "nbytes",
    "ncrypto",
    "perfetto",
    "re2",
    "simdutf",
    "sqlite",
    "tcmalloc",
    "v8",
    "workerd-cxx",
    "zlib",
    "zstd",
}
EXPECTED_ARCHITECTURE_HASHES = {
    "linux-amd64": {
        "package_sha256": "18d805cbf10043dabbff30ef57bc83a8fc8524ea666fa3827dafe9b3dee83d58",
        "binary_sha256": "a74a07d8003e5ea018d4fe4c4746c70e38b73aa5d5add97747a959991bd3e000",
    },
    "linux-arm64": {
        "package_sha256": "8e89f4d3c16f3fa38d659d5c1fc21b9ffc66d99b2bd72f154f2ef3495bf5fb63",
        "binary_sha256": "2193c2ed64e76fe0a7bf0fb3d6e6eb128ada19198913459fb2a1eafcfdfa9814",
    },
}
EXPECTED_RESIDUAL_LIMITATIONS = {
    "upstream-binary-native-closure-attestation",
    "v8-perfetto-feature-closure-attestation",
}
EXPECTED_NOTICE_SETS = {
    "gcc-runtime": {
        "notices/gcc-runtime-12.3.0/COPYING",
        "notices/gcc-runtime-12.3.0/COPYING.LIB",
        "notices/gcc-runtime-12.3.0/COPYING.RUNTIME",
        "notices/gcc-runtime-12.3.0/COPYING3",
        "notices/gcc-runtime-12.3.0/COPYING3.LIB",
    },
    "llvm-runtime": {
        "notices/llvm-runtime-19.1.7/compiler-rt/CREDITS.TXT",
        "notices/llvm-runtime-19.1.7/compiler-rt/LICENSE.TXT",
        "notices/llvm-runtime-19.1.7/libcxx/CREDITS.TXT",
        "notices/llvm-runtime-19.1.7/libcxx/LICENSE.TXT",
        "notices/llvm-runtime-19.1.7/libcxxabi/CREDITS.TXT",
        "notices/llvm-runtime-19.1.7/libcxxabi/LICENSE.TXT",
        "notices/llvm-runtime-19.1.7/libunwind/LICENSE.TXT",
    },
    "perfetto": {
        "notices/perfetto-56.1/LICENSE",
        "notices/perfetto-56.1/METADATA",
        "notices/perfetto-56.1/README.chromium",
        "notices/perfetto-56.1/python/LICENSE",
    },
    "re2": {
        "notices/re2-2024-07-02/LICENSE",
        "notices/re2-2024-07-02/python/LICENSE",
    },
}
EXPECTED_TREE_AUDITS = {
    "gcc-runtime": {
        "tree_sha": "2d179124ce42db58e4341281c094f102ddd6d30b",
        "legal_like_files": {"COPYING", "COPYING.LIB", "COPYING.RUNTIME", "COPYING3", "COPYING3.LIB"},
        "libgcc_tree_sha": "7bbe754d8041096908cc3ad23d239e7d1f651db4",
    },
    "llvm-runtime": {
        "tree_sha": "59c674c18e0bb2fe82680dc1a1c397657c0050b2",
        "legal_like_files": {
            "compiler-rt/CREDITS.TXT",
            "compiler-rt/LICENSE.TXT",
            "libcxx/CREDITS.TXT",
            "libcxx/LICENSE.TXT",
            "libcxxabi/CREDITS.TXT",
            "libcxxabi/LICENSE.TXT",
            "libunwind/LICENSE.TXT",
        },
        "subtree_shas": {
            "compiler-rt": "ec9dfb9909997011cbdc7c4f496fc90f79e2a46a",
            "libcxx": "7bcf87ad3b5284406c816d9038759dddb5ef5bfe",
            "libcxxabi": "c3aa69d7f2e91b7f5d70c403e91aefb01ff44be3",
            "libunwind": "d5269f75a5dd964b1a64554d20c73b443f7b09da",
        },
    },
    "perfetto": {
        "tree_sha": "b90b6017510d687367d7ea3d7cdc109d4b429b39",
        "legal_like_files": {"LICENSE", "python/LICENSE"},
        "additional_legal_metadata": {"METADATA", "README.chromium"},
    },
    "re2": {
        "tree_sha": "7422b5ab1f79bfe58b958873ff02db50141afd9a",
        "legal_like_files": {"LICENSE", "python/LICENSE"},
    },
}
SOURCE_OBLIGATION_STATES = {
    "none-permissive",
    "none-gcc-runtime-exception",
    "none-public-domain",
    "none-selected-bsd-3-clause",
    "source-archive-locked-conservative",
}
HEX_256 = re.compile(r"^[0-9a-f]{64}$")
FULL_COMMIT = re.compile(r"^[0-9a-f]{40}$")


class VerificationError(RuntimeError):
    """The vendored inventory is malformed or has drifted."""


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def safe_relative_path(value: object) -> Path:
    if not isinstance(value, str):
        raise VerificationError("blob path is not a string")
    pure = PurePosixPath(value)
    if pure.is_absolute() or not pure.parts or any(part in {"", ".", ".."} for part in pure.parts):
        raise VerificationError(f"unsafe blob path: {value!r}")
    if pure.parts[0] not in {"notices", "evidence"}:
        raise VerificationError(f"blob is outside notices/evidence: {value!r}")
    return Path(*pure.parts)


def load_json(path: Path) -> dict[str, object]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise VerificationError(f"cannot parse {path}: {error}") from error
    if not isinstance(value, dict):
        raise VerificationError(f"{path} is not a JSON object")
    return value


def verify_workerd_pin(data: dict[str, object], package_lock_path: Path) -> None:
    workerd = data.get("workerd")
    if not isinstance(workerd, dict):
        raise VerificationError("missing workerd lock")
    version = workerd.get("version")
    commit = workerd.get("commit")
    if version != "1.20260708.1" or not isinstance(commit, str) or not FULL_COMMIT.fullmatch(commit):
        raise VerificationError("workerd version or commit is not exactly pinned")
    package_lock = load_json(package_lock_path)
    packages = package_lock.get("packages")
    if not isinstance(packages, dict):
        raise VerificationError("package-lock has no packages object")
    locked = packages.get("node_modules/workerd")
    if not isinstance(locked, dict):
        raise VerificationError("package-lock has no workerd package")
    if locked.get("version") != version or locked.get("integrity") != workerd.get("npm_integrity"):
        raise VerificationError("workerd package-lock version/integrity differs from native notice lock")
    architectures = workerd.get("architectures")
    if not isinstance(architectures, dict) or set(architectures) != set(EXPECTED_ARCHITECTURE_HASHES):
        raise VerificationError("workerd architecture digest set is incomplete")
    for architecture, values in architectures.items():
        if not isinstance(values, dict):
            raise VerificationError(f"invalid architecture lock: {architecture}")
        for key in ("package_sha256", "binary_sha256"):
            value = values.get(key)
            if not isinstance(value, str) or not HEX_256.fullmatch(value):
                raise VerificationError(f"invalid {key} for {architecture}")
        if values != EXPECTED_ARCHITECTURE_HASHES[architecture]:
            raise VerificationError(f"workerd package/binary hashes drifted for {architecture}")
    if workerd.get("npm_wrapper_sha256") != "c8e89aac2b68f07063af5dff670b4e5726b3889d4f3957da7a2ce012d0c804bc":
        raise VerificationError("workerd NPM wrapper digest drifted")


def verify_components(data: dict[str, object], blob_paths: set[str]) -> None:
    components = data.get("components")
    if not isinstance(components, list):
        raise VerificationError("components is not a list")
    names: set[str] = set()
    referenced_notices: set[str] = set()
    for component in components:
        if not isinstance(component, dict):
            raise VerificationError("component entry is not an object")
        name = component.get("name")
        if not isinstance(name, str) or not name or name in names:
            raise VerificationError(f"invalid or duplicate component name: {name!r}")
        names.add(name)
        if component.get("status") != "covered":
            raise VerificationError(f"component is not notice-covered: {name}")
        if not isinstance(component.get("license_expression"), str) or not component["license_expression"]:
            raise VerificationError(f"component lacks a licence expression: {name}")
        source_obligation = component.get("source_obligation")
        if source_obligation not in SOURCE_OBLIGATION_STATES:
            raise VerificationError(f"component lacks a reviewed source-obligation decision: {name}")
        revision = component.get("revision")
        source_sha = component.get("source_sha256")
        source_url = component.get("source_url")
        if not isinstance(source_url, str) or not source_url.startswith("https://"):
            raise VerificationError(f"component lacks an HTTPS source URL: {name}")
        if revision is not None and (not isinstance(revision, str) or not FULL_COMMIT.fullmatch(revision)):
            raise VerificationError(f"component has a malformed revision: {name}")
        if source_sha is not None and (not isinstance(source_sha, str) or not HEX_256.fullmatch(source_sha)):
            raise VerificationError(f"component has a malformed source digest: {name}")
        if revision is None and source_sha is None:
            raise VerificationError(f"component lacks an immutable commit or source digest: {name}")
        if source_obligation == "source-archive-locked-conservative" and not (
            isinstance(source_sha, str) and HEX_256.fullmatch(source_sha)
        ):
            raise VerificationError(f"component lacks its conservative source archive: {name}")
        if source_obligation == "none-selected-bsd-3-clause" and "BSD-3-Clause" not in component["license_expression"]:
            raise VerificationError(f"component's permissive licence selection is inconsistent: {name}")
        if source_obligation == "none-public-domain" and component["license_expression"] != "blessing":
            raise VerificationError(f"component's public-domain decision is inconsistent: {name}")
        notices = component.get("notices")
        if not isinstance(notices, list) or not notices:
            raise VerificationError(f"component has no notice files: {name}")
        if name in EXPECTED_NOTICE_SETS and set(notices) != EXPECTED_NOTICE_SETS[name]:
            raise VerificationError(f"audited notice set drifted: {name}")
        for notice in notices:
            if notice not in blob_paths or not isinstance(notice, str) or not notice.startswith("notices/"):
                raise VerificationError(f"component references an unknown notice: {name}: {notice!r}")
            referenced_notices.add(notice)
    if names != EXPECTED_COMPONENTS:
        missing = sorted(EXPECTED_COMPONENTS - names)
        extra = sorted(names - EXPECTED_COMPONENTS)
        raise VerificationError(f"native component set drifted; missing={missing}, extra={extra}")
    vendored_notices = {path for path in blob_paths if path.startswith("notices/")}
    if referenced_notices != vendored_notices:
        missing = sorted(vendored_notices - referenced_notices)
        unknown = sorted(referenced_notices - vendored_notices)
        raise VerificationError(f"notice ownership drifted; unowned={missing}, unknown={unknown}")


def verify_blobs(bundle: Path, data: dict[str, object]) -> set[str]:
    blobs = data.get("blobs")
    if not isinstance(blobs, list) or not blobs:
        raise VerificationError("blob inventory is empty")
    expected: set[Path] = set()
    logical: set[str] = set()
    for blob in blobs:
        if not isinstance(blob, dict):
            raise VerificationError("blob entry is not an object")
        relative = safe_relative_path(blob.get("path"))
        logical_path = relative.as_posix()
        expected_digest = blob.get("sha256")
        if relative in expected or not isinstance(expected_digest, str) or not HEX_256.fullmatch(expected_digest):
            raise VerificationError(f"duplicate path or invalid digest: {logical_path}")
        url = blob.get("url")
        if not isinstance(url, str) or not url.startswith("https://"):
            raise VerificationError(f"blob URL is not HTTPS: {logical_path}")
        encoding = blob.get("encoding", "identity")
        if encoding not in {"identity", "base64", "zip-member", "text-section"}:
            raise VerificationError(f"unsupported blob encoding in lock: {logical_path}")
        if encoding == "zip-member":
            archive_sha = blob.get("archive_sha256")
            archive_member = blob.get("archive_member")
            if not isinstance(archive_sha, str) or not HEX_256.fullmatch(archive_sha):
                raise VerificationError(f"archive digest is invalid: {logical_path}")
            if not isinstance(archive_member, str) or not archive_member:
                raise VerificationError(f"archive member is invalid: {logical_path}")
        if encoding == "text-section":
            source_sha = blob.get("source_sha256")
            start = blob.get("start_marker")
            end = blob.get("end_marker")
            if not isinstance(source_sha, str) or not HEX_256.fullmatch(source_sha):
                raise VerificationError(f"text-section source digest is invalid: {logical_path}")
            if not isinstance(start, str) or not start or not isinstance(end, str) or not end:
                raise VerificationError(f"text-section markers are invalid: {logical_path}")
        target = bundle / relative
        try:
            actual = digest(target)
        except OSError as error:
            raise VerificationError(f"cannot read {logical_path}: {error}") from error
        if actual != expected_digest:
            raise VerificationError(f"digest drift for {logical_path}: expected {expected_digest}, got {actual}")
        expected.add(relative)
        logical.add(logical_path)
    actual_files = {
        path.relative_to(bundle)
        for root_name in ("notices", "evidence")
        for path in (bundle / root_name).rglob("*")
        if path.is_file()
    }
    if actual_files != expected:
        missing = sorted(str(path) for path in expected - actual_files)
        extra = sorted(str(path) for path in actual_files - expected)
        raise VerificationError(f"vendored file set drifted; missing={missing}, extra={extra}")
    return logical


def verify_assertions(bundle: Path, data: dict[str, object], blob_paths: set[str]) -> None:
    assertions = data.get("derivation_assertions")
    if not isinstance(assertions, list) or not assertions:
        raise VerificationError("derivation assertions are missing")
    for assertion in assertions:
        if not isinstance(assertion, dict):
            raise VerificationError("derivation assertion is not an object")
        path = assertion.get("path")
        contains = assertion.get("contains")
        if path not in blob_paths or not isinstance(path, str) or not path.startswith("evidence/"):
            raise VerificationError(f"assertion references unknown evidence: {path!r}")
        if not isinstance(contains, list) or not contains or not all(isinstance(item, str) and item for item in contains):
            raise VerificationError(f"assertion has no literals: {path}")
        text = (bundle / Path(*PurePosixPath(path).parts)).read_text(encoding="utf-8")
        for literal in contains:
            if literal not in text:
                raise VerificationError(f"derivation literal missing from {path}: {literal!r}")


def verify_legal_state(data: dict[str, object]) -> tuple[list[dict[str, object]], list[dict[str, object]]]:
    legal_gate = data.get("legal_gate")
    if not isinstance(legal_gate, dict) or legal_gate.get("status") != "covered":
        raise VerificationError("native legal gate is not marked covered")
    basis = legal_gate.get("basis")
    if not isinstance(basis, list) or len(basis) < 5 or not all(isinstance(item, str) and item for item in basis):
        raise VerificationError("native legal-gate basis is incomplete")
    if not isinstance(legal_gate.get("dynamic_library_owner"), str) or not legal_gate["dynamic_library_owner"]:
        raise VerificationError("dynamic Debian library obligations have no recorded owner")

    inspection = data.get("binary_inspection")
    if not isinstance(inspection, dict):
        raise VerificationError("binary inspection record is missing")
    expected_needed = {
        "linux-amd64": {"ld-linux-x86-64.so.2", "libc.so.6", "libm.so.6"},
        "linux-arm64": {"ld-linux-aarch64.so.1", "libc.so.6", "libm.so.6"},
    }
    for architecture, expected in expected_needed.items():
        result = inspection.get(architecture)
        if not isinstance(result, dict) or set(result.get("needed", [])) != expected:
            raise VerificationError(f"dynamic dependency inspection drifted for {architecture}")
        if result.get("bundled_glibc_trig_symbols_found") is not False:
            raise VerificationError(f"bundled V8 glibc trig status is not cleared for {architecture}")
    if set(inspection.get("native_external_repository_markers", [])) != {
        "abseil-cpp",
        "boringssl",
        "capnp-cpp",
        "perfetto",
        "tcmalloc",
        "v8",
        "workerd-cxx",
    }:
        raise VerificationError("native external-repository marker set drifted")
    if set(inspection.get("additional_library_markers", [])) != {
        "brotli",
        "icu",
        "simdutf",
        "sqlite",
        "zlib",
        "zstd",
    }:
        raise VerificationError("additional native library marker set drifted")
    if inspection.get("source_selected_native_components") != ["re2"]:
        raise VerificationError("source-selected native component evidence drifted")
    if set(inspection.get("static_runtime_markers", [])) != {
        "libcxxabi/src/private_typeinfo.cpp",
        "libcxxabi/src/cxa_demangle.cpp",
        "_Unwind_RaiseException",
    }:
        raise VerificationError("static C++/unwind runtime marker set drifted")
    expected_toolchain = {
        "Linker: Ubuntu LLD 19.1.7",
        "GCC: (Ubuntu 12.3.0-1ubuntu1~22.04.3) 12.3.0",
        "Ubuntu clang version 19.1.7 (++20250114103320+cd708029e0b2-1~exp1~20250114103432.75)",
    }
    toolchain_comments = inspection.get("toolchain_comments")
    if not isinstance(toolchain_comments, dict):
        raise VerificationError("toolchain comment evidence is missing")
    for architecture in ("linux-amd64", "linux-arm64"):
        if set(toolchain_comments.get(architecture, [])) != expected_toolchain:
            raise VerificationError(f"toolchain comment evidence drifted for {architecture}")

    startup = legal_gate.get("dynamic_and_startup_obligations")
    expected_startup = {"libc-libm-loader", "glibc-startup", "llvm-static-runtime", "gcc-static-runtime"}
    if not isinstance(startup, dict) or set(startup) != expected_startup:
        raise VerificationError("dynamic/startup obligation ownership is incomplete")
    for name, decision in startup.items():
        if not isinstance(decision, dict):
            raise VerificationError(f"dynamic/startup obligation is malformed: {name}")
        for field in ("coverage", "basis"):
            if not isinstance(decision.get(field), str) or not decision[field]:
                raise VerificationError(f"dynamic/startup obligation lacks {field}: {name}")

    build_only = legal_gate.get("build_only_not_distributed")
    if not isinstance(build_only, list) or {item.get("name") for item in build_only if isinstance(item, dict)} != {
        "protobuf",
        "perfetto optional extension repositories",
    }:
        raise VerificationError("Perfetto build-only dependency decisions are incomplete")
    for decision in build_only:
        if not isinstance(decision, dict) or not all(
            isinstance(decision.get(field), str) and decision[field] for field in ("name", "version", "reason")
        ):
            raise VerificationError("Perfetto build-only dependency decision is malformed")

    audits = legal_gate.get("license_tree_audits")
    if not isinstance(audits, dict) or set(audits) != set(EXPECTED_TREE_AUDITS):
        raise VerificationError("native licence-tree audit set is incomplete")
    for name, expected in EXPECTED_TREE_AUDITS.items():
        audit = audits.get(name)
        if not isinstance(audit, dict) or not isinstance(audit.get("scope"), str) or not audit["scope"]:
            raise VerificationError(f"licence-tree audit is malformed: {name}")
        for field, expected_value in expected.items():
            actual_value = audit.get(field)
            if isinstance(expected_value, set):
                if not isinstance(actual_value, list) or set(actual_value) != expected_value:
                    raise VerificationError(f"licence-tree audit {field} drifted: {name}")
            elif actual_value != expected_value:
                raise VerificationError(f"licence-tree audit {field} drifted: {name}")
    if audits["gcc-runtime"].get("libgcc_additional_legal_like_files") != []:
        raise VerificationError("libgcc legal-filename scan result drifted")

    unresolved = data.get("unresolved")
    if not isinstance(unresolved, list):
        raise VerificationError("unresolved field is not a list")
    for blocker in unresolved:
        if not isinstance(blocker, dict) or not isinstance(blocker.get("id"), str) or not blocker["id"]:
            raise VerificationError("malformed unresolved blocker")
        if not isinstance(blocker.get("reason"), str) or not blocker["reason"]:
            raise VerificationError(f"unresolved blocker lacks a reason: {blocker.get('id')!r}")

    residual = data.get("residual_limitations")
    if not isinstance(residual, list):
        raise VerificationError("residual limitations are missing")
    ids: set[str] = set()
    for limitation in residual:
        if not isinstance(limitation, dict):
            raise VerificationError("malformed residual limitation")
        identifier = limitation.get("id")
        if not isinstance(identifier, str) or not identifier or identifier in ids:
            raise VerificationError(f"invalid residual limitation id: {identifier!r}")
        ids.add(identifier)
        if limitation.get("category") != "supply-chain-evidence":
            raise VerificationError(f"residual limitation is misclassified: {identifier}")
        for field in ("detail", "effect", "mitigation"):
            if not isinstance(limitation.get(field), str) or not limitation[field]:
                raise VerificationError(f"residual limitation lacks {field}: {identifier}")
    if ids != EXPECTED_RESIDUAL_LIMITATIONS:
        raise VerificationError(f"residual limitation disclosure drifted: {sorted(ids)}")
    return unresolved, residual


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bundle", type=Path, default=DEFAULT_BUNDLE)
    parser.add_argument("--package-lock", type=Path, default=DEFAULT_PACKAGE_LOCK)
    parser.add_argument(
        "--integrity-only",
        action="store_true",
        help="verify checked-in bytes but do not open a release gate with unresolved blockers",
    )
    return parser.parse_args()


def main() -> int:
    arguments = parse_args()
    bundle = arguments.bundle.resolve()
    data = load_json(bundle / "LOCK.json")
    if data.get("schema_version") != 1:
        raise VerificationError("unsupported lock schema")
    verify_workerd_pin(data, arguments.package_lock.resolve())
    blob_paths = verify_blobs(bundle, data)
    verify_components(data, blob_paths)
    verify_assertions(bundle, data, blob_paths)
    unresolved, residual = verify_legal_state(data)
    print(
        f"verified {len(data['components'])} native components, {len(blob_paths)} locked files, "
        f"and {len(residual)} disclosed residual limitations"
    )
    if unresolved and not arguments.integrity_only:
        ids = ", ".join(blocker["id"] for blocker in unresolved)
        raise VerificationError(f"release remains fail-closed on unresolved native closure: {ids}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except VerificationError as error:
        print(f"workerd native notice verification failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
