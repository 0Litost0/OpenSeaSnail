"""Ownership checks for project-local state. Unknown paths are never cleaned."""
import os
from pathlib import Path
import shutil
from .errors import Failure
from .state import atomic_json, read_json, safe_path

OWNER='seasnail-dev-package-v1'


def owned_directory(root,relative,run_id,kind, *, reuse=False):
    path=safe_path(root,relative)
    if path.exists():
        marker=path/'.owner.json'
        if not reuse or not marker.is_file() or marker.is_symlink():
            raise Failure(5,'directory_unknown','Refusing to reuse an unowned directory',path=path)
        try:
            data=read_json(marker)
            if not isinstance(data,dict): raise ValueError('Ownership must be an object')
        except (OSError,ValueError) as exc:
            raise Failure(5,'directory_unknown','Unreadable ownership evidence',path=path) from exc
        if data.get('owner')!=OWNER or data.get('path')!=str(path) or data.get('kind')!=kind:
            raise Failure(5,'directory_unknown','Ownership record does not match this directory',path=path)
        return path
    path.mkdir(parents=True,exist_ok=False)
    atomic_json(path/'.owner.json',dict(schema_version=1,owner=OWNER,path=str(path),run_id=run_id,kind=kind))
    return path


def remove_owned(root,path,kind):
    path=safe_path(root,Path(path).relative_to(Path(root).resolve()))
    marker=path/'.owner.json'
    if not marker.is_file() or marker.is_symlink(): raise Failure(5,'directory_unknown','Missing ownership evidence',path=path)
    try:
        data=read_json(marker)
        if not isinstance(data,dict): raise ValueError('Ownership must be an object')
    except (OSError,ValueError) as exc:
        raise Failure(5,'directory_unknown','Unreadable ownership evidence',path=path) from exc
    if data.get('owner')!=OWNER or data.get('path')!=str(path) or data.get('kind')!=kind:
        raise Failure(5,'directory_unknown','Invalid ownership evidence',path=path)
    # Reject symlink components before removal; never follow embedded links.
    shutil.rmtree(path)
