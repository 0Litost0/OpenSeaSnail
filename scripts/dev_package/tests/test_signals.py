"""Real OS signal tests for the orchestrator/supervisor (isolated records)."""
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
import json,sys
from pathlib import Path
from scripts.dev_package.state import Run, atomic_json
from scripts.dev_package.process import RepositoryLock, Controller, Interrupted
root=Path(sys.argv[1])
with RepositoryLock(root) as lock:
    run=Run(root,'build',{})
    with Controller(Path.cwd(),run,lock) as controller:
        try:
            controller.execute('fixture',[sys.executable,'-c','import time; time.sleep(30)'])
        except Interrupted as exc:
            code=128+exc.number
            data=run.finish('interrupted',code)
            print(json.dumps(data),flush=True)
            sys.exit(code)
'''


class SignalTests(unittest.TestCase):
    def test_timeout_terminates_owned_group(self):
        driver=DRIVER.replace('Controller, Interrupted','Controller, Interrupted, StageTimeout').replace(
            "time.sleep(30)'])", "time.sleep(30)'],timeout=.2)").replace(
            'except Interrupted as exc:', 'except StageTimeout:\n            sys.exit(4)\n        except Interrupted as exc:')
        with tempfile.TemporaryDirectory() as folder:
            child=subprocess.run([sys.executable,'-B','-c',driver,folder],cwd=ROOT,capture_output=True,text=True,timeout=15)
            self.assertEqual(child.returncode,4,child.stderr)
            from scripts.dev_package.process import group_activity
            for marker in Path(folder).glob('dist/dev-package/runs/*/process-*.json'):
                self.assertEqual(group_activity(json.loads(marker.read_text())['pgid']),'inactive')

    def test_sigint_and_sigterm(self):
        for number in (signal.SIGINT,signal.SIGTERM):
            with self.subTest(number=number),tempfile.TemporaryDirectory() as folder:
                root=Path(folder)
                child=subprocess.Popen([sys.executable,'-B','-c',DRIVER,folder],cwd=ROOT,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
                try:
                    deadline=time.monotonic()+10
                    markers=[]
                    while time.monotonic()<deadline:
                        markers=list(root.glob('dist/dev-package/runs/*/process-*.json'))
                        if markers: break
                        time.sleep(.02)
                    self.assertTrue(markers,'Worker did not establish its process group')
                    time.sleep(.1)
                    child.send_signal(number)
                    stdout,stderr=child.communicate(timeout=15)
                    self.assertEqual(child.returncode,128+number,stderr)
                    data=json.loads(stdout)
                    self.assertEqual(data['status'],'interrupted')
                    self.assertEqual(data['exit_code'],128+number)
                finally:
                    if child.poll() is None: child.kill(); child.wait()
                    for marker in root.glob('dist/dev-package/runs/*/process-*.json'):
                        group=json.loads(marker.read_text())['pgid']
                        try: os.killpg(group,signal.SIGKILL)
                        except ProcessLookupError: pass


    def test_root_sigkill_then_supervisor_sigkill_retains_residual_gate(self):
        from scripts.dev_package.process import RepositoryLock, Residual, lock_busy
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder)
            child=subprocess.Popen([sys.executable,'-B','-c',DRIVER,folder],cwd=ROOT,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
            group=None
            try:
                deadline=time.monotonic()+10
                markers=[]
                while time.monotonic()<deadline:
                    markers=list(root.glob('dist/dev-package/runs/*/process-*.json'))
                    if markers: break
                    time.sleep(.02)
                self.assertTrue(markers)
                group=json.loads(markers[0].read_text())['pgid']
                time.sleep(.1)
                child.kill(); child.wait(timeout=5)
                self.assertTrue(lock_busy(root), 'Supervisor must retain lock after root SIGKILL')
                os.kill(group,signal.SIGKILL)
                deadline=time.monotonic()+5
                while lock_busy(root) and time.monotonic()<deadline: time.sleep(.02)
                self.assertFalse(lock_busy(root))
                with self.assertRaises(Residual):
                    with RepositoryLock(root): pass
            finally:
                if child.poll() is None: child.kill(); child.wait()
                if group:
                    try: os.killpg(group,signal.SIGTERM)
                    except ProcessLookupError: pass


if __name__=='__main__': unittest.main()
