#!/usr/bin/env python3
"""Isolated M0 flock/descriptor/signal experiment, using only owned processes."""
import fcntl
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time

SELF = str(Path(__file__).resolve())


def marker(path, value):
    temporary = path.with_suffix(".pending")
    temporary.write_text(json.dumps(value))
    temporary.replace(path)


def busy(path):
    with path.open("a") as handle:
        try:
            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
            return False
        except BlockingIOError:
            return True


def wait_file(path):
    deadline = time.monotonic() + 8
    while time.monotonic() < deadline:
        if path.exists():
            return json.loads(path.read_text())
        time.sleep(0.02)
    raise RuntimeError(f"child did not become ready: {path}")


def role(kind, directory, fd, propagate):
    folder = Path(directory)
    if kind == "leaf":
        marker(folder / "leaf.json", {"pid": os.getpid(), "pgid": os.getpgrp()})
        time.sleep(25)  # bounded even if the harness is interrupted
        return
    if kind == "middle":
        kwargs = {"pass_fds": (fd,)} if propagate == "yes" else {}
        child = subprocess.Popen([sys.executable, SELF, "leaf", directory, str(fd), propagate], **kwargs)
        marker(folder / "middle.json", {"pid": os.getpid(), "leaf": child.pid})
        child.wait()
        return
    with (folder / "lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        child = subprocess.Popen([sys.executable, SELF, "middle", directory, str(lock.fileno()), propagate],
                                 pass_fds=(lock.fileno(),), start_new_session=True)
        received = []
        def stop(signum, _frame):
            received.append(signum)
        signal.signal(signal.SIGINT, stop)
        signal.signal(signal.SIGTERM, stop)
        marker(folder / "controller.json", {"pid": os.getpid(), "worker_group": child.pid})
        while child.poll() is None and not received:
            time.sleep(0.02)
        if received:
            os.killpg(child.pid, signal.SIGTERM)
            child.wait(timeout=5)
            raise SystemExit(128 + received[0])


def main():
    outcomes = []
    for name, propagate, action in (
        ("direct-survivor", "no", "kill-controller"),
        ("nested-closes-fd", "no", "kill-controller-and-middle"),
        ("nested-explicit-pass", "yes", "kill-controller-and-middle"),
        ("sigint-group", "no", "sigint"),
        ("sigterm-group", "no", "sigterm"),
    ):
        with tempfile.TemporaryDirectory(prefix="seasnail-m0-lock-") as directory:
            folder = Path(directory)
            controller = subprocess.Popen([sys.executable, SELF, "controller", directory, "-1", propagate],
                                          start_new_session=True)
            group = None
            try:
                info = wait_file(folder / "controller.json")
                group = info["worker_group"]
                leaf = wait_file(folder / "leaf.json")
                assert busy(folder / "lock"), "concurrent writer must be rejected"
                if action.startswith("kill-controller"):
                    controller.kill()
                    controller.wait(timeout=5)
                    if action.endswith("and-middle"):
                        os.kill(group, signal.SIGKILL)
                    time.sleep(0.2)
                    os.kill(leaf["pid"], 0)  # the residual leaf really still exists
                    is_busy = busy(folder / "lock")
                    expected = name != "nested-closes-fd"
                    assert is_busy == expected, (name, is_busy, expected)
                    outcomes.append({"case": name, "residual_leaf_alive": True,
                                     "lock_busy": is_busy, "expected": expected,
                                     "controller_exit": controller.returncode})
                else:
                    sig = signal.SIGINT if action == "sigint" else signal.SIGTERM
                    controller.send_signal(sig)
                    code = controller.wait(timeout=8)
                    assert code == 128 + sig
                    assert not busy(folder / "lock")
                    outcomes.append({"case": name, "controller_exit": code,
                                     "lock_busy_after_cleanup": False})
            finally:
                if controller.poll() is None:
                    controller.kill()
                    controller.wait(timeout=5)
                if group is not None:
                    try:
                        os.killpg(group, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
    print(json.dumps({"cases": outcomes, "all_expectations_met": True}, indent=2))


if __name__ == "__main__":
    if len(sys.argv) > 1:
        role(sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4])
    else:
        main()
