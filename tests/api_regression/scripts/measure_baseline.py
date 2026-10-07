#!/usr/bin/env python3
"""M1 observation via existing PoC host. Not the formal runner or acceptance gate."""
from pathlib import Path
import argparse
import hashlib
import json
import os
import platform
import secrets
import selectors
import shutil
import signal
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import uuid
from quality import score, validate_rules

ROOT = Path(__file__).resolve().parents[1]
REPO = ROOT.parents[1]
DOC = ROOT / 'artifacts/baseline'


def identity(path):
    return {'sha256':hashlib.sha256(path.read_bytes()).hexdigest(), 'bytes':path.stat().st_size}


def ps_identity(pid):
    result = subprocess.run(['ps', '-p', str(pid), '-o', 'lstart=,command='], capture_output=True, text=True, timeout=3)
    if result.returncode != 0 and result.stderr.strip():
        raise RuntimeError('process identity query failed')
    return result.stdout.strip() if result.returncode == 0 else None


def group_members(group_id):
    """The host starts a new session; inherited PGID survives child reparenting."""
    result = subprocess.run(['ps', '-axo', 'pid=,pgid=,comm='], capture_output=True, text=True, timeout=3, check=True)
    found = []
    for line in result.stdout.splitlines():
        parts = line.strip().split(None, 2)
        if len(parts) == 3 and int(parts[1]) == group_id and int(parts[0]) != group_id:
            found.append({'pid':int(parts[0]), 'identity':ps_identity(int(parts[0])),
                          'role':'sherpa' if parts[2].endswith('/seasnail-sherpa-sidecar') else 'auxiliary'})
    return found


def cleanup_owned(child, owned, home, journal, observe):
    """Fail closed if startup/process identity is unknown; never delete a live home."""
    errors = []
    started = time.monotonic()
    deadline = started + 15
    try:
        # Capture descendants before stopping a host, not after losing PPID links.
        if child and child.poll() is None:
            observe()
    except Exception:
        errors.append('process_observation_failed')
    if child:
        if child.poll() is None:
            try:
                child.stdin.close()
                child.wait(timeout=min(10, max(0.01, deadline-time.monotonic())))
            except (subprocess.TimeoutExpired, OSError):
                child.kill()
                try:
                    child.wait(timeout=min(3, max(0.01, deadline-time.monotonic())))
                except subprocess.TimeoutExpired:
                    errors.append('host_still_alive')
                errors.append('host_required_forced_stop')
        if child.poll() not in (None, 0):
            errors.append('abnormal_host_exit')
        if child.poll() is None:
            errors.append('host_exit_unconfirmed')
    remaining = []
    try:
        # PGID observation still finds descendants reparented after an early host exit.
        remaining = group_members(child.pid) if child else []
        known = {(process['pid'], process['identity']) for process in owned if process['identity']}
        if any((process['pid'], process['identity']) not in known for process in remaining):
            errors.append('unregistered_group_member_identity_unknown')
        for process in owned:
            current = ps_identity(process['pid'])
            if current is not None and current != process['identity']:
                errors.append('owned_process_identity_changed')
                continue
            if current and process['identity']:
                wait_until = min(deadline, time.monotonic()+3)
                while ps_identity(process['pid']) == process['identity'] and time.monotonic() < wait_until:
                    time.sleep(0.05)
                if ps_identity(process['pid']) == process['identity']:
                    errors.append('owned_process_required_forced_stop')
                    os.kill(process['pid'], signal.SIGTERM)
                    wait_until = min(deadline, time.monotonic()+1)
                    while ps_identity(process['pid']) == process['identity'] and time.monotonic() < wait_until:
                        time.sleep(0.05)
                    if ps_identity(process['pid']) == process['identity']:
                        os.kill(process['pid'], signal.SIGKILL)
                        while ps_identity(process['pid']) == process['identity'] and time.monotonic() < deadline:
                            time.sleep(0.05)
        remaining = group_members(child.pid) if child else []
        identities_gone = all(ps_identity(process['pid']) is None for process in owned)
        can_remove = (child is None or child.poll() is not None) and not remaining and identities_gone
        can_remove &= not any(error in errors for error in ('process_observation_failed', 'unregistered_group_member_identity_unknown', 'owned_process_identity_changed'))
    except Exception:
        errors.append('process_exit_verification_failed')
        can_remove = False
    if can_remove:
        try:
            shutil.rmtree(home)
            journal.unlink(missing_ok=True)
        except OSError:
            errors.append('directory_cleanup_failed')
    else:
        errors.append('owned_directory_preserved_for_recovery')
    if home.exists():
        # Persist known/unknown resources, even when capture failed before ready.
        journal.write_text(json.dumps({'home':str(home),'host_pid':child.pid if child else None,
                                       'known_processes':owned,'remaining_group_members':remaining,'cleanup_errors':errors})+'\n')
        journal.chmod(0o600)
    return {'teardown_ms':round((time.monotonic()-started)*1000,3),
            'teardown_status':'failed' if errors else 'passed', 'cleanup_errors':errors, 'home_removed':not home.exists()}


def request(base, method, route, token=None, data=None, body=None, content_type=None, expected=200, timings=None):
    headers = {}
    if token:
        headers['Authorization'] = 'Bearer ' + token
    if data is not None:
        body = json.dumps(data).encode(); content_type = 'application/json'
    if content_type:
        headers['Content-Type'] = content_type
    started = time.monotonic()
    try:
        with urllib.request.urlopen(urllib.request.Request(base + route, data=body, headers=headers, method=method), timeout=5) as response:
            if response.status != expected:
                raise RuntimeError('unexpected API status')
            raw = response.read(1024 * 1024)
    except urllib.error.HTTPError as error:
        raise RuntimeError(f'API request failed with status {error.code}') from None
    finally:
        if timings is not None:
            timings.append(round((time.monotonic() - started) * 1000, 3))
    return json.loads(raw) if raw else None


def ready(child, deadline, observe):
    selector = selectors.DefaultSelector(); selector.register(child.stdout, selectors.EVENT_READ)
    buf = b''
    next_observation = 0
    try:
        while time.monotonic() < deadline:
            if time.monotonic() >= next_observation and child.poll() is None:
                observe()
                next_observation = time.monotonic() + 0.5
            if child.poll() is not None:
                raise RuntimeError('PoC host exited before readiness')
            for key, _ in selector.select(timeout=0.1):
                buf += os.read(key.fd, 4096)
                if len(buf) > 8192:
                    raise RuntimeError('oversized ready record')
                if b'\n' in buf:
                    value = json.loads(buf.split(b'\n', 1)[0])
                    if value['pid'] != child.pid or value['runtime'] != 'sherpa':
                        raise RuntimeError('unexpected host/runtime identity')
                    return value
        raise RuntimeError('PoC host/model readiness timed out')
    finally:
        selector.close()


def run_sample(sample, index, args, run_id):
    # All host temporary audio files are rooted under this owned directory.
    home = Path(tempfile.mkdtemp(prefix='seasnail-m1-baseline-'))
    home.chmod(0o700); (home / 'tmp').mkdir(mode=0o700)
    raw_dir = ROOT / 'artifacts/baseline' / run_id
    raw_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
    raw_dir.chmod(0o700)
    stderr_path = raw_dir / f"{sample['id']}-{index}.stderr.log"
    log = open(stderr_path, 'wb'); os.chmod(stderr_path, 0o600)
    env = {key:value for key, value in os.environ.items() if key in ('PATH', 'LANG', 'LC_ALL', 'SYSTEMROOT')}
    env.update({'POC_DATA_DIR':str(home), 'POC_RUNTIME':'sherpa', 'SEASNAIL_ASR_ROOT':str(args.asr_root),
                'FFMPEG_PATH':str(args.ffmpeg), 'TMPDIR':str(home / 'tmp')})
    child = None; owned = []; error = None
    host_identity = None
    api_ms = []; record = {'sample_id':sample['id'], 'repetition':index, 'execution':'environment_error'}
    journal = raw_dir / f"{sample['id']}-{index}.recovery.json"
    started = time.monotonic()
    def observe():
        if child.poll() is not None or not host_identity or ps_identity(child.pid) != host_identity:
            raise RuntimeError('host identity cannot be verified for descendant registration')
        for process in group_members(child.pid):
            if not process['identity']:
                raise RuntimeError('descendant identity unavailable')
            if (process['pid'], process['identity']) not in {(item['pid'],item['identity']) for item in owned}:
                owned.append(process)
        journal.write_text(json.dumps({'home':str(home),'host_pid':child.pid,'host_identity':host_identity,'known_processes':owned})+'\n')
        journal.chmod(0o600)
    try:
        child = subprocess.Popen([str(args.host)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log, env=env, start_new_session=True)
        journal.write_text(json.dumps({'home':str(home),'host_pid':child.pid,'host_identity':None,'known_processes':[]})+'\n')
        journal.chmod(0o600)
        host_identity = ps_identity(child.pid)
        os.chmod(journal, 0o600)
        info = ready(child, time.monotonic() + 120, observe)
        base = f"http://127.0.0.1:{info['port']}/api/v1"
        record['poc_host_and_model_ready_ms'] = round((time.monotonic() - started) * 1000, 3)
        observe()
        sidecars = [process for process in owned if process['role'] == 'sherpa']
        if len(sidecars) != 1 or not sidecars[0]['identity']:
            raise RuntimeError('expected one identifiable owned Sherpa sidecar')
        journal.write_text(json.dumps({'home':str(home),'host_pid':child.pid,'host_identity':ps_identity(child.pid),'sidecars':owned})+'\n')
        request(base[:-7], 'GET', '/', timings=api_ms)
        setup = request(base, 'POST', '/auth/setup', data={'username':'baseline','password':secrets.token_urlsafe(24)}, expected=201, timings=api_ms)
        token = setup['secret']
        models = request(base, 'GET', '/models', token=token, timings=api_ms)
        if not any(model['id'] == 'sensevoice-small-sherpa-int8' and model['status'] == 'active' and model['runtime'] == 'sherpa_onnx' for model in models):
            raise RuntimeError('real Sherpa is not active')
        wav = (ROOT / sample['audio_path']).read_bytes()
        if hashlib.sha256(wav).hexdigest() != sample['audio_sha256']:
            raise RuntimeError('audio identity mismatch')
        boundary = 'seasnail-' + uuid.uuid4().hex
        body = bytearray()
        for name, value in [('source','imported'), ('language',sample['language'])]:
            body.extend(f'--{boundary}\r\nContent-Disposition: form-data; name="{name}"\r\n\r\n{value}\r\n'.encode())
        body.extend(f'--{boundary}\r\nContent-Disposition: form-data; name="audio"; filename="speech.wav"\r\nContent-Type: audio/wav\r\n\r\n'.encode())
        body.extend(wav); body.extend(f'\r\n--{boundary}--\r\n'.encode())
        business_started = time.monotonic()
        submitted = request(base, 'POST', '/sessions', token=token, body=bytes(body), content_type='multipart/form-data; boundary='+boundary, expected=202, timings=api_ms)
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            session = request(base, 'GET', '/sessions/'+submitted['id'], token=token, timings=api_ms)
            if session['status'] in ('completed','failed'):
                break
            time.sleep(0.1)
        else:
            raise RuntimeError('transcription deadline exceeded')
        if session['status'] != 'completed':
            raise RuntimeError('real transcription did not complete')
        actual = session['transcript']['full_text']
        reference = json.loads((ROOT / sample['reference_path']).read_text())
        rules = json.loads((ROOT / 'assets/quality-rules.v3.json').read_text())
        validate_rules(rules)
        record.update({'execution':'completed','actual':actual,'score':score(reference, actual, rules['samples'][sample['id']]),
                       'business_ms':round((time.monotonic()-business_started)*1000,3), 'api_request_max_ms':max(api_ms)})
    except Exception as exc:
        error = str(exc) if isinstance(exc, RuntimeError) else type(exc).__name__
        record['error'] = error
    finally:
        try:
            record.update(cleanup_owned(child, owned, home, journal, observe))
        finally:
            log.close()

    return record


def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('--host', type=Path, default=REPO/'target/debug/seasnail-poc-host')
    parser.add_argument('--asr-root', type=Path, default=REPO/'dist/SeaSnail.app/Contents/Resources/asr')
    parser.add_argument('--ffmpeg', type=Path, default=REPO/'dist/SeaSnail.app/Contents/Resources/ffmpeg')
    args=parser.parse_args()
    args.host=args.host.resolve();args.asr_root=args.asr_root.resolve();args.ffmpeg=args.ffmpeg.resolve()
    if platform.system() != 'Darwin':
        parser.error('M1 observation uses macOS PoC process adapter; not Windows acceptance')
    manifest=json.loads((ROOT/'assets/manifest.json').read_text())
    artifact=args.asr_root/'sensevoice-small/sherpa_onnx/int8/artifact-manifest.json'
    bundle=json.loads(artifact.read_text())
    # Hash the complete declared bundle tree, not just the human-readable version.
    bundle_identity=[]
    for path in sorted(artifact.parent.rglob('*')):
        if path.is_file():
            bundle_identity.append({'path':str(path.relative_to(artifact.parent)), **identity(path)})
    run_id='m1-'+uuid.uuid4().hex
    result={'schema_version':1,'run_id':run_id,'scope':'M1 observation through existing PoC host; not formal acceptance',
            'platform':{'os':platform.system(),'arch':platform.machine(),'release':platform.release()},
            'host':identity(args.host),'ffmpeg':identity(args.ffmpeg),'artifact_manifest':identity(artifact),
            'bundle_files':bundle_identity,'dataset':identity(ROOT/'assets/manifest.json'),'rules':identity(ROOT/'assets/quality-rules.v3.json'),
            'scorer':identity(Path(__file__).with_name('quality.py')),'source':{'commit':subprocess.check_output(['git','rev-parse','HEAD'],cwd=REPO,text=True).strip(),
            'dirty':bool(subprocess.check_output(['git','status','--porcelain'],cwd=REPO,text=True))},
            'rust_lock':identity(ROOT/'poc/Cargo.lock'),'observations':[],
            'quality_gate':'incomplete','approval_required':True,'phase_limitations':['PoC model activation occurs before HTTP bind; startup and model ready cannot be measured separately.', 'No formal reporter check timing; M2/M3 must validate those independent budgets.']}
    output=DOC/(run_id+'-observations.json');DOC.mkdir(parents=True, exist_ok=True)  # Never overwrite the approved acquisition evidence.
    for sample in manifest['samples']:
        for index in range(1,4):
            record=run_sample(sample,index,args,run_id)
            result['observations'].append(record)
            encoded=json.dumps(result,ensure_ascii=False,indent=2)+'\n'
            output.write_text(encoded)
            archive=ROOT/'artifacts/baseline'/run_id/'observations.json'
            archive.write_text(encoded); archive.chmod(0o600)
            print(f"{sample['id']} repetition {index}: {record['execution']}; cleanup={record['teardown_status']}", flush=True)
            if record['execution'] != 'completed' or record['teardown_status'] != 'passed':
                return 2
    return 0  # Measurement completed, never an acceptance-quality exit code.


if __name__ == '__main__':
    raise SystemExit(main())
