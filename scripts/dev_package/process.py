"""Shared kernel lock, per-stage supervisor, fail-closed residual group checks."""
import fcntl
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

from .state import atomic_json, now, read_json, safe_path


class Busy(Exception): pass
class Residual(Exception): pass
class Interrupted(Exception):
    def __init__(self, number): self.number = number


class StageTimeout(Exception): pass


def process_identity(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError: return None
    except PermissionError: return 'unknown'
    try:
        row = subprocess.check_output(['/bin/ps', '-p', str(pid), '-o', 'lstart='],
                                      stderr=subprocess.DEVNULL, timeout=5, text=True).strip()
        return row or 'unknown'
    except (OSError, subprocess.SubprocessError): return 'unknown'


def group_activity(pgid):
    if not isinstance(pgid, int) or pgid <= 1: return 'unknown'
    try: os.killpg(pgid, 0)
    except ProcessLookupError: return 'inactive'
    except PermissionError: return 'unknown'
    return 'active'


def lock_path(root):
    return safe_path(root, 'build/dev-package/repository.lock')


def lock_busy(root):
    path = lock_path(root)
    if not path.exists(): return False
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        try: fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError: return True
        return False
    finally: os.close(fd)


def run_activity(root, directory, data):
    owner = data.get('owner')
    owner_unknown = False
    if owner and data.get('status') == 'running':
        identity = process_identity(owner['pid'])
        if identity == 'unknown' or owner.get('identity') == 'unknown':
            owner_unknown = identity is not None
        elif identity is not None and identity == owner.get('identity'): return 'active'
    groups_unknown = False
    for path in directory.glob('process-*.json'):
        if path.is_symlink(): return 'unknown'
        record = read_json(path)
        activity = group_activity(record.get('pgid'))
        if activity == 'active':
            identity = process_identity(record.get('pid', 0))
            if identity not in (None, 'unknown') and identity == record.get('identity'): return 'active'
            groups_unknown = True
        groups_unknown |= activity == 'unknown'
    if owner_unknown or groups_unknown: return 'unknown'
    # Kernel lock alone cannot identify which historic run owns it.
    if data.get('status') == 'running' and lock_busy(root): return 'unknown'
    return 'inactive'


class RepositoryLock:
    def __init__(self, root): self.root, self.fd = Path(root), None
    def __enter__(self):
        path = lock_path(self.root)
        path.parent.mkdir(parents=True, exist_ok=True)
        self.fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
        try:
            try: fcntl.flock(self.fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError: raise Busy('Another modifying task holds the repository lock')
            runs = safe_path(self.root, 'dist/dev-package/runs')
            if runs.exists():
                for directory in runs.iterdir():
                    if directory.is_symlink(): raise Residual(f'Unknown run path: {directory}')
                    for marker in directory.glob('process-*.json'):
                        if marker.is_symlink(): raise Residual(f'Unknown process record: {marker}')
                        record = read_json(marker)
                        if group_activity(record.get('pgid')) != 'inactive':
                            raise Residual(f'Recorded process group may still be active: {marker}; inspect it before retrying')
            return self
        except BaseException:
            os.close(self.fd); self.fd = None
            raise
    def __exit__(self, *_):
        if self.fd is not None: os.close(self.fd); self.fd = None


class Controller:
    def __init__(self, root, run, lock):
        self.root, self.run, self.lock = Path(root), run, lock
        self.signal = None
        self.previous = {}
        self.counter = 0
    def __enter__(self):
        for number in (signal.SIGINT, signal.SIGTERM):
            self.previous[number] = signal.signal(number, self.on_signal)
        return self
    def on_signal(self, number, _frame): self.signal = number
    def __exit__(self, *_):
        for number, handler in self.previous.items(): signal.signal(number, handler)
    def execute(self, stage, command, *, env=None, cwd=None, timeout=None):
        from .checks import tool_environment
        if self.signal: raise Interrupted(self.signal)
        self.counter += 1
        marker = self.run.directory / f'process-{self.counter:04d}.json'
        request = self.run.directory / f'command-{self.counter:04d}.json'
        # Persist intent first; worker persists its group BEFORE spawning any tools.
        atomic_json(request, dict(command=[str(x) for x in command], cwd=str(cwd or self.root),
                                  marker=str(marker), lock_fd=self.lock.fd))
        self.run.data['stage'] = stage; self.run.save()
        self.run.event('stage_started', stage=stage)
        started = time.monotonic()
        with Path(self.run.data['log_path']).open('a') as log:
            child = subprocess.Popen([sys.executable, '-B', '-m', 'scripts.dev_package.worker', str(request)],
                                     cwd=self.root, env=tool_environment(env), stdout=log, stderr=subprocess.STDOUT,
                                     pass_fds=(self.lock.fd,), start_new_session=True)
            last_progress = started
            while child.poll() is None:
                timed_out = timeout is not None and time.monotonic() - started >= timeout
                if self.signal or timed_out:
                    # Live immediate child cannot have its PID reused until reaped.
                    os.killpg(child.pid, signal.SIGTERM)
                    try: child.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        os.killpg(child.pid, signal.SIGKILL); child.wait()
                    if self.signal: raise Interrupted(self.signal)
                    raise StageTimeout(f'{stage} exceeded {timeout} seconds')
                if time.monotonic() - last_progress >= 30:
                    event = self.run.event('stage_progress', stage=stage,
                                          elapsed_seconds=round(time.monotonic()-started, 1))
                    print('DEV_PACKAGE_EVENT ' + __import__('json').dumps(event), file=sys.stderr, flush=True)
                    last_progress = time.monotonic()
                time.sleep(.05)
        if self.signal: raise Interrupted(self.signal)
        code = child.returncode
        self.run.event('stage_finished', stage=stage, subprocess_exit_code=code,
                       elapsed_seconds=round(time.monotonic()-started, 3))
        if group_activity(child.pid) != 'inactive':
            raise Residual(f'Stage left a live or unknown process group: {marker}')
        return code
