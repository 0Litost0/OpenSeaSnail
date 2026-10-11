"""Native deployment checks, immutable file inventories and reuse receipts."""
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
from .checks import output, verify_file, version_tuple
from .errors import Failure
from .state import atomic_json, safe_path


def sha(path):
    with Path(path).open('rb') as stream: return hashlib.file_digest(stream,'sha256').hexdigest()


def inventory(root):
    if Path(root).is_symlink() or not Path(root).is_dir():
        raise Failure(5,'native_directory_invalid','Artifact must be a real directory',path=root)
    rows=[]
    for path in sorted(Path(root).rglob('*')):
        relative=path.relative_to(root).as_posix()
        info=path.lstat(); mode=stat.S_IMODE(info.st_mode)
        if path.is_symlink():
            target=os.readlink(path)
            if Path(target).is_absolute() or not path.resolve().is_relative_to(Path(root).resolve()):
                raise Failure(5,'native_link_escape','Artifact symlink escapes its root',path=path)
            rows.append(dict(path=relative,kind='symlink',mode=mode,target=target))
        elif path.is_dir(): rows.append(dict(path=relative,kind='directory',mode=mode))
        elif path.is_file(): rows.append(dict(path=relative,kind='file',mode=mode,size_bytes=info.st_size,sha256=sha(path)))
        else: raise Failure(5,'native_file_unknown','Unexpected special file',path=path)
    return rows


def deployment(path,target='13.0', *, tools=None):
    tools=tools or {}
    try:
        architecture=output([tools.get('lipo','lipo'),'-archs',str(path)])
        if architecture.strip()!='arm64': raise ValueError('Native slices must be arm64 only')
        loads=output([tools.get('vtool','vtool'),'-show-build',str(path)])
        versions=re.findall(r'\bminos\s+([0-9.]+)',loads)
        if not versions or any(version_tuple(v)>version_tuple(target) for v in versions):
            raise ValueError('Mach-O deployment target is missing or exceeds the App requirement')
        libraries=output([tools.get('otool','otool'),'-L',str(path)])
        for line in libraries.splitlines()[1:]:
            name=line.strip().split(' (',1)[0]
            if name.startswith('/') and not name.startswith(('/usr/lib/','/System/Library/')):
                raise ValueError('Non-system absolute dylib dependency')
        return dict(path=str(path),architecture=architecture,minos=versions,loads=libraries)
    except (OSError,ValueError,subprocess.SubprocessError) as exc:
        raise Failure(5,'native_deployment',str(exc),path=path) from exc


def read_manifest(path, *, executable=False):
    try:
        value=json.loads(path.read_text())
        if not isinstance(value,dict) or not isinstance(value.get('files'),list):
            raise ValueError('Manifest must contain a files array')
        for row in value['files']:
            if not isinstance(row,dict) or not isinstance(row.get('path'),str) or not row['path']:
                raise ValueError('Manifest file must contain a nonempty path')
            if type(row.get('size_bytes')) is not int or row['size_bytes']<0:
                raise ValueError('Manifest file must contain a nonnegative size_bytes')
            if not isinstance(row.get('sha256'),str) or not re.fullmatch(r'[0-9a-f]{64}',row['sha256']):
                raise ValueError('Manifest file must contain a SHA-256 digest')
            if executable and type(row.get('executable')) is not bool:
                raise ValueError('Artifact file must declare executable mode')
        return value
    except (OSError,ValueError,TypeError,KeyError) as exc:
        raise Failure(5,'native_manifest_invalid',str(exc),path=path) from exc


def verify_sherpa(root,artifact, *, tools=None):
    expected=read_manifest(Path(root)/'scripts/sherpa/artifact-manifest.macos-arm64.json',executable=True)
    if read_manifest(artifact/'artifact-manifest.json',executable=True)!=expected:
        raise Failure(5,'sherpa_manifest_drift','Artifact manifest does not match the pinned declaration',path=artifact)
    allowed={'artifact-manifest.json','licenses/license-manifest.json'}
    results=[]
    for row in expected['files']:
        path=safe_path(root,artifact.relative_to(root)/row['path'])
        try: verify_file(path,row)
        except (ValueError,OSError) as exc: raise Failure(5,'sherpa_artifact_integrity',str(exc),path=path) from exc
        if bool(path.stat().st_mode & 0o111)!=row['executable']:
            raise Failure(5,'sherpa_mode_drift','Pinned executable mode does not match',path=path)
        allowed.add(row['path'])
        if row.get('architecture'): results.append(deployment(path,tools=tools))
    license_manifest=read_manifest(artifact/'licenses/license-manifest.json')
    for row in license_manifest['files']:
        relative=row['path'] if row['path'].startswith('licenses/') else 'licenses/'+row['path']
        path=safe_path(root,artifact.relative_to(root)/relative)
        try: verify_file(path,row)
        except (ValueError,OSError) as exc: raise Failure(5,'sherpa_license_integrity',str(exc),path=path) from exc
        allowed.add(relative)
    actual={p.relative_to(artifact).as_posix() for p in artifact.rglob('*') if p.is_file()}
    if actual!=allowed: raise Failure(5,'sherpa_unlisted_file','Artifact file coverage mismatch',path=artifact)
    # Trusted origin is provided by freshly executed prepare/verify-locks. Reuse
    # additionally requires the immutable inventory receipt created after that.
    return results


def receipt_identity(root,tools,kind):
    root=Path(root)
    declarations=['scripts/sherpa/native-toolchain.json','packaging/macos/Info.plist','apps/desktop/src-tauri/tauri.conf.json']
    if kind=='sherpa':
        declarations += ['scripts/sherpa/'+x for x in ['source-lock.json','artifact-lock.json','artifact-manifest.macos-arm64.json','fetch-inputs.py','build-probe-macos-arm64.sh','prepare-macos-arm64.sh','verify-locks.sh','reproducible-build.py','reproducible-rpath.cmake','sidecar/main.cc','sidecar/text_visibility.h']]
    else: declarations += ['scripts/dev_package/phases/build-ffmpeg-macos-arm64.sh','scripts/licenses/verify-ffmpeg-source.py']
    declarations += ['scripts/dev_package/'+x for x in ['native.py','pipeline.py','network.py','recovery.py','publication.py','temporary.py','errors.py','tools.py','checks.py','policy.json','phases/fetch_ffmpeg.py','phases/fetch_sherpa.py']]
    identity={'kind':kind,'tools':{name:{'path':path,'sha256':sha(path)} for name,path in tools.items()},'inputs':{name:sha(root/name) for name in declarations}}
    return identity,hashlib.sha256(json.dumps(identity,sort_keys=True).encode()).hexdigest()


def create_receipt(path,identity,artifact,validation):
    atomic_json(path,dict(schema_version=1,identity=identity,artifact=str(artifact),inventory=inventory(artifact),validation=validation,complete=True))


def reusable(path,identity,artifact):
    if not path.is_file() or path.is_symlink(): return False
    try:
        data=json.loads(path.read_text())
        if not isinstance(data,dict): raise ValueError('Receipt must be an object')
    except (ValueError,OSError) as exc:
        raise Failure(5,'native_receipt_invalid','Unreadable completion receipt',path=path) from exc
    if data.get('schema_version')!=1 or data.get('identity')!=identity or data.get('complete') is not True:
        raise Failure(5,'native_receipt_invalid','Completion receipt identity does not match',path=path)
    if data.get('artifact')!=str(artifact) or data.get('inventory')!=inventory(artifact):
        raise Failure(5,'native_receipt_integrity','Completed native artifact was changed',path=artifact)
    return True
