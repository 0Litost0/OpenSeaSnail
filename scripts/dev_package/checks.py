"""Read-only prerequisite and declared-input checks, shared with doctor."""
import hashlib
import json
import os
from pathlib import Path
import platform
import plistlib
import re
import shutil
import subprocess
import sys

from .state import error, safe_path
from .tools import installed_binary

class UnknownInput(ValueError):
    pass


ROOT = Path(__file__).resolve().parents[2]
POLICY = json.loads(Path(__file__).with_name('policy.json').read_text())


def version_tuple(value):
    match = re.search(r'(\d+)\.(\d+)(?:\.(\d+))?', value)
    if not match: raise ValueError(f'Unrecognized version: {value}')
    return tuple(int(x or 0) for x in match.groups())


def supported_node(version, root=ROOT):
    """Interpret the repository's explicitly supported engine branches."""
    engines = json.loads((root/'apps/desktop/package.json').read_text())['engines']['node']
    actual = version_tuple(version)
    for branch in engines.split('||'):
        branch = branch.strip()
        minimum = version_tuple(branch)
        if branch.startswith('^') and actual[0] == minimum[0] and actual >= minimum:
            return True
        if branch.startswith('>=') and actual >= minimum: return True
    return False


def tool_environment(overrides=None):
    env = dict(os.environ)
    if overrides: env.update(overrides)
    # Shim executables must diagnose missing tools, never fetch or auto-pin them.
    env.update(GIT_OPTIONAL_LOCKS='0', GIT_TERMINAL_PROMPT='0',
               RUSTUP_AUTO_INSTALL='0', COREPACK_ENABLE_NETWORK='0',
               COREPACK_ENABLE_DOWNLOAD_PROMPT='0', COREPACK_ENABLE_AUTO_PIN='0',
               COREPACK_DEFAULT_TO_LATEST='0', COREPACK_ENV_FILE='0')
    return env


def output(command, cwd=None):
    completed = subprocess.run(command, capture_output=True, text=True, timeout=30,
                               env=dict(tool_environment(), GIT_NO_LAZY_FETCH='1'), cwd=cwd)
    if completed.returncode: raise ValueError(f'Tool check returned {completed.returncode}')
    return completed.stdout.strip()


def verify_file(path, row):
    if path.is_symlink() or not path.is_file(): raise ValueError('Expected a regular locked file')
    if path.stat().st_size != row['size_bytes']: raise ValueError('Locked file size mismatch')
    with path.open('rb') as stream:
        if hashlib.file_digest(stream, 'sha256').hexdigest() != row['sha256']:
            raise ValueError('Locked file SHA-256 mismatch')


def nearest_existing(path):
    while not path.exists(): path = path.parent
    return path


def disk_check(root, required=None):
    required = POLICY['disk_initial_bytes'] if required is None else required
    path = safe_path(root, 'dist/dev-package')
    parent = nearest_existing(path)
    free = shutil.disk_usage(parent).free
    return {'name':'disk', 'actual':free, 'required':required, 'state':'ready' if free >= required else 'invalid',
            'verified':True, 'path':str(parent)}


def inspect(root=ROOT, *, frontend_only=False, inputs=True):
    root = Path(root).resolve()
    checks, errors, tools = [], [], {}
    def item(name, actual, required, valid, *, code='tool_unavailable', exit_code=3, path=None):
        row = dict(name=name, actual=actual, required=required, state='ready' if valid else 'invalid', verified=valid)
        if path: row['path'] = str(path)
        checks.append(row)
        if not valid:
            errors.append(dict(exit_code=exit_code, **error(code, 'check', f'{name} does not satisfy its requirement', actual=actual,
                         required=required, path=path, retry='after_user_fix', action='Select an existing supported tool/environment and retry; see doc/development.md')))
    item('python', platform.python_version(), POLICY['python_minimum'], sys.version_info >= (3,11))
    if not frontend_only:
        item('platform', f'{platform.system()}/{platform.machine()}', 'Darwin/arm64', platform.system()=='Darwin' and platform.machine()=='arm64')
    developer=None
    if not frontend_only and platform.system()=='Darwin':
        try:
            developer=output(['xcode-select','-p'])
            if not Path(developer).is_dir(): developer=None
        except (OSError,ValueError,subprocess.SubprocessError): pass
        item('apple_tool_selection',developer,'existing selected developer directory',developer is not None)
    command_args = {'git':['--version'], 'node':['--version'], 'pnpm':['--version']}
    if not frontend_only:
        command_args.update({'cargo':['--version'],'rustc':['--version'],'swift':['--version'],'cmake':['--version'],
            'cc':['--version'],'c++':['--version'],'xcrun':['--show-sdk-version'], 'xcode-select':['-p'],
            'curl':['--version'],'jq':['--version'],'tar':['--version'],'ditto':['-h'],'shasum':['--version'],
            'otool':['-h'],'vtool':['-h'],'lipo':['-h'],'make':['--version']})
    for name, args in command_args.items():
        if name in ('cc','c++','swift','xcrun') and not developer:
            item(name,None,'existing selected Apple developer directory',False); continue
        executable = shutil.which(name)
        if not executable:
            item(name, None, 'installed executable', False); continue
        try:
            executable=installed_binary(executable,name,root)
        except (OSError,ValueError,KeyError,TypeError) as exc:
            item(name,str(exc),'already installed supported executable',False); continue
        tools[name] = executable
        # Apple inspection tools' help may use nonzero exit status; presence alone
        # is explicitly recorded as an executable check, not a capability proof.
        if name in ('ditto','otool','vtool','lipo'):
            item(name, executable, 'installed executable', os.access(executable, os.X_OK)); continue
        try:
            text = output([executable, *args], cwd=root/'apps/desktop' if name=='pnpm' else root); first = text.splitlines()[0]
            valid, required = True, 'installed executable'
            if name=='node': valid,required = supported_node(first,root), json.loads((root/'apps/desktop/package.json').read_text())['engines']['node']
            if name=='pnpm':
                package=json.loads((root/'apps/desktop/package.json').read_text())
                required=package['packageManager']; valid=first==required.split('@',1)[1]
            if name=='rustc': required=POLICY['rust_minimum_verified']; valid=version_tuple(first)>=version_tuple(required)
            if name=='swift':
                declared=(root/'apps/desktop/native/post-paste-monitor/Package.swift').read_text()
                required=re.search(r'swift-tools-version:\s*([0-9.]+)',declared)[1]
                valid=version_tuple(first)>=version_tuple(required)
            item(name,first,required,valid,path=executable)
        except (ValueError, IndexError, OSError, subprocess.SubprocessError) as exc:
            item(name,type(exc).__name__,'successful supported tool check',False,path=executable)
    if not frontend_only:
        try:
            native=json.loads((root/'scripts/sherpa/native-toolchain.json').read_text())
            native_commands=[('clang',['cc','--version'],native['clang']),('clangxx',['c++','--version'],native['clang']),
                  ('sdk_version',['xcrun','--show-sdk-version'],native['sdk_version']),('sdk_build',['xcrun','--show-sdk-build-version'],native['sdk_build']),
                  ('cmake_identity',['cmake','--version'], 'cmake version '+native['cmake'])]
        except (OSError,ValueError,KeyError,TypeError) as exc:
            native_commands=[]
            item('native_declaration',type(exc).__name__,'valid native-toolchain.json',False,exit_code=5)
        for name, command, required in native_commands:
            try:
                if command[0] in ('cc','c++','xcrun') and not developer: raise ValueError('No Apple tools selected')
                actual=output([tools.get(command[0],command[0]),*command[1:]]).splitlines()[0]
            except (OSError,ValueError,subprocess.SubprocessError,IndexError): actual=None
            item(name,actual,required,actual==required)
        for variable in ('CC','CXX','CFLAGS','CXXFLAGS','LDFLAGS'):
            item(f'environment.{variable}', 'set' if os.environ.get(variable) else 'unset', 'unset', not os.environ.get(variable))
        try:
            host=platform.mac_ver()[0]
            if not developer: raise ValueError('No Apple tools selected')
            if developer.endswith('/CommandLineTools'):
                item('host_support',host,POLICY['clt_verified_hosts'],host in POLICY['clt_verified_hosts'])
            else:
                policy=POLICY['xcode_vendor_host_range']
                item('host_support',host,policy,version_tuple(host)>=version_tuple(policy['minimum']) and version_tuple(host)[0]<=policy['maximum_major'])
            rust_binary=tools.get('rustc')
            if not rust_binary: raise ValueError('Rust compiler not installed')
            rust_host=output([rust_binary,'-vV'])
            item('rust_host',re.search(r'^host: (.+)$',rust_host,re.M)[1], 'aarch64-apple-darwin', 'host: aarch64-apple-darwin' in rust_host)
            plist=plistlib.loads((root/'packaging/macos/Info.plist').read_bytes())
            tauri=json.loads((root/'apps/desktop/src-tauri/tauri.conf.json').read_text())
            expected=plist['LSMinimumSystemVersion']
            item('deployment_target',tauri['bundle']['macOS']['minimumSystemVersion'],expected,tauri['bundle']['macOS']['minimumSystemVersion']==expected,exit_code=5)
        except (OSError,ValueError,KeyError,TypeError,subprocess.SubprocessError) as exc:
            item('support_declarations',type(exc).__name__,'readable, consistent declarations',False,exit_code=5)
        for relative in ('build/dev-package','dist/dev-package','target','apps/desktop','third_party/sherpa/macos-arm64','third_party/ffmpeg/macos-arm64'):
            try:
                path=safe_path(root,relative); parent=nearest_existing(path)
                item('writable.'+relative,str(parent),'writable/searchable destination',parent.is_dir() and os.access(parent,os.W_OK|os.X_OK),exit_code=8,path=parent)
            except ValueError as exc: item('writable.'+relative,str(exc),'no symlink traversal',False,exit_code=8)
        try:
            row=disk_check(root); checks.append(row)
            if row['state']!='ready':
                errors.append(dict(exit_code=8,**error('disk_insufficient','check','Insufficient reservation space',actual=row['actual'],required=row['required'],path=row['path'],retry='after_user_fix',action='Provide sufficient free space; do not delete unknown files')))
        except (ValueError,OSError) as exc: item('disk',type(exc).__name__,'accessible destination volume',False,exit_code=8)
    if inputs: inspect_inputs(root, checks, errors, frontend_only)
    return dict(checks=checks, errors=errors, tools=tools, ready_for_task=not errors, prepared=False)


def inspect_inputs(root, checks, errors, frontend_only=False):
    def row(name, path, verifier=None, directory=False):
        try:
            path=safe_path(root,path)
            if not path.exists(): state,verified='missing_preparable',False
            elif directory and not any(p.name!='.gitkeep' for p in path.iterdir()): state,verified='missing_preparable',False
            elif verifier:
                verifier(path); state,verified='ready',True
            else: state,verified='unknown',False
            checks.append(dict(name=name,path=str(path),state=state,verified=verified))
            if state=='unknown':
                errors.append(dict(exit_code=5,**error('input_unknown','check','Existing input has no verifiable identity',path=path)))
        except (ValueError,OSError,KeyError,TypeError,subprocess.SubprocessError) as exc:
            checks.append(dict(name=name,path=str(path),state='unknown' if isinstance(exc,UnknownInput) else 'invalid',verified=False))
            errors.append(dict(exit_code=5,**error('input_unknown' if isinstance(exc,UnknownInput) else 'input_invalid','check',str(exc),path=path)))
    declarations=['apps/desktop/package.json','apps/desktop/pnpm-lock.yaml']
    if not frontend_only:
        declarations+=['Cargo.toml','Cargo.lock','apps/desktop/native/post-paste-monitor/Package.resolved', 'scripts/sherpa/source-lock.json','scripts/sherpa/artifact-lock.json','scripts/sherpa/artifact-manifest.macos-arm64.json']
    for name in declarations:
        path=root/name
        if not path.is_file() or path.is_symlink():
            errors.append(dict(exit_code=5,**error('declaration_missing','check','Required committed declaration missing',path=path)))
    if errors and any(x['code']=='declaration_missing' for x in errors): return
    # Node's dependency tree requires an install receipt, not directory existence.
    def frontend(path):
        marker=path/'.modules.yaml'
        if not marker.is_file() or marker.is_symlink(): raise UnknownInput('Unrecognized node_modules tree; inspect before preparation')
        checks.append(dict(name='frontend.lock_validation',state='missing_preparable',verified=False,reason='Frozen install must validate current lock before use'))
    row('frontend.dependencies','apps/desktop/node_modules',frontend,directory=True)
    if frontend_only: return
    try:
        source=json.loads((root/'scripts/sherpa/source-lock.json').read_text())
        artifact=json.loads((root/'scripts/sherpa/artifact-lock.json').read_text())
        expected=json.loads((root/'scripts/sherpa/artifact-manifest.macos-arm64.json').read_text())
        # Validate the shape needed by this reader; no mutation or fallback baseline.
        downloads=source['source_archives']+source['onnxruntime_cmake_dependencies']+artifact['downloads']
        for entry in downloads+expected['files']:
            if not isinstance(entry['size_bytes'],int) or entry['size_bytes']<0: raise ValueError('Invalid locked size')
            if not re.fullmatch('[0-9a-f]{64}',entry['sha256']): raise ValueError('Invalid locked SHA-256')
        for key in ('sherpa_onnx','onnxruntime'):
            if not re.fullmatch('[0-9a-f]{40}',source[key]['revision']): raise ValueError('Invalid source revision')
    except (OSError,ValueError,KeyError,TypeError) as exc:
        checks.append(dict(name='sherpa.declarations',state='invalid',verified=False))
        errors.append(dict(exit_code=5,**error('input_invalid','check','Invalid Sherpa declaration: '+str(exc),path=root/'scripts/sherpa')))
        return
    for entry in downloads:
        row('archive.'+entry['file_name'],'third_party/sherpa/macos-arm64/cache/'+entry['file_name'],lambda p,e=entry:verify_file(p,e))
    for name,key in [('sherpa-onnx','sherpa_onnx'),('onnxruntime','onnxruntime')]:
        def checkout(path, entry=source[key]):
            if not (path/'.git').exists(): raise UnknownInput('Unrecognized source checkout')
            if output(['git','-C',str(path),'rev-parse','HEAD'])!=entry['revision']: raise ValueError('Source revision drift')
            if output(['git','-C',str(path),'status','--porcelain','--untracked-files=normal']): raise ValueError('Dirty locked source checkout')
            for sub in entry.get('submodules',[]):
                child=safe_path(root,path.relative_to(root)/sub['path'])
                if output(['git','-C',str(child),'rev-parse','HEAD'])!=sub['revision']: raise ValueError('Submodule revision drift')
        row('source.'+name,'third_party/sherpa/macos-arm64/sources/'+name,checkout,directory=True)
    def native(path):
        if json.loads((path/'artifact-manifest.json').read_text())!=expected: raise ValueError('Artifact manifest drift')
        for entry in expected['files']: verify_file(safe_path(root,path.relative_to(root)/entry['path']),entry)
        license_manifest=safe_path(root,path.relative_to(root)/'licenses/license-manifest.json')
        license_rows=json.loads(license_manifest.read_text())['files']
        covered=set()
        for entry in license_rows:
            relative='licenses/'+entry['path'] if not entry['path'].startswith('licenses/') else entry['path']
            verify_file(safe_path(root,path.relative_to(root)/relative),entry); covered.add(relative)
        actual={str(p.relative_to(path)) for p in (path/'licenses').rglob('*') if p.is_file() and p!=license_manifest}
        if covered!=actual: raise ValueError('License manifest coverage drift')
        allowed={entry['path'] for entry in expected['files']} | covered | {'artifact-manifest.json','licenses/license-manifest.json'}
        for child in path.rglob('*'):
            if child.is_symlink(): raise UnknownInput('Unrecognized symlink in artifact')
            if child.is_file() and str(child.relative_to(path)) not in allowed:
                raise UnknownInput('Unrecognized extra artifact file: '+str(child.relative_to(path)))
        checks.append(dict(name='sherpa.fixed_files',state='ready',verified=True,validation_scope=['pinned_file_sizes_and_hashes','self_manifest_consistency_only']))
    row('sherpa.artifact','third_party/sherpa/macos-arm64/artifact',native,directory=True)
    for item in checks:
        if item['name']=='sherpa.artifact' and item['state']=='ready':
            item.update(state='missing_preparable',verified=False,reason='Pinned files checked; trusted license provenance and managed completion receipt still require M2 validation')
    # FFmpeg deployment correction requires a new reviewed recipe/receipt in M2.
    try:
        ff=safe_path(root,'third_party/ffmpeg/macos-arm64/bundle')
    except ValueError as exc:
        errors.append(dict(exit_code=5,**error('ffmpeg_invalid','check',str(exc))))
        checks.append(dict(name='ffmpeg.bundle',state='invalid',verified=False))
        return
    if ff.exists() and any(p.name!='.gitkeep' for p in ff.iterdir()):
        try:
            output([sys.executable,str(root/'scripts/licenses/verify-ffmpeg-source.py'),str(ff)])
            checks.append(dict(name='ffmpeg.bundle',path=str(ff),state='missing_preparable',verified=False,reason='Current recipe requires deployment-13 rebuild and managed completion receipt'))
        except (OSError,ValueError,subprocess.SubprocessError) as exc:
            checks.append(dict(name='ffmpeg.bundle',path=str(ff),state='invalid',verified=False))
            errors.append(dict(exit_code=5,**error('ffmpeg_invalid','check',str(exc),path=ff)))
    else:
        checks.append(dict(name='ffmpeg.bundle',path=str(ff),state='missing_preparable',verified=False))
    for name in ('cargo.dependencies','swift.dependencies'):
        checks.append(dict(name=name,state='missing_preparable',verified=False,reason='Locked preparation must resolve dependencies; standard caches are not scanned'))
