"""Bounded HTTPS downloads; verify before publication; no unowned cleanup."""
import os
from pathlib import Path
import subprocess
import time
import urllib.parse
import uuid

from .checks import verify_file
from .errors import Failure
from .recovery import owned_directory, remove_owned
from .state import safe_path

TRANSIENT_CURL={5,6,7,18,28,52,55,56,92}


def secure_url(value):
    parsed=urllib.parse.urlsplit(value)
    if parsed.scheme!='https' or not parsed.hostname or parsed.username or parsed.password:
        raise Failure(5,'download_url_invalid','Locked downloads require credential-free HTTPS URLs')


def download(root,cache,row,run_id, *, sleeper=time.sleep):
    cache=safe_path(root,Path(cache).relative_to(Path(root).resolve()))
    name=row['file_name']
    if Path(name).name!=name or name in ('.','..'):
        raise Failure(5,'download_name_invalid','Invalid locked filename')
    secure_url(row['url'])
    target=cache/name
    if target.exists() or target.is_symlink():
        try: verify_file(target,row)
        except (OSError,ValueError) as exc: raise Failure(5,'download_integrity',str(exc),path=target) from exc
        print('Verified existing '+name,flush=True)
        return target
    cache.mkdir(parents=True,exist_ok=True)
    # Each attempt owns only this unique leaf. Previously interrupted owned
    # leaves are preserved until a lock-protected recovery proves inactivity.
    for attempt in range(1,4):
        relative=cache.relative_to(Path(root).resolve())/'.dev-package-downloads'/f'{run_id}-{uuid.uuid4().hex}'
        workspace=owned_directory(root,relative,run_id,'download')
        temporary=workspace/'input'
        try:
            print(f'Downloading {name}: attempt {attempt}/3',flush=True)
            completed=subprocess.run(['curl','--fail','--silent','--show-error','--location',
                '--proto','=https','--proto-redir','=https','--connect-timeout','30','--max-time','3600',
                '--speed-time','120','--speed-limit','1024','--max-filesize',str(row['size_bytes']),
                '--output',str(temporary),'--write-out','%{http_code}','--url',row['url']],check=False,capture_output=False,stdout=subprocess.PIPE,text=True)
            if completed.returncode:
                code=completed.returncode
                http=(completed.stdout or '').strip()
                transient=code in TRANSIENT_CURL or (code==22 and http in {'429','500','502','503','504'})
                if transient and attempt<3:
                    print(f'Transient download failure ({code}); bounded retry',flush=True)
                    sleeper(attempt); continue
                raise Failure(4 if transient or code==22 else (8 if code==23 else (3 if code==60 else 5)),
                              'network_exhausted' if transient else 'download_failed',
                              'Locked download failed',path=target,child_code=code,
                              retry='safe' if transient else 'investigate')
            try: verify_file(temporary,row)
            except (ValueError,OSError) as exc: raise Failure(5,'download_integrity',str(exc),path=target) from exc
            try: os.link(temporary,target)
            except FileExistsError:
                try: verify_file(target,row)
                except (ValueError,OSError) as exc: raise Failure(5,'download_integrity',str(exc),path=target) from exc
            return target
        finally:
            remove_owned(root,workspace,'download')
