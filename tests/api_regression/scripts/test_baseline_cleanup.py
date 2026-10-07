#!/usr/bin/env python3
"""Regression checks for M1 measurement cleanup; not formal M2 lifecycle acceptance."""
from pathlib import Path
import json
import os
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from measure_baseline import cleanup_owned, group_members, ps_identity

HELPER = '''
import subprocess,sys,time
child=subprocess.Popen(['/bin/sleep','60'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
print(child.pid,flush=True)
if sys.argv[1]=='early':
    sys.exit(1)
sys.stdin.buffer.read()
child.terminate();child.wait(timeout=3)
'''


class CleanupTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # Fail before spawning fixtures if the sandbox blocks process identity queries.
        ps_identity(os.getpid())

    def exercise(self, mode, observation_failed=False):
        home = Path(tempfile.mkdtemp(prefix='seasnail-m1-cleanup-check-'))
        journal = home / 'recovery.json'
        child = subprocess.Popen([sys.executable,'-c',HELPER,mode],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,start_new_session=True)
        descendant = int(child.stdout.readline())
        descendant_identity = ps_identity(descendant)
        owned = []
        def observe():
            if observation_failed:
                raise RuntimeError('controlled process observation failure')
            for process in group_members(child.pid):
                if process not in owned:
                    owned.append(process)
        try:
            if mode == 'early':
                child.wait(timeout=3)
            result = cleanup_owned(child,owned,home,journal,observe)
            if mode == 'normal' and not observation_failed:
                self.assertEqual(result['teardown_status'],'passed')
                self.assertTrue(result['home_removed'])
                self.assertFalse(journal.exists())
                self.assertIsNone(ps_identity(descendant))
            else:
                self.assertEqual(result['teardown_status'],'failed')
                self.assertTrue(home.exists())
                saved=json.loads(journal.read_text())
                self.assertEqual(saved['home'],str(home))
                self.assertEqual(journal.stat().st_mode & 0o777,0o600)
                if mode == 'early':
                    self.assertIn('unregistered_group_member_identity_unknown',result['cleanup_errors'])
                    self.assertTrue(any(p['pid']==descendant for p in saved['remaining_group_members']))
                    self.assertEqual(ps_identity(descendant),descendant_identity, 'unknown reparented process must not be killed')
                else:
                    self.assertIn('process_observation_failed',result['cleanup_errors'])
        finally:
            if child.poll() is None:
                child.stdin.close()
                try:
                    child.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    child.kill();child.wait(timeout=3)
            if ps_identity(descendant) == descendant_identity:
                os.kill(descendant,signal.SIGKILL)
                deadline=time.monotonic()+3
                while ps_identity(descendant) == descendant_identity and time.monotonic()<deadline:
                    time.sleep(0.05)
            if home.exists():
                import shutil
                shutil.rmtree(home)
            child.stdout.close()
            if not child.stdin.closed:
                child.stdin.close()

    def test_normal_shutdown_removes_owned_home(self):
        self.exercise('normal')

    def test_ready_failure_preserves_unregistered_reparented_process(self):
        self.exercise('early')

    def test_observation_failure_preserves_recovery_record(self):
        self.exercise('normal',observation_failed=True)


if __name__ == '__main__':
    unittest.main()
