#!/usr/bin/env python3
"""Focused follow-up experiments, with explicit expected failures and saved evidence."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parents[2]
OUT = ROOT/'artifacts/validation'
OUT.mkdir(parents=True, exist_ok=True)
base_env = {**os.environ,'POC_PASSWORD':'POC_PASSWORD_CANARY_20261002','POC_PROVIDER_SECRET':'POC_PROVIDER_CANARY_20261002','POC_FAULT':'0','POC_FLAKY':'0','POC_RUNTIME':'mock'}
real_env = {'POC_RUNTIME':'sherpa','SEASNAIL_ASR_ROOT':str(REPO/'dist/SeaSnail.app/Contents/Resources/asr'),'FFMPEG_PATH':str(REPO/'dist/SeaSnail.app/Contents/Resources/ffmpeg')}
records=[]
def run(name, args, expected=0, extra=None):
    directory=OUT/name;directory.mkdir(exist_ok=True)
    command=['pnpm','exec',*args]
    started=time.monotonic()
    result=subprocess.run(command,cwd=ROOT,env={**base_env,'POC_OUTPUT':str(directory),**(extra or {})},stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=180)
    (directory/'console.txt').write_bytes(result.stdout)
    records.append({'name':name,'command':command,'exit_code':result.returncode,'expected_exit':expected,'matches_expectation':result.returncode==expected,'duration_seconds':round(time.monotonic()-started,3)})
    (OUT/'experiments.json').write_text(json.dumps(records,indent=2)+'\n')
    print(f'{name}: exit={result.returncode}, expected={expected}',flush=True)

run('vitest-annotations-failure',['vitest','run','annotations.test.ts','--reporter=agent','--reporter=json','--reporter=junit'],1,{'POC_FAULT':'1'})
run('vitest-annotations-repair',['vitest','run','annotations.test.ts'])
run('pw-clean',['playwright','test','pipeline.spec.ts','--grep','CLEAN'])
run('vitest-clean',['vitest','run','pipeline.test.ts','-t','CLEAN'])
run('pw-sherpa',['playwright','test','pipeline.spec.ts','--grep','SHERPA'],extra=real_env)
run('vitest-sherpa',['vitest','run','pipeline.test.ts','-t','SHERPA'],extra=real_env)
run('pw-lifecycle',['playwright','test','lifecycle.spec.ts'])
run('pw-timeout',['playwright','test','lifecycle.spec.ts','--grep','LIFE-timeout'],1,{'POC_TIMEOUT':'1'})
run('pw-missing',['playwright','test','pipeline.spec.ts','--grep','SHERPA'],1,{'POC_RUNTIME':'sherpa','SEASNAIL_ASR_ROOT':str(OUT/'intentionally-missing-model')})
run('vitest-missing',['vitest','run','pipeline.test.ts','-t','SHERPA'],1,{'POC_RUNTIME':'sherpa','SEASNAIL_ASR_ROOT':str(OUT/'intentionally-missing-model')})

identity={}
for name,path in {'host':REPO/'target/debug/seasnail-poc-host','artifact_manifest':Path(real_env['SEASNAIL_ASR_ROOT'])/'sensevoice-small/sherpa_onnx/int8/artifact-manifest.json','audio':ROOT.parent/'assets/audio/fleurs-en-v1.wav','node_lock':ROOT/'pnpm-lock.yaml','rust_lock':ROOT/'Cargo.lock'}.items():
    identity[name]={'sha256':hashlib.sha256(path.read_bytes()).hexdigest(),'bytes':path.stat().st_size}
(OUT/'identity.json').write_text(json.dumps(identity,indent=2)+'\n')
raise SystemExit(0 if all(r['matches_expectation'] for r in records) else 1)
