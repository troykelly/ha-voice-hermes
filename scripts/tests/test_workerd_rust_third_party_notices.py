#!/usr/bin/env python3
"""Focused provenance, drift, source, and notice tests for workerd Rust."""

from __future__ import annotations

from contextlib import redirect_stderr
import hashlib
import importlib.util
import io
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
GENERATOR = ROOT / "scripts" / "generate-workerd-rust-third-party-notices.py"
OUTPUT_DIRECTORY = (
    ROOT
    / "ha_voice_hermes_gateway"
    / "licenses"
    / "container"
    / "workerd-rust"
)
BUNDLE = OUTPUT_DIRECTORY / "THIRD_PARTY_NOTICES.md"
LOCK = OUTPUT_DIRECTORY / "UPSTREAM_CARGO.lock"


def run_generator(*arguments: str) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        [sys.executable, str(GENERATOR), *arguments],
        cwd=ROOT,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )


def load_generator():
    spec = importlib.util.spec_from_file_location("workerd_rust_notice_generator", GENERATOR)
    if spec is None or spec.loader is None:
        raise RuntimeError("could not load generator")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class WorkerdRustThirdPartyNoticesTest(unittest.TestCase):
    def test_checked_in_outputs_match_remote_provenance(self) -> None:
        result = run_generator("--release-check")
        self.assertEqual(result.returncode, 0, result.stderr.decode("utf-8", "replace"))
        self.assertIn(b"zero unresolved packages", result.stdout)

    def test_generation_is_deterministic_and_complete(self) -> None:
        first = run_generator("--stdout")
        second = run_generator("--stdout")
        self.assertEqual(first.returncode, 0, first.stderr.decode("utf-8", "replace"))
        self.assertEqual(second.returncode, 0, second.stderr.decode("utf-8", "replace"))
        self.assertEqual(first.stdout, second.stdout)
        self.assertEqual(first.stdout, BUNDLE.read_bytes())
        self.assertEqual(
            hashlib.sha256(LOCK.read_bytes()).hexdigest(),
            "d2f4504f29de4419cbc792e27dcdca430cb9a5202cda5c003b7679b5bacbfaf7",
        )

        text = first.stdout.decode("utf-8")
        self.assertIn("Registry crates inventoried: **234**", text)
        self.assertIn("Exact git packages inventoried: **7**", text)
        self.assertIn("Exact path packages inventoried: **1**", text)
        self.assertIn("Unresolved lockfile packages: **0**", text)
        self.assertIn("Reviewed notice fallbacks: **31**", text)
        self.assertIn("Unique verbatim notice texts including Rust: **140**", text)
        self.assertIn("`v1.20260708.1`", text)
        self.assertIn("`99057e45cb7152cc5efc59b8d2182386fe1e3eec`", text)
        self.assertIn("`b0ddf5c5dbb280e86af6469f0db6775df671619d9fb8f04f2bd549f1eedbe7c2`", text)
        self.assertIn("Rust compiler source commit: `ac68faa20c58cbccd01ee7208bf3b6e93a7d7f96`", text)
        for notice_reference in (
            "`rust/COPYRIGHT-library.html`@`78c163fcec50e64b`",
            "`rust/licenses/Apache-2.0.txt`@`074e6e32c86a4c0e`",
            "`rust/licenses/BSD-2-Clause.txt`@`f32fb3b417a19416`",
            "`rust/licenses/LLVM-exception.txt`@`e34c58338bd89d43`",
            "`rust/licenses/MIT.txt`@`b85dcd3e453d0598`",
            "`rust/licenses/Unicode-3.0.txt`@`f5062c9a188d81df`",
        ):
            self.assertIn(notice_reference, text)
        self.assertIn("Copyright notices for The Rust Standard Library", text)
        self.assertIn("Copyright (c) 2016 by Armin Ronacher.", text)
        self.assertIn("`lol_html 2.7.2`", text)
        self.assertIn("`lol_html_c_api 1.3.1`", text)
        self.assertIn("`ruff_python_parser 0.0.0`", text)
        self.assertIn("`direct-cargo-bazel-deps 0.0.1`", text)
        self.assertIn("`e3aa54798602dd27250fafde1b5a66f080046252`", text)
        self.assertIn("`32c54189cb45a9d0409a1140265ce6d5fcec214d`", text)
        self.assertIn("`8d35c463c40eacbe6984a6dc5c454ae1e837ead1e21ce18b052034a04e5b40f6`", text)
        self.assertIn("`c0a26b7c0b010b1d4fdfc4b652db7c7e791e987f533eddc01988b19576df2428`", text)
        self.assertIn("`42a6ae45c09fd5cb400b280e8280dab57ebe9ee45fbccb5232bdcaf3134c8808`", text)
        self.assertNotIn("require separate source/license coverage", text)

        inventory = text.split("## Exact crates.io inventory", 1)[1].split(
            "## Reviewed notice fallbacks", 1
        )[0]
        inventory_rows = [line for line in inventory.splitlines() if line.startswith("| `")]
        self.assertEqual(len(inventory_rows), 234)
        source_inventory = text.split("## Exact git and path package inventory", 1)[1].split(
            "Lock-source identities covered by the table above:", 1
        )[0]
        source_rows = [
            line for line in source_inventory.splitlines() if line.startswith("| `")
        ]
        self.assertEqual(len(source_rows), 8)
        mpl = text.split("## MPL-2.0 source-code availability", 1)[1].split(
            "## Exact crates.io inventory", 1
        )[0]
        mpl_rows = [line for line in mpl.splitlines() if line.startswith("| `")]
        self.assertEqual(len(mpl_rows), 5)
        for package in ("cssparser", "cssparser-macros", "dtoa-short", "selectors", "smartstring"):
            self.assertRegex(mpl, rf"(?m)^\| `{re.escape(package)} [^`]+` \|")
        self.assertIn("selectors/selectors-0.33.0.crate", mpl)
        self.assertIn("fallback/MPL-2.0.txt", text)
        self.assertIn("publisher marked dirty", text)
        self.assertNotIn(str(ROOT), text)
        self.assertIsNone(re.search(r"/(?:Users|home)/[^\s|`]+", text))

    def test_remote_tag_and_lock_drift_fail_closed(self) -> None:
        generator = load_generator()
        with mock.patch.object(generator, "_remote_tag_commit", return_value="0" * 40):
            with self.assertRaisesRegex(generator.GenerationError, "remote .* moved"):
                generator._upstream()
        with (
            mock.patch.object(
                generator, "_remote_tag_commit", return_value=generator.WORKERD_COMMIT
            ),
            mock.patch.object(generator, "_download", return_value=b"drifted lock"),
        ):
            with self.assertRaisesRegex(generator.GenerationError, "lock.* hash changed"):
                generator._upstream()

        lol_html = next(
            item for item in generator.SOURCE_REPOSITORIES if item.name == "lol-html"
        )
        with mock.patch.object(generator, "_git_tag_commit", return_value="0" * 40):
            with self.assertRaisesRegex(generator.GenerationError, "lol-html tag.* moved"):
                generator._verify_source_repository_tag(lol_html)

        with (
            tempfile.TemporaryDirectory() as cache,
            mock.patch.dict(os.environ, {"WORKERD_RUST_CRATE_CACHE": cache}),
            mock.patch.object(generator, "_download", return_value=b"drifted source"),
        ):
            with self.assertRaisesRegex(
                generator.GenerationError, "source archive checksum mismatch"
            ):
                generator._source_archive(lol_html)

    def test_release_check_fails_for_any_unresolved_package(self) -> None:
        generator = load_generator()
        generation = generator.Generation(BUNDLE.read_bytes(), LOCK.read_bytes(), 1)
        error = io.StringIO()
        with redirect_stderr(error), mock.patch.object(generator, "_check") as check:
            self.assertEqual(generator._release_check(generation), 1)
        check.assert_not_called()
        self.assertIn("1 unresolved lockfile package", error.getvalue())


if __name__ == "__main__":
    unittest.main()
