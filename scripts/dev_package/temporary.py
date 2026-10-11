"""Lock-protected recovery of explicitly owned temporary leaves."""
from pathlib import Path
from .errors import Failure
from .recovery import owned_directory,remove_owned
from .state import safe_path


def recover_bucket(root,relative,kind):
    parent=safe_path(root,relative)
    if not parent.exists(): return []
    if not parent.is_dir(): raise Failure(5,'temporary_unknown','Temporary bucket is not a directory',path=parent)
    removed=[]
    for leaf in sorted(parent.iterdir()):
        owned_directory(root,leaf.relative_to(Path(root).resolve()),'recovery',kind,reuse=True)
        remove_owned(root,leaf,kind)
        removed.append(str(leaf))
    return removed
