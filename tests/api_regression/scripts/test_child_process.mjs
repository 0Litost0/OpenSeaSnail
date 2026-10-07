import {test} from 'node:test';
import assert from 'node:assert/strict';
import {admitInterruption,finalizeChild,stopChild} from '../fixtures/child-process.mjs';
const identity=pid=>({pid,birth:`birth-${pid}`,executable:`/owned/${pid}`});
const cli=identity(1),framework=identity(2),worker=identity(3),host=identity(4),sidecar=identity(5);
function fixture(){return {manifest:{run_id:'fresh',selected_case_ids:['SHERPA-001'],runner_pid:1,runner_identity:cli,framework_pid:2,framework_identity:framework},proof:{run_id:'fresh',case_id:'SHERPA-001',runner:worker,hosts:[host]},sidecar:{owner:host,child:sidecar},cliIdentity:cli,previousRuns:new Set(),inspect:async pid=>identity(pid),parent:async pid=>pid-1}}
test('interruption requires fresh run, registered births and the complete CLI/framework/worker/host/sidecar chain',async()=>{
  assert.equal(await admitInterruption(fixture()),true);
  const controls=[v=>v.previousRuns.add('fresh'),v=>v.manifest.runner_identity.birth='reused-cli',v=>v.manifest.framework_identity.birth='reused-framework',v=>v.proof.runner.birth='reused-worker',v=>v.proof.run_id='other',v=>v.proof.hosts=[],v=>v.sidecar.child.birth='reused-sidecar',v=>v.parent=async()=>999,v=>v.manifest.framework_identity=null];
  for(const mutate of controls){const v=fixture();v.manifest=structuredClone(v.manifest);v.proof=structuredClone(v.proof);v.sidecar=structuredClone(v.sidecar);mutate(v);assert.equal(await admitInterruption(v),false)}
});
test('force-stop waits for completion after SIGKILL instead of assuming delivery means exit',async()=>{
  let resolve;const completion=new Promise(done=>resolve=done);const signals=[];let exited=false;
  const child={exitCode:null,signalCode:null,kill(signal){signals.push(signal);if(signal==='SIGKILL')setTimeout(()=>{exited=true;resolve()},5)}};
  await stopChild(child,completion,1,50);assert.equal(exited,true);assert.deepEqual(signals,['SIGINT','SIGKILL']);
});
for(const phase of ['interruption admission','report parsing','report copying'])test(`${phase} failure still clears watchdogs, verifies child exit and recovers environment`,async()=>{
  let watchdog=false,recovered=false,exited=false;const errors=[];
  const timer=setTimeout(()=>watchdog=true,20);const child={exitCode:null,signalCode:null,kill(){exited=true;resolve()}};let resolve;const completion=new Promise(done=>resolve=done);
  await assert.rejects(async()=>{try{throw new Error(phase)}finally{await finalizeChild({child,completion,timers:[timer],recover:async()=>{recovered=true},recordFailure:code=>errors.push(code),grace:10,force:10})}},new RegExp(phase));
  await new Promise(done=>setTimeout(done,25));assert.equal(watchdog,false);assert.equal(exited,true);assert.equal(recovered,true);assert.deepEqual(errors,[]);
});
test('unverified forced exit and recovery failure both survive as separate cleanup failures',async()=>{
  const errors=[];let recoveryAttempted=false;const child={exitCode:null,signalCode:null,kill(){}};
  await finalizeChild({child,completion:new Promise(()=>{}),timers:[],recover:async()=>{recoveryAttempted=true;throw new Error('recovery failed')},recordFailure:code=>errors.push(code),grace:1,force:1});
  assert.equal(recoveryAttempted,true);assert.deepEqual(errors,['child_exit_unverified','child_environment_recovery_failed','child_environment_recovery_unverified']);
});

test('a force-killed ordinary or unadmitted child cannot claim descendants were cleaned',async()=>{
  const errors=[];let resolve;const completion=new Promise(done=>resolve=done);
  const child={exitCode:null,signalCode:null,kill(signal){if(signal==='SIGKILL')resolve()}};
  await finalizeChild({child,completion,timers:[],recover:async()=>false,recordFailure:code=>errors.push(code),grace:1,force:10});
  assert.deepEqual(errors,['child_environment_recovery_unverified']);
});
