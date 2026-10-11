"""Real stage interruption and rerun of an owned partial download."""
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest

ROOT=Path(__file__).resolve().parents[3]
DRIVER='''
import json,os,sys
from pathlib import Path
from scripts.dev_package.state import Run
from scripts.dev_package.process import RepositoryLock,Controller,Interrupted
from scripts.dev_package.temporary import recover_bucket
root=Path(sys.argv[1]); row=json.loads((root/'row.json').read_text())
with RepositoryLock(root) as lock:
    recover_bucket(root,'cache/.dev-package-downloads','download')
    run=Run(root,'build',{'fixture':'download interruption'})
    command=[sys.executable,'-B','-c',"import json,sys; from pathlib import Path; from scripts.dev_package.network import download; root=Path(sys.argv[1]); download(root,root/'cache',json.loads((root/'row.json').read_text()),sys.argv[2])",str(root),run.id]
    with Controller(Path.cwd(),run,lock) as controller:
        try: code=controller.execute('fixture_download',command,env=dict(os.environ,PATH=str(root/'bin')+os.pathsep+os.environ['PATH']))
        except Interrupted as exc: code=128+exc.number
        run.finish('succeeded' if not code else 'interrupted',code)
        sys.exit(code)
'''


class DownloadInterruptTests(unittest.TestCase):
    def test_interrupted_partial_is_removed_and_same_workflow_reruns(self):
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder).resolve(); (root/'bin').mkdir()
            payload=b'locked download fixture'
            (root/'row.json').write_text(json.dumps(dict(file_name='input.bin',url='https://example.invalid/fixture',size_bytes=len(payload),sha256=hashlib.sha256(payload).hexdigest())))
            (root/'mode').write_text('stall')
            fake=root/'bin/curl'
            fake.write_text('#!'+sys.executable+'\n'+
                "import sys,time\nfrom pathlib import Path\nroot=Path(__file__).resolve().parents[1]\noutput=Path(sys.argv[sys.argv.index('--output')+1])\n"+
                "if (root/'mode').read_text()=='stall':\n output.write_bytes(b'partial')\n while True: time.sleep(.05)\n"+
                'output.write_bytes('+repr(payload)+")\nprint('200',end='')\n")
            fake.chmod(0o755)
            child=subprocess.Popen([sys.executable,'-B','-c',DRIVER,str(root)],cwd=ROOT,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
            try:
                deadline=time.monotonic()+10
                while not list(root.glob('cache/.dev-package-downloads/*/input')) and time.monotonic()<deadline: time.sleep(.02)
                self.assertTrue(list(root.glob('cache/.dev-package-downloads/*/input')))
                child.send_signal(signal.SIGTERM); out,err=child.communicate(timeout=15)
                self.assertEqual(child.returncode,143,err)
                self.assertFalse((root/'cache/input.bin').exists())
                self.assertTrue(list(root.glob('cache/.dev-package-downloads/*/.owner.json')))
                (root/'mode').write_text('success')
                second=subprocess.run([sys.executable,'-B','-c',DRIVER,str(root)],cwd=ROOT,capture_output=True,text=True,timeout=15)
                self.assertEqual(second.returncode,0,second.stderr)
                self.assertEqual((root/'cache/input.bin').read_bytes(),payload)
                self.assertFalse(list((root/'cache/.dev-package-downloads').iterdir()))
            finally:
                if child.poll() is None: child.kill(); child.wait()
                for marker in root.glob('dist/dev-package/runs/*/process-*.json'):
                    try: os.killpg(json.loads(marker.read_text())['pgid'],signal.SIGKILL)
                    except ProcessLookupError: pass


if __name__=='__main__': unittest.main()
