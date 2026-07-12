#!/usr/bin/env python3
"""Focused drift and minimum-content tests for the WASM notice bundle."""

from __future__ import annotations

from pathlib import Path
import re
import subprocess
import sys
import unittest


ROOT = Path(__file__).resolve().parents[2]
GENERATOR = ROOT / "scripts" / "generate-wasm-third-party-notices.py"
BUNDLE = (
    ROOT
    / "ha_voice_hermes_gateway"
    / "licenses"
    / "wasm"
    / "THIRD_PARTY_NOTICES.md"
)


def run_generator(*arguments: str) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        [sys.executable, str(GENERATOR), *arguments],
        cwd=ROOT,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )


class WasmThirdPartyNoticesTest(unittest.TestCase):
    def test_checked_in_bundle_matches_locked_graph(self) -> None:
        result = run_generator("--check")
        self.assertEqual(result.returncode, 0, result.stderr.decode("utf-8", "replace"))

    def test_generation_is_deterministic_and_has_required_notices(self) -> None:
        first = run_generator("--stdout")
        second = run_generator("--stdout")
        self.assertEqual(first.returncode, 0, first.stderr.decode("utf-8", "replace"))
        self.assertEqual(second.returncode, 0, second.stderr.decode("utf-8", "replace"))
        self.assertEqual(first.stdout, second.stdout)
        self.assertEqual(first.stdout, BUNDLE.read_bytes())

        text = first.stdout.decode("utf-8")
        self.assertIn("Third-party packages: **85**", text)
        self.assertRegex(
            text,
            r"(?m)^\| `icu_collections` \| `2\.2\.0` \| `Unicode-3\.0` \|",
        )
        self.assertIn("Copyright © 2020-2024 Unicode, Inc.", text)
        self.assertRegex(
            text,
            r"(?m)^\| `matchit` \| `0\.7\.3` \| `MIT AND BSD-3-Clause` .*"
            r"`LICENSE`@.*`LICENSE\.httprouter`@",
        )
        self.assertIn("Copyright (c) 2022 Ibraheem Ahmed", text)
        self.assertIn("Copyright (c) 2013, Julien Schmidt", text)
        self.assertIn("Rust toolchain notice files: **6**", text)
        self.assertIn(
            "Rust compiler source commit: "
            "`ac68faa20c58cbccd01ee7208bf3b6e93a7d7f96`",
            text,
        )
        self.assertIn(
            "`rust-toolchain.toml` SHA-256: "
            "`08ba787d61637d3820dc74fce360d8e46f2c0e4e4588474d2697d6b062adcbda`",
            text,
        )
        self.assertIn("LLVM version reported by rustc: `22.1.2`", text)
        for notice_reference in (
            "`COPYRIGHT-library.html`@`78c163fcec50e64b`",
            "`licenses/Apache-2.0.txt`@`074e6e32c86a4c0e`",
            "`licenses/BSD-2-Clause.txt`@`f32fb3b417a19416`",
            "`licenses/LLVM-exception.txt`@`e34c58338bd89d43`",
            "`licenses/MIT.txt`@`b85dcd3e453d0598`",
            "`licenses/Unicode-3.0.txt`@`f5062c9a188d81df`",
        ):
            self.assertIn(notice_reference, text)
        self.assertIn("Copyright notices for The Rust Standard Library", text)
        self.assertIn("--- LLVM Exceptions to the Apache 2.0 License ----", text)
        self.assertNotIn(str(ROOT), text)
        self.assertIsNone(re.search(r"/(?:Users|home)/[^\s|`]+", text))


if __name__ == "__main__":
    unittest.main()
