#!/usr/bin/env python3
"""M0 evidence collector only; not the public dev-package implementation.

Usage: python3 run_experiment.py NAME -- COMMAND ARG...
Logs stay in ignored dist/dev-package/m0-20261011. Disk samples measure the
experiment tree and whole-volume free space; the latter includes unrelated I/O.
"""
import argparse
import datetime
import json
import os
from pathlib import Path
import shutil
import subprocess
import time

ROOT = Path(__file__).resolve().parents[4]
EVIDENCE = ROOT / "dist/dev-package/m0-20261011"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--measure-dir", type=Path, help="existing experiment subtree to sample")
    parser.add_argument("name")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command or not args.name.replace("-", "").replace("_", "").isalnum():
        parser.error("a safe name and command are required")
    EVIDENCE.mkdir(parents=True, exist_ok=True)
    measure_dir = args.measure_dir.resolve() if args.measure_dir else EVIDENCE
    if not measure_dir.is_dir() or not measure_dir.is_relative_to(EVIDENCE):
        parser.error("measurement directory must exist inside the experiment tree")
    result_path = EVIDENCE / f"{args.name}.json"
    log_path = EVIDENCE / f"{args.name}.log"
    if result_path.exists() or log_path.exists():
        parser.error("evidence already exists; choose a distinct experiment name")
    started = datetime.datetime.now(datetime.timezone.utc).isoformat()
    start = time.monotonic()
    samples = []
    with log_path.open("w") as log:
        child = subprocess.Popen(command, cwd=ROOT, stdout=log, stderr=subprocess.STDOUT)
        print(json.dumps({"experiment": args.name, "pid": child.pid,
                          "log": str(log_path)}), flush=True)
        try:
            while True:
                usage = shutil.disk_usage(EVIDENCE)
                measured = subprocess.run(["du", "-sk", str(measure_dir)],
                                          capture_output=True, text=True)
                samples.append({"elapsed_seconds": round(time.monotonic() - start, 2),
                                "volume_free_bytes": usage.free,
                                "tree_allocated_bytes": int(measured.stdout.split()[0]) * 1024
                                if measured.returncode == 0 else None})
                try:
                    code = child.wait(timeout=5)
                    break
                except subprocess.TimeoutExpired:
                    pass
        except KeyboardInterrupt:
            child.terminate()
            try:
                child.wait(timeout=10)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
            code = 130
    result = {"experiment": args.name, "command": command, "cwd": str(ROOT),
              "started_at": started, "elapsed_seconds": round(time.monotonic() - start, 2),
              "exit_code": code, "log_path": str(log_path), "samples": samples,
              "measure_dir": str(measure_dir),
              "selected_environment": {k: os.environ.get(k) for k in
                  ("MACOSX_DEPLOYMENT_TARGET", "CARGO_TARGET_DIR", "CARGO_NET_OFFLINE", "TMPDIR", "revision", "GIT_CEILING_DIRECTORIES")}}
    result_path.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({k: v for k, v in result.items() if k != "samples"}), flush=True)
    raise SystemExit(code if code >= 0 else 128 - code)


if __name__ == "__main__":
    main()
