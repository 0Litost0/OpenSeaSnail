"""Stage supervisor. Holds inherited flock even if a tool closes all its FDs."""
import os
from pathlib import Path
import signal
import subprocess
import sys
from .process import process_identity
from .state import atomic_json, now, read_json


def main():
    request = read_json(sys.argv[1])
    # Validate that the inherited descriptor is still open; never reacquire it.
    os.fstat(request['lock_fd'])
    marker = Path(request['marker'])
    record = dict(schema_version=1, pid=os.getpid(), pgid=os.getpgrp(),
                  identity=process_identity(os.getpid()), started_at=now(), state='running')
    atomic_json(marker, record)
    interrupted = []
    def handle(number, frame): interrupted.append(number)
    signal.signal(signal.SIGINT, handle); signal.signal(signal.SIGTERM, handle)
    child = subprocess.Popen(request['command'], cwd=request['cwd'])
    while True:
        try:
            code = child.wait(timeout=.1)
            break
        except subprocess.TimeoutExpired:
            if interrupted:
                # Group signal already reached the child. Remain alive holding the lock.
                continue
    record.update(state='finished', finished_at=now(), subprocess_exit_code=code)
    atomic_json(marker, record)
    return code if code >= 0 else 128-code


if __name__ == '__main__':
    sys.exit(main())
