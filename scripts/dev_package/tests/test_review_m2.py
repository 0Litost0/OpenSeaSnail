"""Review regressions: native manifest classification and live work retention."""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from scripts.dev_package import native
from scripts.dev_package.errors import Failure
from scripts.dev_package.process import RepositoryLock, Residual, group_activity

ROOT=Path(__file__).resolve().parents[3]
DRIVER=r'''
import os,signal,sys,threading,time
from pathlib import Path
from unittest.mock import patch
from scripts.dev_package import native,publication,state
from scripts.dev_package.pipeline import Pipeline
from scripts.dev_package.process import Controller,RepositoryLock,Interrupted,StageTimeout
root=Path(sys.argv[1]).resolve(); ready=root/'ready'; mode=sys.argv[2]
command="import os,signal,time; from pathlib import Path; pid=os.fork();\nif pid==0:\n signal.signal(signal.SIGTERM,signal.SIG_IGN); Path("+repr(str(ready))+").write_text('ready'); time.sleep(60)\nelse: time.sleep(60)"
with RepositoryLock(root) as lock:
 run=state.Run(root,'build',{})
 with Controller(Path.cwd(),run,lock) as controller:
  pipeline=Pipeline.__new__(Pipeline)
  pipeline.root=root; pipeline.run=run; pipeline.tools={}; pipeline.env={}; pipeline.full=False
  pipeline.disk=lambda phase:None
  base=root/'native'; base.mkdir()
  pipeline.native_workspace=lambda *args:(base,base/'artifact',base/'receipt.json')
  pipeline.stage=lambda *args,**kwargs:controller.execute('fixture',[sys.executable,'-c',command],timeout=.5 if mode=='timeout' else None)
  def interrupt():
   while not ready.exists(): time.sleep(.01)
   os.kill(os.getpid(),signal.SIGTERM)
  if mode=='signal':
   thread=threading.Thread(target=interrupt,daemon=True); thread.start()
  with patch.object(native,'receipt_identity',return_value=({},'fixture')):
   try: pipeline.prepare_ffmpeg()
   except (Interrupted,StageTimeout):
    code=143 if mode=='signal' else 4
    assert (root/'build/dev-package/ffmpeg-work'/run.id).is_dir()
    run.finish('interrupted' if mode=='signal' else 'blocked',code)
    sys.exit(code)
'''


class ReviewM2Tests(unittest.TestCase):
    def test_invalid_manifests_are_integrity_failures_with_paths(self):
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder); artifact=root/'artifact'; artifact.mkdir()
            pinned=root/'scripts/sherpa/artifact-manifest.macos-arm64.json'
            pinned.parent.mkdir(parents=True); pinned.write_text('{"files":[]}')
            manifest=artifact/'artifact-manifest.json'
            licenses=artifact/'licenses/license-manifest.json'; licenses.parent.mkdir()
            for path in (manifest,licenses):
                for invalid in ('{','null','[]','{}','{"files":[{}]}','{"files":[null]}'):
                    with self.subTest(path=path,invalid=invalid):
                        manifest.write_text('{"files":[]}'); licenses.write_text('{"files":[]}')
                        path.write_text(invalid)
                        with self.assertRaises(Failure) as raised: native.verify_sherpa(root,artifact)
                        self.assertEqual(raised.exception.exit_code,5)
                        self.assertEqual(raised.exception.detail['path'],str(path))

    def test_interrupted_ffmpeg_retains_work_and_blocks_recovery_with_live_descendant(self):
        for mode in ('signal','timeout'):
            with self.subTest(mode=mode): self.check_live_work(mode)

    def check_live_work(self,mode):
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder); group=None
            child=subprocess.Popen([sys.executable,'-B','-c',DRIVER,folder,mode],cwd=ROOT,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,text=True)
            try:
                child.wait(timeout=15)
                self.assertEqual(child.returncode,143 if mode=='signal' else 4)
                marker=next(root.glob('dist/dev-package/runs/*/process-*.json'))
                group=json.loads(marker.read_text())['pgid']
                self.assertEqual(group_activity(group),'active')
                self.assertTrue(list((root/'build/dev-package/ffmpeg-work').glob('*/.owner.json')))
                with self.assertRaises(Residual):
                    with RepositoryLock(root): pass
            finally:
                if child.poll() is None: child.kill(); child.wait()
                if group:
                    try: os.killpg(group,signal.SIGKILL)
                    except ProcessLookupError: pass
                else:
                    for marker in root.glob('dist/dev-package/runs/*/process-*.json'):
                        try: os.killpg(json.loads(marker.read_text())['pgid'],signal.SIGKILL)
                        except ProcessLookupError: pass
            deadline=time.monotonic()+5
            while group_activity(group)!='inactive' and time.monotonic()<deadline: time.sleep(.02)
            self.assertEqual(group_activity(group),'inactive')
            from scripts.dev_package.temporary import recover_bucket
            with RepositoryLock(root):
                self.assertEqual(len(recover_bucket(root,'build/dev-package/ffmpeg-work','ffmpeg-work')),1)
            self.assertFalse(list((root/'build/dev-package/ffmpeg-work').iterdir()))


if __name__=='__main__': unittest.main()
