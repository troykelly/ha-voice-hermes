#!/usr/bin/env python3
"""Offline tests for the fail-closed native workerd notice inventory."""

from __future__ import annotations

import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
BUNDLE = (
    ROOT
    / "ha_voice_hermes_gateway"
    / "licenses"
    / "container"
    / "workerd-native"
)
VERIFY = ROOT / "scripts" / "verify-workerd-native-notices.py"
REFRESH = ROOT / "scripts" / "refresh-workerd-native-notices.py"
PACKAGE_LOCK = ROOT / "ha_voice_hermes_gateway" / "package-lock.json"


def run(script: Path, *arguments: str) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        [sys.executable, str(script), *arguments],
        cwd=ROOT,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )


class WorkerdNativeNoticesTest(unittest.TestCase):
    def test_offline_integrity_verification_succeeds(self) -> None:
        result = run(VERIFY, "--integrity-only")
        self.assertEqual(result.returncode, 0, result.stderr.decode("utf-8", "replace"))
        self.assertIn(b"verified 24 native components", result.stdout)

    def test_release_gate_passes_with_residual_supply_chain_limits_disclosed(self) -> None:
        result = run(VERIFY)
        self.assertEqual(result.returncode, 0, result.stderr.decode("utf-8", "replace"))
        self.assertIn(b"2 disclosed residual limitations", result.stdout)
        lock = json.loads((BUNDLE / "LOCK.json").read_text(encoding="utf-8"))
        self.assertEqual(lock["unresolved"], [])
        self.assertTrue(
            {"re2", "llvm-runtime", "gcc-runtime"}.issubset(
                {component["name"] for component in lock["components"]}
            )
        )
        self.assertEqual(
            {item["id"] for item in lock["residual_limitations"]},
            {
                "upstream-binary-native-closure-attestation",
                "v8-perfetto-feature-closure-attestation",
            },
        )

    def test_refresh_plan_is_deterministic_without_network(self) -> None:
        first = run(REFRESH, "--emit-plan")
        second = run(REFRESH, "--emit-plan")
        self.assertEqual(first.returncode, 0, first.stderr.decode("utf-8", "replace"))
        self.assertEqual(first.stdout, second.stdout)
        plan = json.loads(first.stdout)
        self.assertEqual(len(plan), 113)
        self.assertEqual([entry["path"] for entry in plan], sorted(entry["path"] for entry in plan))

    def test_digest_tampering_is_rejected_offline(self) -> None:
        with tempfile.TemporaryDirectory(prefix="workerd-native-test.") as temporary:
            copied = Path(temporary) / "workerd-native"
            shutil.copytree(BUNDLE, copied)
            notice = copied / "notices" / "sqlite-3.47.0" / "LICENSE.md"
            notice.write_bytes(notice.read_bytes() + b"tampered\n")
            result = run(
                VERIFY,
                "--integrity-only",
                "--bundle",
                str(copied),
                "--package-lock",
                str(PACKAGE_LOCK),
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(b"digest drift", result.stderr)

    def test_missing_source_obligation_decision_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory(prefix="workerd-native-source-test.") as temporary:
            copied = Path(temporary) / "workerd-native"
            shutil.copytree(BUNDLE, copied)
            lock_path = copied / "LOCK.json"
            lock = json.loads(lock_path.read_text(encoding="utf-8"))
            del lock["components"][0]["source_obligation"]
            lock_path.write_text(json.dumps(lock, indent=2, sort_keys=True) + "\n", encoding="utf-8")
            result = run(
                VERIFY,
                "--integrity-only",
                "--bundle",
                str(copied),
                "--package-lock",
                str(PACKAGE_LOCK),
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(b"source-obligation decision", result.stderr)

    def test_incomplete_licence_tree_audit_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory(prefix="workerd-native-audit-test.") as temporary:
            copied = Path(temporary) / "workerd-native"
            shutil.copytree(BUNDLE, copied)
            lock_path = copied / "LOCK.json"
            lock = json.loads(lock_path.read_text(encoding="utf-8"))
            lock["legal_gate"]["license_tree_audits"]["perfetto"]["legal_like_files"] = ["LICENSE"]
            lock_path.write_text(json.dumps(lock, indent=2, sort_keys=True) + "\n", encoding="utf-8")
            result = run(
                VERIFY,
                "--integrity-only",
                "--bundle",
                str(copied),
                "--package-lock",
                str(PACKAGE_LOCK),
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(b"licence-tree audit legal_like_files drifted", result.stderr)


if __name__ == "__main__":
    unittest.main()
