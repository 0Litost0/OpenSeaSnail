// M2 composition smoke only; no ASR quality gate or acceptance claim.
import {AttemptEnvironment} from '../fixtures/environment.mjs';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {createHash} from 'node:crypto';
const repository=path.resolve(new URL('../../..',import.meta.url).pathname);
const resources=path.join(repository,'dist/SeaSnail.app/Contents/Resources');
process.env.SEASNAIL_ASR_ROOT=path.join(resources,'asr');process.env.FFMPEG_PATH=path.join(resources,'ffmpeg');
const root=await fs.realpath(await fs.mkdtemp(path.join(os.tmpdir(),'seasnail-api-real-m2-')));
const env=new AttemptEnvironment({root,caseId:'ASR-001',runtime:'sherpa'});
const observation={scope:'M2 shared production composition smoke; quality not evaluated',runtime:'sherpa',quality_gate:'incomplete',budget_status:'proposed'};
let failure;
try{
  await env.prepare();let began=Date.now();await env.start();observation.startup_ms=Date.now()-began;
  const modelBegan=Date.now();await env.waitModelReady();observation.model_ready_ms=Date.now()-modelBegan;
  const setup=await fetch(env.baseURL+'/auth/setup',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({username:'real-composition-fixture',password:'synthetic-password-123'}),signal:AbortSignal.timeout(5000)});
  if(setup.status!==201)throw new Error('setup failed');const token=(await setup.json()).secret;
  const deadline=Date.now()+90000;began=Date.now();let active;
  while(Date.now()<deadline){const response=await fetch(env.baseURL+'/models',{headers:{Authorization:`Bearer ${token}`},signal:AbortSignal.timeout(5000)});if(!response.ok)throw new Error('models request failed');active=(await response.json()).find(m=>m.id==='sensevoice-small-sherpa-int8'&&m.status==='active');if(active)break;await new Promise(r=>setTimeout(r,100))}
  if(!active || active.runtime!=='sherpa_onnx')throw new Error('real model did not become ready; no fallback');
  observation.model_id=active.id;
  const entries=(await fs.readdir(path.join(env.home,'sidecars'))).filter(v=>v.endsWith('.pid'));
  if(entries.length!==1)throw new Error('expected one real owned sidecar');
  const record=JSON.parse(await fs.readFile(path.join(env.home,'sidecars',entries[0]),'utf8'));
  const actual=await env.identity(record.child.pid);if(JSON.stringify(actual)!==JSON.stringify(record.child))throw new Error('real sidecar identity mismatch');
  observation.child_identity_verified=true;
  const artifact=await fs.readFile(path.join(process.env.SEASNAIL_ASR_ROOT,'sensevoice-small/sherpa_onnx/int8/artifact-manifest.json'));
  observation.artifact_manifest_sha256=createHash('sha256').update(artifact).digest('hex');
  observation.execution='passed';
}catch(error){failure=error;observation.execution='environment_error';observation.error=error.message}
finally{
  const began=Date.now();observation.teardown=await env.teardown();observation.teardown_ms=Date.now()-began;
  if(observation.teardown.status==='passed')await fs.rm(root,{recursive:true});
  console.log(JSON.stringify(observation,null,2));
  if(failure||observation.teardown.status!=='passed')process.exitCode=1;
}
