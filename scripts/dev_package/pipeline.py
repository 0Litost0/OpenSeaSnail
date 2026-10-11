"""Sequential stage adapters. Public build/package integration arrives in M3."""
import hashlib
import json
import os
from pathlib import Path
import plistlib
import subprocess
import sys
import time

from . import checks, native, publication, temporary
from .errors import Failure
from .recovery import owned_directory, remove_owned
from .process import StageTimeout
from .state import atomic_json, safe_path


class Pipeline:
    def __init__(self,root,run,controller,tools, *, full=False):
        self.root=Path(root).resolve(); self.run=run; self.controller=controller
        self.tools=tools; self.full=full; self.outputs={}; self.validations=[]
        self.tools_dir=owned_directory(self.root,run.directory.relative_to(self.root)/'tools',run.id,'tool-paths')
        for name,path in {**tools,'python3':sys.executable}.items():
            (self.tools_dir/name).symlink_to(path)
        self.env=checks.tool_environment({'PATH':str(self.tools_dir)+os.pathsep+os.environ.get('PATH',''),
                      'CI':'true','PYTHONDONTWRITEBYTECODE':'1','DEV_PACKAGE_RUN_ID':run.id,
                      'MACOSX_DEPLOYMENT_TARGET':plistlib.loads((self.root/'packaging/macos/Info.plist').read_bytes())['LSMinimumSystemVersion']})
        self.env.update(CARGO_NET_RETRY='2',CARGO_HTTP_TIMEOUT='3600',CARGO_HTTP_LOW_SPEED_LIMIT='1024')
        # Command-local Git configuration; preserve inherited config and never
        # write developer/global configuration.
        count=int(self.env.get('GIT_CONFIG_COUNT','0'))
        for key,value in [('http.lowSpeedLimit','1024'),('http.lowSpeedTime','120')]:
            self.env[f'GIT_CONFIG_KEY_{count}']=key; self.env[f'GIT_CONFIG_VALUE_{count}']=value; count+=1
        self.env['GIT_CONFIG_COUNT']=str(count)
        self.locks={name:native.sha(self.root/name) for name in ['Cargo.lock','apps/desktop/pnpm-lock.yaml','apps/desktop/native/post-paste-monitor/Package.resolved']}
        self.budgets=list(checks.POLICY['disk_phase_bytes'])
        for parent,kind in [('third_party/ffmpeg/macos-arm64/cache/.dev-package-downloads','download'),
                ('third_party/sherpa/macos-arm64/cache/.dev-package-downloads','download'),
                ('third_party/sherpa/macos-arm64/sources/.dev-package-sources','source-checkout'),
                ('build/dev-package/ffmpeg-work','ffmpeg-work')]:
            for path in temporary.recover_bucket(self.root,parent,kind): self.run.event('temporary_recovered',path=path)

    def assert_locks(self):
        for name,digest in self.locks.items():
            if native.sha(self.root/name)!=digest: raise Failure(5,'lock_changed','Dependency preparation changed a committed lock',path=self.root/name)

    def stage(self,name,command, *, code=6,cwd=None,timeout=None):
        report=self.run.directory/f'error-{self.controller.counter+1:04d}.json'
        env=dict(self.env,DEV_PACKAGE_ERROR_REPORT=str(report))
        try: result=self.controller.execute(name,command,cwd=cwd,env=env,timeout=timeout)
        except StageTimeout as exc:
            failure=Failure(4,'stage_network_timeout',str(exc),retry='safe'); failure.detail['stage']=name
            raise failure from exc
        self.assert_locks()
        if result:
            if report.is_file():
                data=json.loads(report.read_text())
                if data.get('schema_version')==1 and data.get('exit_code')==result and isinstance(data.get('error'),dict):
                    exc=Failure(result,data['error']['code'],data['error']['reason'])
                    exc.detail=data['error']; exc.detail['stage']=name; raise exc
            exc=Failure(code,'stage_failed','Stage returned nonzero without a structured classification',child_code=result)
            exc.detail['stage']=name; raise exc

    def disk(self,phase):
        budgets=checks.POLICY['disk_phase_bytes']
        required=sum(budgets[k] for k in self.budgets)
        checked=checks.disk_check(self.root,required)
        self.run.event('disk_checked',phase=phase,**checked)
        if checked['state']!='ready': raise Failure(8,'disk_insufficient','Insufficient remaining stage reservation',path=checked['path'])

    def completed(self,phase):
        if phase in self.budgets: self.budgets.remove(phase)

    def prepare_dependencies(self):
        self.disk('frontend')
        self.stage('frontend_dependencies',[self.tools['pnpm'],'install','--frozen-lockfile','--fetch-retries=2','--fetch-timeout=3600000'],cwd=self.root/'apps/desktop',timeout=10800)
        generated=self.run.directory/'openapi.ts'
        self.stage('openapi_compare',[self.tools['pnpm'],'exec','openapi-typescript',str(self.root/'proto/openapi.yaml'),'-o',str(generated)],cwd=self.root/'apps/desktop',code=5)
        if generated.read_bytes()!=(self.root/'apps/desktop/src/api/openapi.ts').read_bytes():
            raise Failure(5,'openapi_stale','Generated OpenAPI source is stale; regenerate explicitly',path=self.root/'apps/desktop/src/api/openapi.ts')
        self.stage('cargo_dependencies',[self.tools['cargo'],'fetch','--locked','--target','aarch64-apple-darwin'],timeout=10800)
        scratch=owned_directory(self.root,'build/dev-package/swift/'+('full-'+self.run.id if self.full else 'incremental'),self.run.id,'swift-build',reuse=not self.full)
        cache=owned_directory(self.root,'build/dev-package/swift-cache',self.run.id,'swift-cache',reuse=True)
        self.outputs.update(swift_scratch=str(scratch),swift_cache=str(cache))
        self.stage('swift_dependencies',[self.tools['swift'],'package','--package-path',str(self.root/'apps/desktop/native/post-paste-monitor'),
             '--scratch-path',str(scratch),'--cache-path',str(cache),'--manifest-cache','local','--disable-keychain','--force-resolved-versions','resolve'],timeout=10800)
        self.completed('cargo_dependency_sources')
        self.validations.append('frozen_pnpm_locked_cargo_resolved_swift_and_openapi_compare')

    def prepare_sherpa_inputs(self):
        self.disk('locked_inputs')
        for attempt in range(1,4):
            try:
                self.stage('sherpa_inputs',[sys.executable,'-B','-m','scripts.dev_package.phases.fetch_sherpa'],code=5,timeout=14400)
                break
            except Failure as exc:
                if exc.detail['code']!='source_network_exhausted' or attempt==3: raise
                for path in temporary.recover_bucket(self.root,'third_party/sherpa/macos-arm64/sources/.dev-package-sources','source-checkout'):
                    self.run.event('temporary_recovered',path=path)
                self.run.event('source_retry',attempt=attempt+1,maximum_attempts=3)
                time.sleep(attempt)
        self.completed('locked_inputs'); self.validations.append('sherpa_full_lock_source_archive_model_license_verification')

    def native_workspace(self,kind,identity,key):
        relative=Path('build/dev-package/native')/kind/(key if not self.full else key+'-full-'+self.run.id)
        base=owned_directory(self.root,relative,self.run.id,kind+'-build',reuse=not self.full)
        publication.recover(self.root,base,identity)
        return base,base/'artifact',base/'receipt.json'

    def prepare_ffmpeg(self):
        self.disk('ffmpeg')
        identity,key=native.receipt_identity(self.root,self.tools,'ffmpeg')
        base,artifact,receipt=self.native_workspace('ffmpeg',identity,key)
        if artifact.exists():
            if not self.full and native.reusable(receipt,identity,artifact):
                self.run.event('native_reused',native_kind='ffmpeg',receipt=str(receipt)); self.outputs['ffmpeg']=str(artifact/'ffmpeg'); self.completed('ffmpeg'); return
            raise Failure(5,'native_incomplete','Existing output has no valid completion receipt',path=artifact)
        staging=publication.begin(self.root,base,self.run.id)
        work=owned_directory(self.root,'build/dev-package/ffmpeg-work/'+self.run.id,self.run.id,'ffmpeg-work')
        self.env['DEV_PACKAGE_FFMPEG_WORK_PARENT']=str(work)
        try:
            self.stage('ffmpeg_build',['/bin/bash',str(self.root/'scripts/dev_package/phases/build-ffmpeg-macos-arm64.sh'),
                  '--cache-dir',str(self.root/'third_party/ffmpeg/macos-arm64/cache'),'--output',str(staging)])
        finally:
            self.env.pop('DEV_PACKAGE_FFMPEG_WORK_PARENT',None)
        # execute only returns once the entire group is inactive. Exceptions
        # retain the owned workspace for recovery after the residual-group gate.
        remove_owned(self.root,work,'ffmpeg-work')
        self.stage('ffmpeg_source_verification',[sys.executable,str(self.root/'scripts/licenses/verify-ffmpeg-source.py'),str(staging)],code=5)
        validation=native.deployment(staging/'ffmpeg',self.env['MACOSX_DEPLOYMENT_TARGET'],tools=self.tools)
        if native.receipt_identity(self.root,self.tools,'ffmpeg')[0]!=identity:
            raise Failure(5,'native_inputs_changed','Native inputs changed during compilation')
        publication.publish(self.root,base,staging,identity,[validation,'trusted_source_recipe_and_full_corresponding_source'])
        self.outputs['ffmpeg']=str(artifact/'ffmpeg'); self.completed('ffmpeg')

    def prepare_sherpa_native(self):
        self.disk('sherpa_native')
        identity,key=native.receipt_identity(self.root,self.tools,'sherpa')
        base,artifact,receipt=self.native_workspace('sherpa',identity,key)
        if artifact.exists():
            if not self.full and native.reusable(receipt,identity,artifact):
                native.verify_sherpa(self.root,artifact,tools=self.tools)
                self.run.event('native_reused',native_kind='sherpa',receipt=str(receipt)); self.outputs['sherpa']=str(artifact); self.completed('sherpa_native'); return
            raise Failure(5,'native_incomplete','Existing output has no valid completion receipt',path=artifact)
        source_root=self.root/'third_party/sherpa/macos-arm64'
        command=['--source',str(source_root/'sources/sherpa-onnx'),'--onnxruntime-source',str(source_root/'sources/onnxruntime'),'--cache',str(source_root/'cache')]
        compile_root=base/'compile'
        self.stage('sherpa_build',['/bin/bash',str(self.root/'scripts/sherpa/build-probe-macos-arm64.sh'),*command,'--output',str(compile_root)])
        staging=publication.begin(self.root,base,self.run.id)
        self.stage('sherpa_prepare',['/bin/bash',str(self.root/'scripts/sherpa/prepare-macos-arm64.sh'),*command,'--native-install',str(compile_root/'install'),'--output',str(staging)],code=5)
        validation=native.verify_sherpa(self.root,staging,tools=self.tools)
        if native.receipt_identity(self.root,self.tools,'sherpa')[0]!=identity:
            raise Failure(5,'native_inputs_changed','Native inputs changed during compilation')
        publication.publish(self.root,base,staging,identity,validation+['trusted_fresh_prepare_and_full_lock_validation'])
        self.outputs['sherpa']=str(artifact); self.completed('sherpa_native')
