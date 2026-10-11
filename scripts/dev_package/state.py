"""Atomic, durable schema-v1 state and read-only run discovery."""
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import uuid

RUN_ID = re.compile(r"^[0-9]{8}T[0-9]{12}Z-[0-9a-f]{12}$")
TERMINAL = {'checked', 'succeeded', 'blocked', 'failed', 'interrupted', 'abandoned'}


def now():
    return dt.datetime.now(dt.timezone.utc).isoformat().replace('+00:00', 'Z')


def new_id():
    return dt.datetime.now(dt.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ-') + uuid.uuid4().hex[:12]


def error(code, stage, reason, *, actual=None, required=None, path=None,
          retry='investigate', action='Inspect the evidence before retrying', child_code=None):
    return dict(code=code, stage=stage, reason=reason, actual=actual, required=required,
                path=str(path) if path else None, retry_policy=retry,
                required_action=action, subprocess_exit_code=child_code)


def result(operation, **fields):
    base = dict(schema_version=1, operation=operation, run_id=None, status='blocked',
                stage='bootstrap', started_at=None, finished_at=None, source={},
                environment={}, checks=[], artifacts={}, log_path=None, errors=[],
                validation_scope=[], requested_scope={}, test_summary=None, changes=None,
                ready_for_task=False, prepared=False)
    base.update(fields)
    return base


def safe_path(root, relative):
    """Reject every symlink component before accessing managed project paths."""
    root = Path(root).resolve()
    relative = Path(relative)
    if relative.is_absolute() or '..' in relative.parts:
        raise ValueError('Path must remain inside this checkout')
    path = root
    for part in relative.parts:
        path = path / part
        if path.is_symlink():
            raise ValueError(f'Symlink in managed path: {path}')
    return path


def atomic_json(path, data):
    path = Path(path)
    with tempfile.NamedTemporaryFile(mode='w', encoding='utf-8', dir=path.parent,
                                     prefix='.' + path.name + '.', delete=False) as stream:
        temp = Path(stream.name)
        try:
            json.dump(data, stream, ensure_ascii=False, indent=2)
            stream.write('\n'); stream.flush(); os.fsync(stream.fileno())
        except BaseException:
            temp.unlink(missing_ok=True)
            raise
    try:
        os.replace(temp, path)
        fd = os.open(path.parent, os.O_RDONLY)
        try: os.fsync(fd)
        finally: os.close(fd)
    finally:
        temp.unlink(missing_ok=True)


def read_json(path):
    return json.loads(Path(path).read_text())


def source_identity(root):
    """Digests only: no source bodies or inherited environment in records."""
    env = dict(os.environ, GIT_OPTIONAL_LOCKS='0')
    def git(*args):
        return subprocess.check_output(['git', '-C', str(root), *args], env=env,
                                       timeout=30)
    files = git('ls-files', '-z', '--cached', '--others', '--exclude-standard').split(b'\0')
    digests = {}
    for raw in files:
        if not raw: continue
        name = os.fsdecode(raw)
        if not name.startswith(('apps/', 'crates/', 'scripts/', 'packaging/', 'proto/')) and name not in ('Cargo.toml', 'Cargo.lock', '.cargo/config.toml'):
            continue
        path = Path(root) / name
        if path.is_symlink():
            digests[name] = hashlib.sha256(os.fsencode(os.readlink(path))).hexdigest()
        elif path.is_file():
            with path.open('rb') as stream:
                digests[name] = hashlib.file_digest(stream, 'sha256').hexdigest()
    serialized = json.dumps(digests, sort_keys=True).encode()
    return dict(head=git('rev-parse', 'HEAD').decode().strip(),
                dirty=bool(git('status', '--porcelain', '--untracked-files=normal')),
                inputs_sha256=hashlib.sha256(serialized).hexdigest(), inputs=digests)


class Run:
    def __init__(self, root, operation, scope):
        self.root = Path(root).resolve()
        self.id = new_id()
        self.directory = safe_path(root, f'dist/dev-package/runs/{self.id}')
        self.directory.mkdir(parents=True, exist_ok=False)
        self.data = result(operation, run_id=self.id, status='running', stage='bootstrap',
                           started_at=now(), requested_scope=scope,
                           log_path=str(self.directory / 'build.log'))
        (self.directory / 'build.log').touch(exist_ok=False)
        self.save()

    def save(self):
        atomic_json(self.directory / 'state.json', self.data)

    def event(self, kind, **fields):
        event = dict(schema_version=1, event=kind, run_id=self.id,
                     operation=self.data['operation'], time=now(), **fields)
        with (self.directory / 'events.jsonl').open('a') as log:
            log.write(json.dumps(event, ensure_ascii=False) + '\n')
            log.flush(); os.fsync(log.fileno())
        return event

    def finish(self, status, code, **fields):
        self.data.update(status=status, finished_at=now(), exit_code=code, **fields)
        self.save()
        atomic_json(self.directory / 'result.json', self.data)
        self.event('run_finished', status=status, exit_code=code)
        return self.data


def run_directory(root, run_id):
    if not RUN_ID.fullmatch(run_id):
        raise ValueError('Invalid run ID')
    return safe_path(root, f'dist/dev-package/runs/{run_id}')


def query(root, run_id):
    from .process import run_activity
    root = Path(root).resolve()
    directory = run_directory(root, run_id)
    if not directory.is_dir():
        raise FileNotFoundError(f'Unknown run ID: {run_id}')
    data = read_json(safe_path(root, directory.relative_to(root) / 'state.json'))
    if data.get('schema_version') != 1 or data.get('run_id') != run_id:
        raise ValueError('Unrecognized run record')
    activity = run_activity(root, directory, data)
    data['activity'] = activity
    if data['status'] == 'running':
        if activity == 'inactive': data['status'] = 'abandoned'
        elif activity == 'unknown': data['observed_status'] = 'unknown'
    data['result_readable'] = safe_path(root, directory.relative_to(root) / 'result.json').is_file()
    return data


def list_runs(root, operation=None, since=None, limit=20, current=False):
    directory = safe_path(root, 'dist/dev-package/runs')
    if not directory.exists(): return []
    rows = []
    for path in sorted(directory.iterdir(), reverse=True):
        if not RUN_ID.fullmatch(path.name): continue
        data = query(root, path.name)
        if operation and data['operation'] != operation: continue
        if since and dt.datetime.fromisoformat(data['started_at'].replace('Z', '+00:00')) < since: continue
        if current and data['activity'] not in ('active', 'unknown'): continue
        rows.append(data)
        if len(rows) >= limit: break
    return rows
