import contextlib
import datetime as dt
import io
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

from scripts.dev_package import __main__ as cli, checks, process, state

ROOT=Path(__file__).resolve().parents[3]


class TemporaryRepo(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory()
        self.root=Path(self.temp.name)
    def tearDown(self): self.temp.cleanup()


class ContractTests(TemporaryRepo):
    def parse(self,*args): return cli.parser().parse_args(args)
    def test_invalid_combinations_have_no_writes(self):
        for args in [('check','--full'),('build','--suite','all'),('test','--dry-run'),('version','show','--dry-run'),('status','../unsafe'),('runs','--limit','0'),('runs','--since','2026-01-01')]:
            with self.subTest(args=args), contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as caught:
                with patch.object(cli,'ROOT',self.root): cli.main(args)
            self.assertEqual(caught.exception.code,2)
        self.assertEqual(list(self.root.iterdir()),[])
    def test_valid_parameters(self):
        self.assertEqual(self.parse('test').suite,'unit')
        self.assertTrue(self.parse('package','--full','--json').full)
        self.assertEqual(self.parse('version','bump','patch','--dry-run').part,'patch')
        self.assertTrue(self.parse('status','--current').current)
        self.assertEqual(self.parse('runs','--since','2026-10-11T00:00:00Z').since.utcoffset(),dt.timedelta(0))
    def test_shell_help_without_python_and_node(self):
        with tempfile.TemporaryDirectory() as folder:
            (Path(folder)/'dirname').symlink_to('/usr/bin/dirname')
            (Path(folder)/'cat').symlink_to('/bin/cat')
            env=dict(os.environ,PATH=folder)
            r=subprocess.run(['/bin/bash',str(ROOT/'scripts/dev_package/dev-package.sh'),'--help'],env=env,capture_output=True,text=True)
            self.assertEqual(r.returncode,0,r.stderr)
            r=subprocess.run(['/bin/bash',str(ROOT/'scripts/dev_package/dev-package.sh'),'check','--json'],env=env,capture_output=True,text=True)
            self.assertEqual(r.returncode,3)
            self.assertEqual(json.loads(r.stdout)['errors'][0]['code'],'python_unavailable')
    def test_shell_json_escapes_unusual_python_version(self):
        with tempfile.TemporaryDirectory() as folder:
            (Path(folder)/'dirname').symlink_to('/usr/bin/dirname')
            (Path(folder)/'python3').write_text('#!/bin/bash\nif [ "$1" = --version ]; then printf \'bad"\\\\value\\nline\\tend\\n\'; else exit 1; fi\n')
            (Path(folder)/'python3').chmod(0o755)
            r=subprocess.run(['/bin/bash',str(ROOT/'scripts/dev_package/dev-package.sh'),'check','--json'],env=dict(os.environ,PATH=folder),capture_output=True,text=True)
            self.assertEqual(r.returncode,3)
            self.assertIn('bad"\\value',json.loads(r.stdout)['errors'][0]['actual'])
    def test_empty_queries_are_read_only(self):
        for args in [('runs','--json'),('status','--current','--json')]:
            data,code=cli.dispatch(self.parse(*args),self.root)
            self.assertEqual(code,0); self.assertEqual(data['runs'],[])
        self.assertEqual(list(self.root.iterdir()),[])
    def test_unknown_run_and_path_validation(self):
        with self.assertRaises(ValueError): state.query(self.root,'../outside')
        with self.assertRaises(FileNotFoundError): state.query(self.root,state.new_id())
        self.assertEqual(list(self.root.iterdir()),[])
    def test_symlinked_runs_are_rejected(self):
        (self.root/'dist').symlink_to(self.root)
        with self.assertRaises(ValueError): state.list_runs(self.root)
    def test_schema_defaults(self):
        data=state.result('check')
        self.assertEqual(data['schema_version'],1)
        self.assertIsNone(data['run_id']); self.assertEqual(data['artifacts'],{})
        for name in ('validation_scope','requested_scope','changes','prepared','ready_for_task','test_summary','log_path'): self.assertIn(name,data)
    def test_query_abandoned_run_without_changing_record(self):
        run=state.Run(self.root,'build',{})
        run.data['owner']={'pid':99999999,'identity':'not alive'}; run.save()
        original=(run.directory/'state.json').read_bytes()
        data=state.query(self.root,run.id)
        self.assertEqual(data['status'],'abandoned')
        self.assertEqual((run.directory/'state.json').read_bytes(),original)
    def test_runs_filters(self):
        a=state.Run(self.root,'build',{}); a.finish('failed',6)
        b=state.Run(self.root,'version.increment',{}); b.finish('succeeded',0,changes={'committed':True})
        rows=state.list_runs(self.root,operation='version.increment',limit=1)
        self.assertEqual([x['run_id'] for x in rows],[b.id])
        self.assertTrue(rows[0]['changes']['committed'])
        self.assertEqual(state.list_runs(self.root,since=dt.datetime.now(dt.timezone.utc)+dt.timedelta(seconds=5)),[])
    def test_permission_failure_has_no_invented_log(self):
        with patch.object(state.Path,'mkdir',side_effect=PermissionError('denied')):
            with self.assertRaises(PermissionError): state.Run(self.root,'build',{})
        data,code=None,None
        with patch.object(process.RepositoryLock,'__enter__',side_effect=PermissionError('denied')):
            data,code=cli.dispatch(self.parse('build'),self.root)
        self.assertEqual(code,8); self.assertIsNone(data['log_path']); self.assertIsNone(data['run_id'])
    def test_mutation_stub_creates_honest_result_and_notice(self):
        with patch.object(cli,'source_identity',return_value={'head':'fixture'}),contextlib.redirect_stderr(io.StringIO()) as events:
            data,code=cli.dispatch(self.parse('build','--json'),self.root)
        self.assertEqual(code,6); self.assertEqual(data['status'],'blocked')
        self.assertEqual(data['errors'][0]['code'],'operation_unavailable')
        notice=json.loads(events.getvalue().split('DEV_PACKAGE_EVENT ',1)[1])
        self.assertEqual(notice['run_id'],data['run_id'])
        self.assertEqual(json.loads((state.run_directory(self.root,data['run_id'])/'result.json').read_text()),data)
        self.assertEqual(state.list_runs(self.root,current=True),[])


class CheckTests(TemporaryRepo):
    def test_declared_node_constraints(self):
        for value,valid in [('v22.22.1',False),('22.22.2',True),('23.0.0',False),('24.14.9',False),('24.15.0',True),('25.0.0',False),('26.0.0',True)]:
            self.assertEqual(checks.supported_node(value),valid,value)
    def test_locked_file_integrity(self):
        import hashlib
        p=self.root/'input'; p.write_bytes(b'correct')
        row={'size_bytes':7,'sha256':hashlib.sha256(b'correct').hexdigest()}
        checks.verify_file(p,row)
        p.write_bytes(b'corrupt')
        with self.assertRaises(ValueError): checks.verify_file(p,row)
        p.unlink(); p.symlink_to(self.root/'missing')
        with self.assertRaises(ValueError): checks.verify_file(p,row)
    def test_low_disk_is_read_only(self):
        with patch.object(checks.shutil,'disk_usage',return_value=shutil._ntuple_diskusage(100,99,1)):
            row=checks.disk_check(self.root,required=2)
        self.assertEqual(row['state'],'invalid'); self.assertEqual(list(self.root.iterdir()),[])
    def test_absent_declared_download_is_preparable(self):
        # Copy declarations only: no tools or cache installs are needed to classify inputs.
        for name in ['apps/desktop/package.json','apps/desktop/pnpm-lock.yaml','Cargo.toml','Cargo.lock','apps/desktop/native/post-paste-monitor/Package.resolved','scripts/sherpa/source-lock.json','scripts/sherpa/artifact-lock.json','scripts/sherpa/artifact-manifest.macos-arm64.json']:
            target=self.root/name; target.parent.mkdir(parents=True,exist_ok=True); shutil.copyfile(ROOT/name,target)
        before=sorted(str(p.relative_to(self.root)) for p in self.root.rglob('*'))
        rows,errors=[],[]; checks.inspect_inputs(self.root,rows,errors)
        self.assertFalse(errors)
        self.assertTrue(all(x['state']=='missing_preparable' for x in rows))
        self.assertEqual(before,sorted(str(p.relative_to(self.root)) for p in self.root.rglob('*')))
    def test_unknown_directory_is_not_reused(self):
        (self.root/'apps/desktop/node_modules').mkdir(parents=True)
        (self.root/'apps/desktop/node_modules/user-file').write_text('preserve')
        for name in ['apps/desktop/package.json','apps/desktop/pnpm-lock.yaml']:
            shutil.copyfile(ROOT/name,self.root/name)
        rows,errors=[],[]; checks.inspect_inputs(self.root,rows,errors,True)
        self.assertEqual(rows[0]['state'],'unknown'); self.assertTrue(errors)
        self.assertEqual((self.root/'apps/desktop/node_modules/user-file').read_text(),'preserve')


class ProcessTests(TemporaryRepo):
    def test_concurrent_lock_is_busy(self):
        with process.RepositoryLock(self.root):
            self.assertTrue(process.lock_busy(self.root))
            with self.assertRaises(process.Busy):
                with process.RepositoryLock(self.root): pass
        self.assertFalse(process.lock_busy(self.root))
    def test_residual_or_unknown_group_blocks(self):
        run=state.Run(self.root,'build',{})
        state.atomic_json(run.directory/'process-0001.json',{'pgid':None})
        with self.assertRaises(process.Residual):
            with process.RepositoryLock(self.root): pass
    def test_supervisor_retains_lock_when_parent_is_killed(self):
        # Worker runs from the actual module checkout, but lock/record are isolated.
        with process.RepositoryLock(self.root) as lock:
            marker=self.root/'worker.json'; request=self.root/'request.json'
            ready=self.root/'child-ready'
            command='import time; from pathlib import Path; Path('+repr(str(ready))+').touch(); time.sleep(30)'
            state.atomic_json(request,dict(command=[sys.executable,'-c',command],cwd=str(self.root),marker=str(marker),lock_fd=lock.fd))
            child=subprocess.Popen([sys.executable,'-B','-m','scripts.dev_package.worker',str(request)],cwd=ROOT,pass_fds=(lock.fd,),start_new_session=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
            try:
                deadline=time.monotonic()+5
                while not ready.exists() and time.monotonic()<deadline: time.sleep(.02)
                self.assertTrue(marker.exists()); self.assertTrue(ready.exists())
                # Close the parent's reference: supervisor alone retains the flock.
                os.close(lock.fd); lock.fd=None
                self.assertTrue(process.lock_busy(self.root))
                os.killpg(child.pid,signal.SIGTERM); child.wait(timeout=5)
                self.assertFalse(process.lock_busy(self.root))
            finally:
                if child.poll() is None:
                    os.killpg(child.pid,signal.SIGKILL); child.wait()
    def test_hard_killed_supervisor_leaves_group_gate(self):
        run=state.Run(self.root,'build',{})
        with process.RepositoryLock(self.root) as lock:
            marker=run.directory/'process-0001.json'; request=self.root/'request.json'
            state.atomic_json(request,dict(command=[sys.executable,'-c','import time; time.sleep(2)'],cwd=str(self.root),marker=str(marker),lock_fd=lock.fd))
            child=subprocess.Popen([sys.executable,'-B','-m','scripts.dev_package.worker',str(request)],cwd=ROOT,pass_fds=(lock.fd,),start_new_session=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
            try:
                deadline=time.monotonic()+5
                while not marker.exists() and time.monotonic()<deadline: time.sleep(.02)
                self.assertTrue(marker.exists()); time.sleep(.1)
                os.kill(child.pid,signal.SIGKILL); child.wait()
                os.close(lock.fd); lock.fd=None
                self.assertFalse(process.lock_busy(self.root))
                with self.assertRaises(process.Residual):
                    with process.RepositoryLock(self.root): pass
            finally:
                try: os.killpg(child.pid,signal.SIGTERM)
                except ProcessLookupError: pass
    def test_controlled_signals_return_130_and_143(self):
        for number in (signal.SIGINT,signal.SIGTERM):
            with process.RepositoryLock(self.root) as lock:
                run=state.Run(self.root,'build',{})
                with process.Controller(ROOT,run,lock) as controller:
                    controller.signal=number
                    with self.assertRaises(process.Interrupted) as caught:
                        controller.execute('fixture',[sys.executable,'-c','pass'])
                    self.assertEqual(128+caught.exception.number,130 if number==signal.SIGINT else 143)


if __name__=='__main__': unittest.main()
