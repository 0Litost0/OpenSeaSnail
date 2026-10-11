"""Adapt existing strict Sherpa locks and source checkout with typed failures."""
import argparse
import importlib.util
import json
from pathlib import Path
import os
import subprocess
import sys
import uuid
from ..errors import Failure,phase_main
from ..network import download,secure_url
from ..state import safe_path
from ..recovery import owned_directory,remove_owned

ROOT=Path(__file__).resolve().parents[3]


def prepare():
    args=argparse.ArgumentParser()
    args.add_argument('--root',default='third_party/sherpa/macos-arm64')
    parsed=args.parse_args()
    root=safe_path(ROOT,parsed.root)
    source=json.loads((ROOT/'scripts/sherpa/source-lock.json').read_text())
    artifact=json.loads((ROOT/'scripts/sherpa/artifact-lock.json').read_text())
    script=ROOT/'scripts/sherpa/fetch-inputs.py'
    spec=importlib.util.spec_from_file_location('locked_sherpa_fetch',script)
    fetch=importlib.util.module_from_spec(spec); spec.loader.exec_module(fetch)
    cache=root/'cache'; sources=root/'sources'
    safe_path(ROOT,cache.relative_to(ROOT)); safe_path(ROOT,sources.relative_to(ROOT))
    sources.mkdir(parents=True,exist_ok=True)
    run_id=os.environ.get('DEV_PACKAGE_RUN_ID','private-stage')
    for row in source['source_archives']+source['onnxruntime_cmake_dependencies']+artifact['downloads']:
        download(ROOT,cache,row,run_id)
    for name,key in [('sherpa-onnx','sherpa_onnx'),('onnxruntime','onnxruntime')]:
        secure_url(source[key]['repository'])
        target=safe_path(ROOT,(sources/name).relative_to(ROOT))
        # The parent retries classified timeouts only after its supervisor has
        # confirmed the whole stage group exited. Never delete a clone tree
        # while a timed-out Git descendant could still be writing to it.
        workspace=None
        succeeded=False
        try:
            if target.exists(): fetch.checkout(sources,name,source[key])
            else:
                workspace=owned_directory(ROOT,sources.relative_to(ROOT)/'.dev-package-sources'/f'{run_id}-{uuid.uuid4().hex}',run_id,'source-checkout')
                fetch.checkout(workspace,name,source[key],cleanup_temporary=False)
                fetch.checkout(workspace,name,source[key]) # revision/clean/submodules before publication
                (workspace/name).rename(target)
            succeeded=True
        except subprocess.TimeoutExpired as exc:
            raise Failure(4,'source_network_exhausted','Git source preparation timed out',path=sources/name,retry='safe') from exc
        except ValueError as exc: raise Failure(5,'source_integrity',str(exc),path=sources/name) from exc
        except subprocess.CalledProcessError as exc:
            raise Failure(6,'source_stage_failed','Source checkout failed without a trustworthy network classification',path=sources/name,child_code=exc.returncode) from exc
        finally:
            if workspace is not None and succeeded: remove_owned(ROOT,workspace,'source-checkout')
    completed=subprocess.run(['/bin/bash',str(ROOT/'scripts/sherpa/verify-locks.sh'),'--cache',str(cache),'--source',str(sources/'sherpa-onnx'),'--onnxruntime-source',str(sources/'onnxruntime')])
    if completed.returncode: raise Failure(5,'sherpa_lock_verification','Strict upstream/source/model/license validation failed',path=root,child_code=completed.returncode)


if __name__=='__main__': sys.exit(phase_main(prepare))
