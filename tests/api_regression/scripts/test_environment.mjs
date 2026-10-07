import { test } from 'node:test';
import {spawn} from 'node:child_process';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { AttemptEnvironment,cleanupStale } from '../fixtures/environment.mjs';
const root=await fs.realpath(await fs.mkdtemp(path.join(os.tmpdir(),'seasnail-api-m2-')));
const environments=[];
async function create(caseId,options={}) {const env=new AttemptEnvironment({root,caseId,...options});environments.push(env);await env.prepare();await env.start();return env}
async function call(env,method,url,data,expected=200) {
  const response=await fetch(env.baseURL+url,{method,headers:{'Content-Type':'application/json',...(env.token?{Authorization:`Bearer ${env.token}`}:{})},body:data===undefined?undefined:JSON.stringify(data),signal:AbortSignal.timeout(5000)});
  assert.equal(response.status,expected,`${method} ${url}`);return expected===204?null:response.json();
}
async function ready(env) {const deadline=Date.now()+90000;while(Date.now()<deadline){const models=await call(env,'GET','/models');if(models.some(m=>m.id==='sensevoice-small-sherpa-int8'&&m.status==='active'))return;await new Promise(r=>setTimeout(r,25))}throw new Error('model ready deadline')}
async function upload(env) {
  const bytes=await fs.readFile(new URL('../assets/audio/pipeline-v1.wav',import.meta.url));
  const form=new FormData();form.set('source','imported');form.set('language','zh');form.set('audio',new Blob([bytes],{type:'audio/wav'}),'zh.wav');
  const response=await fetch(env.baseURL+'/sessions',{method:'POST',headers:{Authorization:`Bearer ${env.token}`},body:form,signal:AbortSignal.timeout(5000)});assert.equal(response.status,202);return (await response.json()).id;
}
async function terminal(env,id,wanted) {const deadline=Date.now()+60000;while(Date.now()<deadline){const session=await call(env,'GET',`/sessions/${id}`);if(['completed','failed'].includes(session.status)){assert.equal(session.status,wanted);return session}await new Promise(r=>setTimeout(r,25))}throw new Error('business deadline')}
try {
  await test('shared stages, two environments, persistent token and dictionary across real restart',async()=>{
    const a=await create('AUTH-001');const b=await create('AUTH-002');
    assert.notEqual(a.home,b.home);assert.notEqual(a.port,b.port);assert.notEqual(a.child.pid,b.child.pid);
    const account=await call(a,'POST','/auth/setup',{username:'fixture-a',password:'synthetic-password-123'},201);a.token=account.secret;
    assert.deepEqual(await call(b,'GET','/auth/status'),{initialized:false});
    await call(a,'POST','/dictionary/entries',{terms:['SeaSnail']});
    const provider=await call(a,'POST','/reasoning/provider-configs',{name:'synthetic-persistence',provider_type:'openai_compatible_self_hosted_private',endpoint:'http://127.0.0.1:65534/v1',model:'synthetic-model'},201);
    await call(a,'PUT',`/internal/reasoning/provider-configs/${provider.id}/credential`,{mode:'credential',credential:'synthetic-provider-key-123'});
    await ready(a);
    const id=await upload(a);const before=await terminal(a,id,'completed');assert.equal(before.transcript.full_text,'海螺支持语音转写和词典功能');
    const pid=a.child.pid;await a.restart();assert.notEqual(a.child.pid,pid);
    assert.deepEqual((await call(a,'GET','/dictionary')).items.map(x=>x.term),['SeaSnail']);
    assert.equal((await call(a,'GET','/reasoning/provider-configs')).find(value=>value.id===provider.id).credential_state,'bound');
    assert.equal((await call(a,'GET',`/sessions/${id}`)).transcript.full_text,before.transcript.full_text);
    await a.restart({keychain:'missing-master-dek'});await call(a,'GET','/dictionary',undefined,423);
    await call(a,'POST',`/accounts/${account.account_id}/unlock`,{password:'synthetic-password-123'});
    assert.equal((await call(a,'GET','/dictionary')).items.length,1);
  });
  await test('late events from an old host cannot change restarted model readiness',async()=>{
    const env=await create('AUTH-001');await env.waitModelReady();const old=env.child;const exitCallbacks=old.listeners('exit');
    await env.restart();await env.waitModelReady();
    for(const callback of exitCallbacks)callback(0,null);old.stdout.emit('data',Buffer.from(JSON.stringify({event:'model-ready',success:false})+'\n'));
    assert.equal(env.processEnded,false);assert.equal(env.modelReady,true);await env.waitModelReady();
  });
  await test('retry attempt uses fresh home and credentials; duplicate attempt rejected',async()=>{
    const first=await create('DICT-002');const retry=await create('DICT-002',{runId:first.manifest.run_id,attempt:1});
    assert.notEqual(first.home,retry.home);assert.notEqual(first.port,retry.port);
    await call(first,'POST','/auth/setup',{username:'first-attempt',password:'synthetic-password-123'},201);
    assert.deepEqual(await call(retry,'GET','/auth/status'),{initialized:false});
    const duplicate=new AttemptEnvironment({root,caseId:'DICT-002',runId:first.manifest.run_id,attempt:1});await assert.rejects(duplicate.prepare(),{code:'EEXIST'});
  });
  await test('failure sequence drives real pipeline and retry recovers',async()=>{
    const env=await create('ASR-002',{scenario:'asr-retry-v1'});const setup=await call(env,'POST','/auth/setup',{username:'fixture-retry',password:'synthetic-password-123'},201);env.token=setup.secret;await ready(env);
    const id=await upload(env);await terminal(env,id,'failed');await call(env,'POST',`/sessions/${id}/retry`,{},202);const result=await terminal(env,id,'completed');assert.equal(result.transcript.full_text,'海螺支持语音转写和词典功能');
  });
  await test('unverified PID record preserves home without signaling unrelated process',async()=>{
    const env=await create('TOKEN-001');const identity=await env.identity(process.pid);
    await fs.mkdir(path.join(env.home,'sidecars'),{recursive:true});
    await fs.writeFile(path.join(env.home,'sidecars','unknown.pid'),JSON.stringify({version:1,owner:{...identity,birth:'wrong'},child:identity,role:'negative'}),{mode:0o600});
    const cleanup=await env.teardown();assert.equal(cleanup.status,'failed');assert.equal(cleanup.home_removed,false);assert.ok(await fs.stat(env.home));
    await fs.unlink(path.join(env.home,'sidecars','unknown.pid'));
  });
  await test('startup failure is cleaned without publishing readiness',async()=>{
    const env=new AttemptEnvironment({root,caseId:'CLEAN-003',scenario:'missing-scenario'});environments.push(env);await env.prepare();
    await assert.rejects(env.start());assert.equal((await env.teardown()).status,'passed');
  });
  await test('no-speech and delayed cancellation use the shared pipeline',async()=>{
    const env=await create('CLEAN-004',{scenario:'asr-no-speech-v1'});const setup=await call(env,'POST','/auth/setup',{username:'fixture-empty',password:'synthetic-password-123'},201);env.token=setup.secret;await ready(env);
    await terminal(env,await upload(env),'failed');await env.restart({scenario:'asr-delay-v1'});await ready(env);await upload(env);
    assert.equal((await env.teardown()).status,'passed');
  });
  await test('nonresponsive host is force-stopped within teardown budget',async()=>{
    const env=await create('TOKEN-002');process.kill(env.child.pid,'SIGSTOP');
    const before=Date.now();const cleanup=await env.teardown();assert.equal(cleanup.status,'passed');assert.ok(Date.now()-before<15000);
  });
  await test('runner SIGKILL leaves a recoverable external manifest',async()=>{
    const worker=spawn(process.execPath,[new URL('./crash_environment_worker.mjs',import.meta.url).pathname,root],{stdio:['ignore','pipe','pipe']});
    let info;const deadline=Date.now()+30000;let buffer='';worker.stdout.on('data',chunk=>{buffer+=chunk;try{info=JSON.parse(buffer.trim())}catch{}});
    try {
      while(!info&&Date.now()<deadline)await new Promise(r=>setTimeout(r,25));assert.ok(info,'worker startup deadline');
      const exited=new Promise(resolve=>worker.once('exit',resolve));worker.kill('SIGKILL');await exited;
      const results=await cleanupStale(root);assert.equal(results.find(r=>r.home===info.home).status,'passed');
      await assert.rejects(fs.stat(info.home),{code:'ENOENT'});
    } finally {worker.kill('SIGKILL')}
  });
  await test('PID birth mismatch does not signal the reused process',async()=>{
    const env=await create('AUTH-003');const unrelated=await env.identity(process.pid);
    await env.stop();env.manifest.hosts.push({...unrelated,birth:'different-start'});await env.save();
    assert.equal((await env.teardown()).status,'passed');assert.ok(await env.identity(process.pid));
  });
  await test('pending host admission preserves recovery data',async()=>{
    const env=new AttemptEnvironment({root,caseId:'CLEAN-001'});environments.push(env);await env.prepare();
    env.manifest.host_admission='pending';await env.save();assert.equal((await env.teardown()).status,'failed');assert.ok(await fs.stat(env.home));
    env.manifest.host_admission='not_spawned';await env.save();
  });
  await test('pending candidate cannot borrow a historical identity with reused PID',async()=>{
    const env=new AttemptEnvironment({root,caseId:'RECOVERY-001'});environments.push(env);await env.prepare();const current=await env.identity(process.pid);
    env.manifest.host_admission='pending';env.manifest.candidate_host_pid=process.pid;env.manifest.hosts=[{...current,birth:'historical-start'}];await env.save();
    assert.equal((await env.teardown()).status,'failed');assert.ok(await fs.stat(env.home));assert.ok(await env.identity(process.pid));
    for(const admission of [undefined,'registered']) {
      env.manifest.host_admission=admission;env.manifest.hosts=[];await env.save();
      assert.equal((await env.teardown()).status,'failed');assert.ok(await env.identity(process.pid));
    }
    env.manifest.host_admission='not_spawned';env.manifest.candidate_host_pid=null;env.manifest.hosts=[];await env.save();
  });
  await test('manifest path mismatch cannot redirect cleanup to another live home',async()=>{
    const a=await create('CLEAN-002');const b=await create('CLEAN-005',{runId:a.manifest.run_id});
    const original=structuredClone(a.manifest);a.manifest.case_id=b.manifest.case_id;a.manifest.runner.birth='stale-runner';await a.save();
    try{const results=await cleanupStale(root);assert.equal(results.find(r=>r.home===a.home).status,'unverified');assert.ok(await fs.stat(b.home));assert.ok(await b.identity(b.child.pid));}
    finally{a.manifest=original;await a.save()}
  });
  await test('host exiting between identity inspection and signaling is confirmed clean',async()=>{
    const env=await create('AUTH-004');const inspect=env.identity.bind(env);const identity=await inspect(env.child.pid);
    let first=true;env.identity=async pid=>{
      if(first&&pid===identity.pid){first=false;env.child.stdin.end();await env.exit;return identity}
      return inspect(pid);
    };
    await env.stopOwned(identity,Date.now()+15000);
    assert.equal(await inspect(identity.pid),null);
    assert.equal((await env.teardown()).status,'passed');
  });
  await test('directory removal failure retains recovery manifest',async()=>{
    const env=await create('DICT-001');const parent=path.dirname(env.home);await fs.chmod(parent,0o500);
    try {const cleanup=await env.teardown();assert.equal(cleanup.status,'failed');assert.ok(await fs.stat(env.recoveryPath))}
    finally {await fs.chmod(parent,0o700)}
  });

} finally {
  let failed=false;
  for(const env of environments){const cleanup=await env.teardown();if(cleanup.status!=='passed'){failed=true;console.error(JSON.stringify(cleanup))}}
  if(!failed)await fs.rm(root,{recursive:true,force:true});
  if(failed)throw new Error('M2 environment cleanup failed');
}
