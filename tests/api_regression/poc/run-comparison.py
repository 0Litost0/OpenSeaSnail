#!/usr/bin/env python3
"""PoC evidence collection only; each candidate remains its own test runner."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import time

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parents[2]
ARTIFACTS = ROOT / os.environ.get('POC_COMPARISON_DIR', 'artifacts/comparison')
NEXTEST = os.environ.get('POC_NEXTEST', '/private/tmp/seasnail-poc-tools/cargo-nextest')
ARTIFACTS.mkdir(parents=True, exist_ok=True)
records = []

def run(name, command, expected=0, extra=None):
    output = ARTIFACTS / name
    output.mkdir(exist_ok=True)
    env = {**os.environ, 'POC_PASSWORD':'POC_PASSWORD_CANARY_20261002', 'POC_PROVIDER_SECRET':'POC_PROVIDER_CANARY_20261002', 'POC_OUTPUT': str(output), 'POC_FAULT': '0', 'POC_FLAKY': '0', 'POC_RUNTIME': 'mock', **(extra or {})}
    start = time.monotonic()
    result = subprocess.run(command, cwd=ROOT, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=90)
    (output / 'console.txt').write_bytes(result.stdout)
    if command[0] == NEXTEST and 'run' in command:
        junit = ROOT / 'target/nextest/poc/junit.xml'
        shutil.copy2(junit, output/'junit.xml')
    records.append({'name':name, 'command':command, 'exit_code':result.returncode, 'expected_exit':expected, 'duration_seconds':round(time.monotonic()-start,3), 'matches_expectation':result.returncode==expected})
    (ARTIFACTS/'experiments.json').write_text(json.dumps(records,indent=2)+'\n')
    print(f'{name}: exit={result.returncode}, expected={expected}, seconds={records[-1]["duration_seconds"]}',flush=True)

pw=['pnpm','exec','playwright','test','comparison.spec.ts']
vt=['pnpm','exec','vitest','run','comparison.test.ts']
nx=[NEXTEST,'nextest','run','--manifest-path','Cargo.toml','--target-dir','../../../target','--offline','--test','comparison','--profile','poc']
candidates=os.environ.get('POC_CANDIDATES','pw,vitest,rust').split(',')
if 'pw' in candidates:run('pw-list',pw+['--list'])
if 'vitest' in candidates:run('vitest-list',['pnpm','exec','vitest','list','--json'])
if 'rust' in candidates:run('rust-list',[NEXTEST,'nextest','list','--manifest-path','Cargo.toml','--target-dir','../../../target','--offline','--test','comparison','--message-format','json'])
for name,command in [('pw',pw),('vitest',vt),('rust',nx)]:
    if name not in candidates:continue
    for n in range(3): run(f'{name}-pass-{n+1}',command)
    run(f'{name}-failure',command,expected=100 if name=='rust' else 1,extra={'POC_FAULT':'1'})
    selector=['--grep','DICT-001'] if name=='pw' else ['-t','DICT-001'] if name=='vitest' else ['-E','test(dict_001_crud_restart)']
    run(f'{name}-reproduce',command+selector,expected=100 if name=='rust' else 1,extra={'POC_FAULT':'1'})
    run(f'{name}-repair',command+selector)
if 'pw' in candidates:run('pw-flaky',pw+['--retries=1'],expected=1,extra={'POC_FLAKY':'1'})
raise SystemExit(0 if all(r['matches_expectation'] for r in records) else 1)
