"""Native publication intent, crash recovery and owned staging cleanup.

Callers must hold RepositoryLock, which also excludes surviving stage groups.
"""
import os
from pathlib import Path
import shutil

from . import native
from .errors import Failure
from .recovery import OWNER
from .state import atomic_json, read_json, safe_path


def checked(root,path):
    try: return safe_path(root,Path(path).relative_to(Path(root).resolve()))
    except ValueError as exc:
        raise Failure(5,'native_publication_unknown',str(exc),path=path) from exc


def record(path):
    try:
        data=read_json(path)
        if not isinstance(data,dict): raise ValueError('Record must be an object')
        return data
    except (OSError,ValueError) as exc:
        raise Failure(5,'native_publication_unknown','Unreadable publication record',path=path) from exc


def sync_directory(path):
    descriptor=os.open(path,os.O_RDONLY)
    try: os.fsync(descriptor)
    finally: os.close(descriptor)


def begin(root,base,run_id):
    stage=checked(root,base/f'.artifact-{run_id}')
    intent=checked(root,base/f'staging-{run_id}.json')
    if stage.exists() or intent.exists():
        raise Failure(5,'native_staging_unknown','Staging already exists',path=stage)
    atomic_json(intent,dict(schema_version=1,owner=OWNER,run_id=run_id,path=str(stage),kind='native-staging'))
    return stage


def recover(root,base,identity):
    artifact=checked(root,base/'artifact'); receipt=checked(root,base/'receipt.json')
    pending=checked(root,base/'.pending-receipt.json')
    if pending.exists():
        data=record(pending)
        if data.get('schema_version')!=1 or data.get('identity')!=identity or data.get('artifact')!=str(artifact) or data.get('complete') is not True:
            raise Failure(5,'native_publication_unknown','Pending publication identity is invalid',path=pending)
        if artifact.exists():
            native.reusable(pending,identity,artifact)
            if receipt.exists():
                native.reusable(receipt,identity,artifact)
                if record(receipt)!=data:
                    raise Failure(5,'native_publication_conflict','Existing receipt conflicts with pending publication',path=receipt)
                pending.unlink()
            else:
                os.replace(pending,receipt)
            sync_directory(base)
        else:
            # Unpublished stages are rebuilt; recovery never upgrades them.
            pending.unlink()
    known=set()
    for intent in sorted(base.glob('staging-*.json')):
        checked(root,intent)
        data=record(intent)
        run_id=data.get('run_id')
        if not isinstance(run_id,str) or intent.name!=f'staging-{run_id}.json':
            raise Failure(5,'native_staging_unknown','Invalid staging record',path=intent)
        stage=checked(root,base/f'.artifact-{run_id}')
        if data.get('schema_version')!=1 or data.get('owner')!=OWNER or data.get('path')!=str(stage) or data.get('kind')!='native-staging':
            raise Failure(5,'native_staging_unknown','Staging ownership does not match',path=intent)
        known.add(stage)
        if stage.exists():
            if not stage.is_dir(): raise Failure(5,'native_staging_unknown','Staging is not a directory',path=stage)
            shutil.rmtree(stage)
        intent.unlink()
    for stage in base.glob('.artifact-*'):
        if stage not in known:
            raise Failure(5,'native_staging_unknown','Unrecorded staging is preserved',path=stage)


def publish(root,base,stage,identity,validation):
    artifact=checked(root,base/'artifact'); pending=checked(root,base/'.pending-receipt.json')
    receipt=checked(root,base/'receipt.json'); checked(root,stage)
    if artifact.exists() or receipt.exists() or pending.exists():
        raise Failure(5,'native_publication_conflict','Final output already exists',path=artifact)
    atomic_json(pending,dict(schema_version=1,identity=identity,artifact=str(artifact),
        inventory=native.inventory(stage),validation=validation,complete=True))
    stage.rename(artifact)
    sync_directory(base)
    os.replace(pending,receipt)
    sync_directory(base)
    # Intent removal is replayable after a crash following final receipt write.
    recover(root,base,identity)
