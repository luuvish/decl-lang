#!/usr/bin/env python3
"""Exercise the process-comparison harness with tiny, isolated subprocesses."""

from __future__ import annotations

import contextlib
import copy
import hashlib
import json
import os
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

HARNESS = Path(__file__).resolve().with_name("compare.py")
PYTHON = Path(sys.executable).resolve()
EMPTY = hashlib.sha256(b"").hexdigest()


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def wait_until(predicate, timeout: float = 5) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.02)
    return bool(predicate())


def process_alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    # A killed orphan can briefly remain as a zombie until its new parent reaps it.
    stat = subprocess.run(
        ["ps", "-o", "stat=", "-p", str(pid)], capture_output=True, text=True, check=False
    )
    return bool(stat.stdout.strip()) and not stat.stdout.strip().startswith("Z")


@unittest.skipUnless(os.name == "posix" and hasattr(os, "wait4"), "POSIX wait4 is required")
class CompareTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="decl-compare-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.script = self.root / "emit.py"
        self.script.write_text(
            "import pathlib, sys\npathlib.Path(sys.argv[1]).write_bytes(b'ok\\n')\n"
        )
        self.manifest_path = self.root / "manifest.json"
        self.output = self.root / "results"
        self.config = {
            "schema_version": 1,
            "cwd": str(self.root),
            "warmups": 1,
            "samples": 1,
            "timeout_s": 5,
            "artifacts": [
                {"path": str(PYTHON), "sha256": digest(PYTHON)},
                {"path": str(self.script), "sha256": digest(self.script)},
            ],
            "variants": [
                {"id": name, "argv": [str(PYTHON), str(self.script), "{output}"]}
                for name in ("before", "after")
            ],
            "cases": [
                {
                    "id": "tiny",
                    "expected": {
                        "exit": 0,
                        "stdout_sha256": EMPTY,
                        "stderr_sha256": EMPTY,
                        "output_sha256": hashlib.sha256(b"ok\n").hexdigest(),
                    },
                }
            ],
        }

    def command(self, config: dict | None = None, *, resume: bool = False) -> list[str]:
        self.manifest_path.write_text(json.dumps(self.config if config is None else config))
        argv = [str(PYTHON), str(HARNESS), str(self.manifest_path), "--output", str(self.output)]
        if resume:
            argv.append("--resume")
        return argv

    def run_comparison(
        self,
        config: dict | None = None,
        *,
        resume: bool = False,
        expected_exit: int = 0,
        env: dict[str, str] | None = None,
    ) -> subprocess.CompletedProcess[str]:
        result = subprocess.run(
            self.command(config, resume=resume),
            capture_output=True,
            text=True,
            env=env,
            timeout=20,
            check=False,
        )
        self.assertEqual(result.returncode, expected_exit, result.stdout + result.stderr)
        return result

    def summary(self) -> dict:
        return json.loads((self.output / "summary.json").read_text())

    def attempts(self) -> list[dict]:
        return [
            json.loads(path.read_text())
            for path in sorted((self.output / "runs").glob("*.json"))
            if not path.name.endswith(".launch.json")
        ]

    def test_accepted_output_and_resume_preserve_attempts(self) -> None:
        self.run_comparison()
        report = self.summary()
        self.assertTrue(report["all_accepted"])
        self.assertEqual(report["attempt_count"], 4)
        self.assertTrue(all(group["complete"] for group in report["groups"]))
        self.assertEqual(report["ratios"][0]["paired_n"], 1)
        self.assertTrue(report["ratios"][0]["complete"])
        snapshot = {
            path: (digest(path), path.stat().st_mtime_ns)
            for path in (self.output / "runs").iterdir()
        }
        result = self.run_comparison(resume=True)
        self.assertEqual(result.stdout, "")
        self.assertEqual(
            snapshot,
            {
                path: (digest(path), path.stat().st_mtime_ns)
                for path in (self.output / "runs").iterdir()
            },
        )
        self.run_comparison(expected_exit=2)

    def test_mismatch_skips_later_attempts_without_latency(self) -> None:
        config = copy.deepcopy(self.config)
        config["cases"][0]["expected"]["output_sha256"] = hashlib.sha256(b"wrong").hexdigest()
        self.run_comparison(config, expected_exit=1)
        rows = self.attempts()
        self.assertEqual([row["status"] for row in rows].count("output_mismatch"), 2)
        self.assertEqual([row["status"] for row in rows].count("skipped_after_failure"), 2)
        self.assertTrue(all(row["latency_s"] is None for row in rows))
        self.assertTrue(all(group["median_s"] is None for group in self.summary()["groups"]))
        self.assertFalse(self.summary()["ratios"][0]["complete"])
        self.run_comparison(config, resume=True, expected_exit=1)

    def test_timeout_has_no_accepted_latency(self) -> None:
        config = copy.deepcopy(self.config)
        config["timeout_s"] = 0.1
        config["variants"] = [
            {"id": "slow", "argv": [str(PYTHON), "-c", "import time; time.sleep(30)"]}
        ]
        config["cases"][0]["expected"] = {"exit": 0, "stdout_sha256": EMPTY}
        self.run_comparison(config, expected_exit=1)
        rows = self.attempts()
        self.assertEqual(rows[0]["status"], "timeout")
        self.assertIsNone(rows[0]["latency_s"])
        self.assertEqual(rows[1]["status"], "skipped_after_failure")

    def test_expected_nonzero_exit_is_hash_checked(self) -> None:
        config = copy.deepcopy(self.config)
        config["variants"] = [
            {
                "id": "diagnostic",
                "argv": [
                    str(PYTHON),
                    "-c",
                    "import sys; sys.stderr.write('error\\n'); sys.exit(7)",
                ],
            }
        ]
        config["cases"][0]["expected"] = {
            "exit": 7,
            "stderr_sha256": hashlib.sha256(b"error\n").hexdigest(),
        }
        self.run_comparison(config)
        self.assertTrue(self.summary()["all_accepted"])

    def test_even_samples_are_rejected(self) -> None:
        config = copy.deepcopy(self.config)
        config["samples"] = 2
        self.run_comparison(config, expected_exit=2)
        self.assertFalse(self.output.exists())

    def test_literal_braces_are_not_format_expressions(self) -> None:
        config = copy.deepcopy(self.config)
        config["variants"] = [
            {
                "id": "literal",
                "argv": [
                    str(PYTHON),
                    "-c",
                    "import pathlib,sys; x={'value': 'ok\\n'}; "
                    "pathlib.Path(sys.argv[1]).write_text(x['value'])",
                    "{output}",
                ],
            }
        ]
        self.run_comparison(config)
        self.assertTrue(self.summary()["all_accepted"])

    def test_manifest_environment_and_artifact_changes_are_rejected(self) -> None:
        self.run_comparison()
        changed = copy.deepcopy(self.config)
        changed["samples"] = 3
        self.run_comparison(changed, resume=True, expected_exit=2)
        changed_env = {
            **os.environ,
            "DECL_COMPARE_TEST_CHANGE": os.environ.get("DECL_COMPARE_TEST_CHANGE", "") + "/changed",
        }
        self.run_comparison(resume=True, expected_exit=2, env=changed_env)
        self.script.write_text(self.script.read_text() + "# changed frozen artifact\n")
        self.run_comparison(resume=True, expected_exit=2)

    def test_tampered_saved_output_is_rejected(self) -> None:
        self.run_comparison()
        (self.output / "runs" / "tiny-r0-before.output").write_bytes(b"tampered")
        self.run_comparison(resume=True, expected_exit=2)

    def check_signal_cleanup(self, sig: signal.Signals) -> None:
        child_pid_file = self.root / "descendant.pid"
        config = copy.deepcopy(self.config)
        config["timeout_s"] = 30
        config["variants"] = [
            {
                "id": "waiting",
                "argv": [
                    str(PYTHON),
                    "-c",
                    "import pathlib,subprocess,sys,time; "
                    "p=subprocess.Popen([sys.executable,'-c','import time; time.sleep(30)']); "
                    "pathlib.Path(sys.argv[1]).write_text(str(p.pid)); time.sleep(30)",
                    str(child_pid_file),
                ],
            }
        ]
        config["cases"][0]["expected"] = {"exit": 0, "stdout_sha256": EMPTY}
        process = subprocess.Popen(
            self.command(config), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True
        )
        receipt = self.output / "runs" / "tiny-r0-waiting.launch.json"
        child_pid = None
        descendant_pid = None
        try:
            self.assertTrue(
                wait_until(
                    lambda: (
                        receipt.exists()
                        and receipt.stat().st_size > 0
                        and child_pid_file.exists()
                        and child_pid_file.stat().st_size > 0
                    )
                )
            )
            launch = json.loads(receipt.read_text())
            child_pid = launch["child_pid"]
            descendant_pid = int(child_pid_file.read_text())
            self.assertEqual(launch["child_pgid"], child_pid)
            self.assertEqual(launch["harness_pid"], process.pid)
            self.assertEqual(os.getpgid(child_pid), child_pid)
            self.assertEqual(os.getpgid(descendant_pid), child_pid)
            process.send_signal(sig)
            stdout, stderr = process.communicate(timeout=10)
            self.assertEqual(process.returncode, 130, stdout + stderr)
            self.assertTrue(wait_until(lambda: not process_alive(child_pid)))
            self.assertTrue(wait_until(lambda: not process_alive(descendant_pid)))
            self.assertFalse((self.output / ".running").exists())
            self.assertFalse((self.output / "runs" / "tiny-r0-waiting.json").exists())
        finally:
            # Keep failed tests from leaving their own deliberately sleeping processes behind.
            if child_pid is None and receipt.exists():
                child_pid = json.loads(receipt.read_text())["child_pid"]
            if child_pid is not None and (
                process_alive(child_pid)
                or (descendant_pid is not None and process_alive(descendant_pid))
            ):
                with contextlib.suppress(ProcessLookupError):
                    os.killpg(child_pid, signal.SIGKILL)
            if process.poll() is None:
                process.kill()
            process.communicate(timeout=10)

    def test_sigterm_cleans_child_group_and_preserves_receipt(self) -> None:
        self.check_signal_cleanup(signal.SIGTERM)

    def test_sigint_cleans_child_group_and_preserves_receipt(self) -> None:
        self.check_signal_cleanup(signal.SIGINT)


if __name__ == "__main__":
    unittest.main()
