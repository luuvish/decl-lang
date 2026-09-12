#!/usr/bin/env python3
"""Run a frozen, correctness-checked serial process comparison on POSIX hosts."""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import json
import math
import os
import platform
import re
import signal
import statistics
import subprocess
import sys
import threading
import time
from pathlib import Path


def digest(path: Path) -> str:
    with path.open("rb") as stream:
        result = hashlib.sha256()
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            result.update(block)
    return result.hexdigest()


def encoded(value: object) -> bytes:
    return (json.dumps(value, indent=2, sort_keys=True) + "\n").encode()


def write_new(path: Path, value: object) -> None:
    with path.open("xb") as stream:
        stream.write(encoded(value))


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def check_manifest(config: dict) -> None:
    require(config.get("schema_version") == 1, "schema_version must be 1")
    require(config.get("warmups", 1) == 1, "exactly one discarded warmup is required")
    samples = config.get("samples", 3)
    require(
        type(samples) is int and samples > 0 and samples % 2 == 1,
        "samples must be a positive odd integer",
    )
    require(
        Path(config["cwd"]).is_absolute() and Path(config["cwd"]).is_dir(),
        "cwd must be an existing absolute directory",
    )
    require(bool(config.get("artifacts")), "freeze relevant files in artifacts")
    for artifact in config["artifacts"]:
        require(Path(artifact["path"]).is_absolute(), "artifact paths must be absolute")
        require(
            bool(re.fullmatch(r"[0-9a-f]{64}", artifact["sha256"])),
            "artifact sha256 must contain 64 lowercase hexadecimal digits",
        )
    for group in ("variants", "cases"):
        require(bool(config.get(group)), f"{group} must not be empty")
        names = [item["id"] for item in config[group]]
        require(len(names) == len(set(names)), f"duplicate {group} ids")
        require(
            all(re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]*", name) for name in names),
            f"{group} ids must contain only letters, digits, underscores and hyphens",
        )
    for variant in config["variants"]:
        argv = variant["argv"]
        require(
            bool(argv) and all(isinstance(arg, str) for arg in argv),
            "argv must be a nonempty array of strings",
        )
        require(Path(argv[0]).is_absolute(), "argv executable must be an absolute path")
        require(
            any(Path(a["path"]).resolve() == Path(argv[0]).resolve() for a in config["artifacts"]),
            "freeze every executable in artifacts",
        )
    for case in config["cases"]:
        expected = case["expected"]
        require(
            type(expected.get("exit")) is int and 0 <= expected["exit"] <= 255,
            "expected.exit must be an integer from 0 through 255",
        )
        hashes = [expected.get(f"{name}_sha256") for name in ("stdout", "stderr", "output")]
        require(any(hashes), "each case needs an expected stdout, stderr or output hash")
        require(
            all(value is None or re.fullmatch(r"[0-9a-f]{64}", value) for value in hashes),
            "expected hashes must contain 64 lowercase hexadecimal digits",
        )
        if expected.get("output_sha256"):
            require(
                all(
                    any("{output}" in arg for arg in variant["argv"])
                    for variant in config["variants"]
                ),
                "output hash requires {output} in every variant command",
            )
        require("output" not in case.get("vars", {}), "output is a reserved variable")
        timeout = case.get("timeout_s", config.get("timeout_s", 120))
        require(
            isinstance(timeout, (int, float)) and math.isfinite(timeout) and timeout > 0,
            "timeout_s must be a positive finite number",
        )
    for item in [config, *config["variants"]]:
        require(
            all(isinstance(k, str) and isinstance(v, str) for k, v in item.get("env", {}).items()),
            "env must map strings to strings",
        )


def check_artifacts(config: dict) -> None:
    for artifact in config["artifacts"]:
        path = Path(artifact["path"])
        require(
            path.is_file() and digest(path) == artifact["sha256"],
            f"artifact fingerprint mismatch: {path}",
        )


def plan(config: dict) -> list[dict]:
    rows = []
    variants = config["variants"]
    for case in config["cases"]:
        for round_number in range(config.get("samples", 3) + 1):
            # With two variants this is AB, BA, AB, BA. More variants rotate.
            offset = round_number % len(variants)
            ordered = variants[offset:] + variants[:offset]
            if len(variants) > 2 and (round_number // len(variants)) % 2:
                ordered = list(reversed(ordered))
            for variant in ordered:
                rows.append(
                    {
                        "id": f"{case['id']}-r{round_number}-{variant['id']}",
                        "case": case["id"],
                        "variant": variant["id"],
                        "round": round_number,
                        "warmup": round_number == 0,
                    }
                )
    return rows


def kill_group(pid: int) -> None:
    with contextlib.suppress(ProcessLookupError):
        os.killpg(pid, signal.SIGKILL)


def run_one(spec: dict, config: dict, root: Path) -> dict:
    case = next(case for case in config["cases"] if case["id"] == spec["case"])
    variant = next(variant for variant in config["variants"] if variant["id"] == spec["variant"])
    prefix = root / "runs" / spec["id"]
    paths = {name: Path(str(prefix) + "." + name) for name in ("stdout", "stderr", "output")}
    require(
        not any(path.exists() for path in paths.values()),
        f"incomplete attempt files exist: {prefix}; use a new destination",
    )
    variables = {**case.get("vars", {}), "output": str(paths["output"])}
    argv = []
    for argument in variant["argv"]:
        for key, value in variables.items():
            argument = argument.replace("{" + key + "}", str(value))
        argv.append(argument)
    env = {**os.environ, **config.get("env", {}), **variant.get("env", {})}
    timeout = case.get("timeout_s", config.get("timeout_s", 120))
    result = {
        **spec,
        "command": argv,
        "cwd": config["cwd"],
        "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "timeout_s": timeout,
        "status": "launch_error",
        "accepted": False,
        "resource_scope": "wait4 child process; not sampled process-tree RSS",
    }
    finished = threading.Event()
    timed_out = threading.Event()
    process = None
    watchdog = None
    with paths["stdout"].open("xb") as stdout, paths["stderr"].open("xb") as stderr:
        started = time.perf_counter()
        try:
            process = subprocess.Popen(
                argv,
                cwd=config["cwd"],
                env=env,
                stdout=stdout,
                stderr=stderr,
                start_new_session=True,
            )
            write_new(
                Path(str(prefix) + ".launch.json"),
                {
                    "harness_pid": os.getpid(),
                    "child_pid": process.pid,
                    "child_pgid": process.pid,
                    "started_utc": result["started_utc"],
                    "command": argv,
                },
            )

            def watch() -> None:
                if not finished.wait(timeout):
                    timed_out.set()
                    kill_group(process.pid)

            watchdog = threading.Thread(target=watch, daemon=True)
            watchdog.start()
            _, status, usage = os.wait4(process.pid, 0)
            elapsed = time.perf_counter() - started
            finished.set()
            watchdog.join()
            process.returncode = os.waitstatus_to_exitcode(status)
            result.update(
                exit=process.returncode,
                attempt_wall_s=elapsed,
                cpu_s=usage.ru_utime + usage.ru_stime,
                peak_rss_bytes=usage.ru_maxrss * (1 if sys.platform == "darwin" else 1024),
                status="timeout" if timed_out.is_set() else "completed",
            )
        except OSError as error:
            result.update(error=str(error), attempt_wall_s=time.perf_counter() - started)
        finally:
            finished.set()
            if process is not None:
                kill_group(process.pid)
                if process.returncode is None:
                    process.wait()
            if watchdog is not None:
                watchdog.join()
    result["files"] = {name: str(path.relative_to(root)) for name, path in paths.items()}
    result["sha256"] = {
        name: digest(path) if path.is_file() else None for name, path in paths.items()
    }
    expected = case["expected"]
    result["validation"] = {"exit": result.get("exit") == expected["exit"]}
    for name in paths:
        if f"{name}_sha256" in expected:
            result["validation"][name] = result["sha256"][name] == expected[f"{name}_sha256"]
    if result["status"] == "completed":
        if result.get("exit", 0) < 0:
            result["status"] = "signal"
        elif not result["validation"]["exit"]:
            result["status"] = "unexpected_exit"
        elif not all(result["validation"].values()):
            result["status"] = "output_mismatch"
        else:
            result.update(status="accepted", accepted=True)
    result["latency_s"] = result.get("attempt_wall_s") if result["accepted"] else None
    return result


def summarize(config: dict, rows: list[dict]) -> dict:
    groups = []
    ratios = []
    reference = config["variants"][0]["id"]
    for case in config["cases"]:
        for variant in config["variants"]:
            attempts = [
                row
                for row in rows
                if row["case"] == case["id"]
                and row["variant"] == variant["id"]
                and not row["warmup"]
            ]
            accepted = [row for row in attempts if row["accepted"]]
            times = [row["latency_s"] for row in accepted]
            group = {
                "case": case["id"],
                "variant": variant["id"],
                "accepted_n": len(accepted),
                "planned_n": config.get("samples", 3),
                "complete": len(accepted) == config.get("samples", 3),
                "statuses": [row["status"] for row in attempts],
                "samples_s": times,
                "median_s": statistics.median(times) if times else None,
                "min_s": min(times) if times else None,
                "max_s": max(times) if times else None,
                "median_peak_rss_bytes": statistics.median(
                    row["peak_rss_bytes"] for row in accepted
                )
                if times
                else None,
            }
            groups.append(group)
            if variant["id"] == reference:
                continue
            paired = []
            for row in accepted:
                before = next(
                    (
                        other
                        for other in rows
                        if other["case"] == case["id"]
                        and other["variant"] == reference
                        and other["round"] == row["round"]
                        and other["accepted"]
                    ),
                    None,
                )
                if before is not None and before["latency_s"] > 0:
                    paired.append(
                        {"round": row["round"], "ratio": row["latency_s"] / before["latency_s"]}
                    )
            ratios.append(
                {
                    "case": case["id"],
                    "variant": variant["id"],
                    "reference": reference,
                    "paired_n": len(paired),
                    "planned_n": config.get("samples", 3),
                    "complete": len(paired) == config.get("samples", 3),
                    "paired_candidate_over_reference": paired,
                    "median_paired_ratio": statistics.median(pair["ratio"] for pair in paired)
                    if paired
                    else None,
                }
            )
    return {
        "groups": groups,
        "ratios": ratios,
        "attempt_count": len(rows),
        "all_accepted": all(row["accepted"] for row in rows),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--resume", action="store_true")
    args = parser.parse_args()
    require(os.name == "posix" and hasattr(os, "wait4"), "this harness requires POSIX wait4")
    config = json.loads(args.manifest.read_text())
    check_manifest(config)
    check_artifacts(config)
    root = args.output.resolve()
    identity = {
        "manifest_sha256": hashlib.sha256(encoded(config)).hexdigest(),
        "harness_sha256": digest(Path(__file__).resolve()),
        "host": [platform.system(), platform.release(), platform.machine(), platform.node()],
        "python": [sys.executable, sys.version],
        "effective_env_sha256": {
            variant["id"]: hashlib.sha256(
                encoded(
                    {
                        key: value
                        for key, value in {
                            **os.environ,
                            **config.get("env", {}),
                            **variant.get("env", {}),
                        }.items()
                        if key not in {"PWD", "OLDPWD", "SHLVL", "_"}
                    }
                )
            ).hexdigest()
            for variant in config["variants"]
        },
    }
    if root.exists() and any(root.iterdir()):
        require(args.resume, "destination is not empty; use --resume for the same experiment")
        require((root / "identity.json").is_file(), "unrelated destination has no identity.json")
        require(
            json.loads((root / "identity.json").read_text()) == identity,
            "manifest, harness, host, runtime or environment fingerprint changed; "
            "use a new destination",
        )
    else:
        require(not args.resume, "cannot resume an experiment that does not exist")
        root.mkdir(parents=True, exist_ok=True)
        write_new(root / "identity.json", identity)
        write_new(root / "manifest.json", config)
        write_new(
            root / "host.json",
            {"platform": platform.platform(), "python": sys.version, "cpu_count": os.cpu_count()},
        )
        (root / "runs").mkdir()
    lock = root / ".running"
    with lock.open("x") as stream:
        stream.write(str(os.getpid()) + "\n")
    try:
        rows = []
        failed = set()
        for spec in plan(config):
            path = root / "runs" / (spec["id"] + ".json")
            key = (spec["case"], spec["variant"])
            if path.exists():
                row = json.loads(path.read_text())
                require(
                    all(row.get(key) == value for key, value in spec.items()),
                    f"stored attempt does not match plan: {path}",
                )
                for name, relative in row.get("files", {}).items():
                    artifact = root / relative
                    actual = digest(artifact) if artifact.is_file() else None
                    require(
                        actual == row["sha256"][name],
                        f"stored attempt artifact changed: {artifact}",
                    )
            elif key in failed:
                row = {
                    **spec,
                    "status": "skipped_after_failure",
                    "accepted": False,
                    "latency_s": None,
                }
                write_new(path, row)
            else:
                check_artifacts(config)
                row = run_one(spec, config, root)
                check_artifacts(config)
                write_new(path, row)
                print(
                    json.dumps({key: row.get(key) for key in ("id", "status", "attempt_wall_s")}),
                    flush=True,
                )
            rows.append(row)
            if not row["accepted"]:
                failed.add(key)
        report = summarize(config, rows)
        temporary = root / "summary.pending"
        temporary.write_bytes(encoded(report))
        temporary.replace(root / "summary.json")
        return 0 if report["all_accepted"] else 1
    finally:
        lock.unlink()


if __name__ == "__main__":

    def interrupt(_signum: int, _frame: object) -> None:
        raise KeyboardInterrupt

    signal.signal(signal.SIGTERM, interrupt)
    try:
        raise SystemExit(main())
    except KeyboardInterrupt:
        print("comparison interrupted; child process group cleaned up", file=sys.stderr)
        raise SystemExit(130) from None
    except (ValueError, KeyError, TypeError, OSError) as error:
        print(f"comparison error: {error}", file=sys.stderr)
        raise SystemExit(2) from error
