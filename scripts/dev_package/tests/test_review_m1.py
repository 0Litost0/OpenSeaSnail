"""Regression cases from the independent M1 review."""
import io
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
from scripts.dev_package import checks, process, state

ROOT=Path(__file__).resolve().parents[3]
DECLARATIONS=['apps/desktop/package.json','apps/desktop/pnpm-lock.yaml','Cargo.toml','Cargo.lock','apps/desktop/native/post-paste-monitor/Package.resolved','scripts/sherpa/source-lock.json','scripts/sherpa/artifact-lock.json','scripts/sherpa/artifact-manifest.macos-arm64.json']


class ReviewRegressions(unittest.TestCase):
    def setup_repo(self,root):
        for name in DECLARATIONS:
            target=root/name; target.parent.mkdir(parents=True,exist_ok=True); shutil.copyfile(ROOT/name,target)
    def test_tool_shims_cannot_enable_download_or_pin(self):
        with patch.dict(os.environ, {'RUSTUP_AUTO_INSTALL':'1','COREPACK_ENABLE_NETWORK':'1','COREPACK_ENABLE_AUTO_PIN':'1','COREPACK_DEFAULT_TO_LATEST':'1','COREPACK_ENV_FILE':'.env'}):
            environment=checks.tool_environment()
            for name in ('RUSTUP_AUTO_INSTALL','COREPACK_ENABLE_NETWORK','COREPACK_ENABLE_AUTO_PIN','COREPACK_DEFAULT_TO_LATEST','COREPACK_ENV_FILE'):
                self.assertEqual(environment[name],'0')
        with patch.object(checks.subprocess,'run',return_value=subprocess.CompletedProcess([],0,'fixture\n','')) as mocked:
            checks.output(['fixture','--version'])
            self.assertEqual(mocked.call_args.kwargs['env']['GIT_NO_LAZY_FETCH'],'1')
    def test_malformed_lock_is_integrity_error(self):
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder).resolve(); self.setup_repo(root)
            (root/'scripts/sherpa/source-lock.json').write_text('{invalid')
            before=list(root.rglob('*')); rows,errors=[],[]
            checks.inspect_inputs(root,rows,errors)
            self.assertEqual(errors[0]['exit_code'],5)
            self.assertEqual(errors[0]['code'],'input_invalid')
            self.assertEqual(before,list(root.rglob('*')))
    def test_ffmpeg_parent_symlink_is_blocked_before_verifier(self):
        with tempfile.TemporaryDirectory() as folder,tempfile.TemporaryDirectory() as external:
            root=Path(folder).resolve(); self.setup_repo(root)
            (root/'third_party').mkdir(); (root/'third_party/ffmpeg').symlink_to(external)
            rows,errors=[],[]
            with patch.object(checks,'output',side_effect=AssertionError('external input must not be inspected')):
                checks.inspect_inputs(root,rows,errors)
            self.assertTrue(any(x['code']=='ffmpeg_invalid' for x in errors))
    def test_unrecognized_extra_artifact_does_not_pass(self):
        import hashlib
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder).resolve(); self.setup_repo(root)
            artifact=root/'third_party/sherpa/macos-arm64/artifact'; artifact.mkdir(parents=True)
            expected=json.loads((ROOT/'scripts/sherpa/artifact-manifest.macos-arm64.json').read_text())
            # Synthetic pinned files keep this regression runnable in a new clone.
            for row in expected['files']:
                payload=('fixture:'+row['path']).encode()
                row.update(size_bytes=len(payload),sha256=hashlib.sha256(payload).hexdigest())
                target=artifact/row['path']; target.parent.mkdir(parents=True,exist_ok=True)
                target.write_bytes(payload)
            (artifact/'artifact-manifest.json').write_text(json.dumps(expected))
            (root/'scripts/sherpa/artifact-manifest.macos-arm64.json').write_text(json.dumps(expected))
            licenses=artifact/'licenses'; licenses.mkdir()
            fake=b'fake license'; (licenses/'FAKE.txt').write_bytes(fake)
            (licenses/'license-manifest.json').write_text(json.dumps({'files':[{'path':'FAKE.txt','size_bytes':len(fake),'sha256':hashlib.sha256(fake).hexdigest()}]}))
            (artifact/'extra').write_text('unknown')
            rows,errors=[],[]; checks.inspect_inputs(root,rows,errors)
            row=next(x for x in rows if x['name']=='sherpa.artifact')
            self.assertEqual(row['state'],'unknown'); self.assertFalse(row['verified']); self.assertTrue(errors)
            (artifact/'extra').unlink(); rows,errors=[],[]; checks.inspect_inputs(root,rows,errors)
            row=next(x for x in rows if x['name']=='sherpa.artifact')
            self.assertEqual(row['state'],'missing_preparable'); self.assertFalse(row['verified'])
            self.assertIn('provenance',row['reason'])
    def test_signal_at_poll_exit_is_not_lost(self):
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder).resolve()
            with process.RepositoryLock(root) as lock:
                run=state.Run(root,'build',{})
                with process.Controller(ROOT,run,lock) as controller:
                    class Child:
                        returncode=0
                        pid=99999999
                        def poll(self): controller.on_signal(signal.SIGTERM,None); return 0
                    with patch.object(process.subprocess,'Popen',return_value=Child()):
                        with self.assertRaises(process.Interrupted) as caught:
                            controller.execute('fixture',[sys.executable,'-c','pass'])
                    self.assertEqual(caught.exception.number,signal.SIGTERM)


if __name__=='__main__': unittest.main()

class ToolResolverRegressions(unittest.TestCase):
    def test_empty_rustup_home_does_not_initialize(self):
        from scripts.dev_package.tools import rustup_binary, ToolUnavailable
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder).resolve(); (root/'rustup').write_text('do not execute')
            (root/'rustc').symlink_to(root/'rustup')
            with patch.dict(os.environ,{'RUSTUP_HOME':str(root/'empty'),'RUSTUP_TOOLCHAIN':'1.0.1'}):
                with self.assertRaises(ToolUnavailable): rustup_binary(root/'rustc','rustc',ROOT)
            self.assertFalse((root/'empty').exists())
    def test_empty_corepack_cache_does_not_initialize(self):
        from scripts.dev_package.tools import pnpm_binary, ToolUnavailable
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder).resolve(); shim=root/'pnpm'
            shim.write_text("require('./lib/corepack.cjs').runMain(['pnpm'])")
            with patch.dict(os.environ,{'COREPACK_HOME':str(root/'empty')}):
                with self.assertRaises(ToolUnavailable): pnpm_binary(shim,ROOT)
            self.assertFalse((root/'empty').exists())
    def test_selected_rustup_binary_is_resolved_without_shim(self):
        from scripts.dev_package.tools import rustup_binary
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder).resolve(); (root/'rustup').write_text('do not execute')
            (root/'rustc').symlink_to(root/'rustup')
            tool=root/'cache/toolchains/explicit-toolchain/bin/rustc'; tool.parent.mkdir(parents=True)
            tool.write_text('installed'); tool.chmod(0o755)
            with patch.dict(os.environ,{'RUSTUP_HOME':str(root/'cache'),'RUSTUP_TOOLCHAIN':'explicit-toolchain'}):
                self.assertEqual(rustup_binary(root/'rustc','rustc',ROOT),str(tool))
    def test_installed_corepack_binary_is_resolved_without_shim(self):
        from scripts.dev_package.tools import pnpm_binary
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder).resolve(); shim=root/'pnpm'; shim.write_text("require('./lib/corepack.cjs')")
            install=root/'cache/v1/pnpm/9.15.9'; (install/'bin').mkdir(parents=True)
            (install/'.corepack').write_text('{}')
            (install/'package.json').write_text(json.dumps({'name':'pnpm','version':'9.15.9','bin':{'pnpm':'bin/pnpm.cjs'}}))
            tool=install/'bin/pnpm.cjs'; tool.write_text('installed'); tool.chmod(0o755)
            with patch.dict(os.environ,{'COREPACK_HOME':str(root/'cache')}):
                self.assertEqual(pnpm_binary(shim,ROOT),str(tool))

    def test_malformed_manager_metadata_is_tool_unavailable(self):
        from scripts.dev_package.tools import pnpm_binary, ToolUnavailable
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder).resolve(); shim=root/'pnpm'; shim.write_text("require('./lib/corepack.cjs')")
            install=root/'cache/v1/pnpm/9.15.9'; install.mkdir(parents=True)
            (install/'.corepack').write_text('{}')
            for metadata in [[],None,{'name':'pnpm','version':'9.15.9','bin':None},{'name':'pnpm','version':'9.15.9','bin':'bin/pnpm.cjs'}]:
                (install/'package.json').write_text(json.dumps(metadata))
                with patch.dict(os.environ,{'COREPACK_HOME':str(root/'cache')}):
                    with self.assertRaises(ToolUnavailable): pnpm_binary(shim,ROOT)
